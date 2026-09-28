use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::time::Instant;

use crate::config::{Config, SessionState};
use crate::download::worker::WorkerHandle;
use crate::http::HttpGet;
use crate::library::db::{Db, DbError, LyricsRow};
use crate::library::reconcile::{ReconcileReport, reconcile};
use crate::lyrics::{self, LyricsOutcome};
use crate::player::engine::{Engine, EngineError, TrackResolver};
use crate::player::mpv::{MpvEvent, MpvSpawner};
use crate::protocol::{
    Command, ContextRef, ErrorCode, Event, PROTOCOL, PlayState, PlayerState, Response,
};
use crate::types::{Album, LoopMode, Track, TrackState};
use crate::updater::{UpdateOutcome, Updater};

pub const EVENT_CAPACITY: usize = 1024;
/// Cached lyrics misses are retried after this long.
const LYRICS_MISS_TTL: i64 = 7 * 24 * 3600;
const UPDATE_RECHECK: Duration = Duration::from_secs(24 * 3600);

/// One socket request; the core answers on `reply`.
pub struct Request {
    pub id: u64,
    pub cmd: Command,
    pub reply: oneshot::Sender<Reply>,
}

/// `events` is set for `subscribe`: the connection forwards it.
pub struct Reply {
    pub response: Response,
    pub events: Option<broadcast::Receiver<Event>>,
}

pub struct CoreDeps<S> {
    pub config: Config,
    pub db: Db,
    pub spawner: S,
    pub worker: WorkerHandle,
    pub http: Arc<dyn HttpGet>,
    pub updater: Option<Arc<Updater>>,
}

/// Receivers the core loop selects over.
pub struct Inbox {
    requests: mpsc::UnboundedReceiver<Request>,
    mpv: mpsc::UnboundedReceiver<(u64, MpvEvent)>,
    internal: mpsc::UnboundedReceiver<Internal>,
}

/// Results of blocking work run off the core loop.
enum Internal {
    Reconciled(Result<ReconcileReport, String>),
    Lyrics {
        track_id: String,
        outcome: LyricsOutcome,
    },
    Updated(UpdateOutcome),
}

#[derive(Clone)]
struct CmdError(ErrorCode, String);

impl From<DbError> for CmdError {
    fn from(e: DbError) -> Self {
        match e {
            DbError::NotFound => CmdError(ErrorCode::NotFound, "not found".into()),
            DbError::Conflict => CmdError(ErrorCode::Conflict, "name already exists".into()),
            DbError::BadRequest(msg) => CmdError(ErrorCode::BadRequest, msg),
            e => CmdError(ErrorCode::Internal, e.to_string()),
        }
    }
}

impl From<EngineError> for CmdError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::Unplayable(_) => CmdError(ErrorCode::BadRequest, e.to_string()),
            EngineError::Mpv(_) => CmdError(ErrorCode::PlayerUnavailable, e.to_string()),
        }
    }
}

fn not_found(what: &str) -> CmdError {
    CmdError(ErrorCode::NotFound, format!("no such {what}"))
}

fn respond(id: u64, result: Result<Value, CmdError>) -> Response {
    match result {
        Ok(v) => Response::ok(id, v),
        Err(CmdError(code, msg)) => Response::err(id, code, msg),
    }
}

/// Complete tracks whose audio file exists play from that path.
struct Resolver<'a>(&'a Db);

impl TrackResolver for Resolver<'_> {
    fn playable_path(&self, track_id: &str) -> Option<String> {
        let track = self.0.get_track(track_id).ok()??;
        let path = track.audio_path?;
        (track.state == TrackState::Complete && Path::new(&path).exists()).then_some(path)
    }
}

enum LyricsLookup {
    Cached(Value),
    Fetch(Box<Track>),
}

/// The service actor: owns the DB, the engine and the worker handle; every
/// mutation happens on its loop.
pub struct Core<S: MpvSpawner> {
    config: Config,
    db: Db,
    engine: Engine<S>,
    worker: WorkerHandle,
    http: Arc<dyn HttpGet>,
    updater: Option<Arc<Updater>>,
    events: broadcast::Sender<Event>,
    library_version: u64,
    session: SessionState,
    requests: mpsc::UnboundedSender<Request>,
    internal: mpsc::UnboundedSender<Internal>,
    lyrics_waiters: HashMap<String, Vec<(u64, oneshot::Sender<Reply>)>>,
    reconciling: bool,
    update_at: Option<Instant>,
}

impl<S: MpvSpawner> Core<S> {
    pub fn new(deps: CoreDeps<S>) -> (Core<S>, Inbox) {
        Self::with_capacity(deps, EVENT_CAPACITY)
    }

    /// `new` with a custom event buffer (tests use a tiny one).
    pub fn with_capacity(deps: CoreDeps<S>, capacity: usize) -> (Core<S>, Inbox) {
        let (requests, requests_rx) = mpsc::unbounded_channel();
        let (mpv_tx, mpv_rx) = mpsc::unbounded_channel();
        let (internal, internal_rx) = mpsc::unbounded_channel();
        let session = SessionState::load(&deps.config.state_path());
        let volume = session
            .volume
            .or(deps.config.volume.map(|v| v * 100.0))
            .unwrap_or(100.0)
            .clamp(0.0, 100.0);
        let loop_mode = session.loop_mode.unwrap_or(LoopMode::None);
        let core = Core {
            engine: Engine::new(deps.spawner, mpv_tx, volume, loop_mode),
            config: deps.config,
            db: deps.db,
            worker: deps.worker,
            http: deps.http,
            updater: deps.updater,
            events: broadcast::channel(capacity).0,
            library_version: 0,
            session,
            requests,
            internal,
            lyrics_waiters: HashMap::new(),
            reconciling: false,
            update_at: None,
        };
        let inbox = Inbox {
            requests: requests_rx,
            mpv: mpv_rx,
            internal: internal_rx,
        };
        (core, inbox)
    }

    pub fn requests(&self) -> mpsc::UnboundedSender<Request> {
        self.requests.clone()
    }

    /// Background reconcile and the first update check.
    pub fn start(&mut self) {
        self.spawn_reconcile();
        self.check_update();
    }

    /// Serve until `stop` resolves. No periodic timers: only channel receives
    /// and the one-shot engine idle and update deadlines.
    pub async fn run(&mut self, mut inbox: Inbox, stop: impl Future<Output = ()>) {
        tokio::pin!(stop);
        loop {
            tokio::select! {
                () = &mut stop => return,
                Some(req) = inbox.requests.recv() => self.on_request(req).await,
                Some((generation, ev)) = inbox.mpv.recv() => self.on_mpv_event(generation, ev).await,
                Some(msg) = inbox.internal.recv() => self.on_internal(msg),
                () = sleep_until_opt(self.engine.idle_deadline()) => self.engine.on_idle_deadline().await,
                () = sleep_until_opt(self.update_at) => self.check_update(),
            }
        }
    }

    /// Save the session and quit mpv.
    pub async fn shutdown(&mut self) {
        let state = self.engine.state();
        self.save_session(&state);
        let _ = self.engine.stop().await;
        self.engine.on_idle_deadline().await;
    }

    pub async fn on_request(&mut self, req: Request) {
        let Request { id, cmd, reply } = req;
        let prev = self.engine.state();
        let mut events = None;
        let r = Resolver(&self.db);
        let result = match cmd {
            Command::Hello { protocol } if protocol != PROTOCOL => Err(CmdError(
                ErrorCode::ProtocolMismatch,
                format!("server speaks protocol {PROTOCOL}, client {protocol}"),
            )),
            Command::Hello { .. } => Ok(json!({
                "protocol": PROTOCOL,
                "server_version": env!("CARGO_PKG_VERSION"),
            })),
            Command::Subscribe => {
                events = Some(self.events.subscribe());
                Ok(json!({"player": prev, "library_version": self.library_version}))
            }
            Command::LibraryGet => self.library_get(),
            Command::Now => self.now(),
            Command::Play { track_id, context } => self.play(track_id, context).await,
            Command::Pause => ok(self.engine.pause().await),
            Command::Resume => ok(self.engine.resume().await),
            Command::Toggle => ok(self.engine.toggle().await),
            Command::Stop => ok(self.engine.stop().await),
            Command::Next => ok(self.engine.next(&r).await),
            Command::Prev => ok(self.engine.prev(&r).await),
            Command::Seek { position } => ok(self.engine.seek(position).await),
            Command::Volume { value } => ok(self.engine.set_volume(value).await),
            Command::Loop { mode } => ok(self.engine.set_loop(mode, &r).await),
            Command::QueuePlayNext { track_id } => self.play_next(&track_id).await,
            Command::AlbumCreate { name } => self.album_create(&name),
            Command::AlbumRename { album_id, name } => self.album_rename(album_id, &name),
            Command::AlbumDelete { album_id } => self.album_delete(album_id).await,
            Command::AlbumAdd { album_id, track_id } => self.album_add(album_id, &track_id).await,
            Command::AlbumRemove { album_id, track_id } => {
                self.album_remove(album_id, &track_id).await
            }
            Command::AlbumReorder {
                album_id,
                track_ids,
            } => self.album_reorder(album_id, &track_ids).await,
            Command::TrackRename { track_id, title } => self.track_rename(&track_id, &title),
            Command::Rescan => {
                self.spawn_reconcile();
                Ok(json!({}))
            }
            Command::Lyrics { track_id } => match self.lyrics_lookup(&track_id) {
                Ok(LyricsLookup::Cached(v)) => Ok(v),
                Ok(LyricsLookup::Fetch(track)) => return self.fetch_lyrics(id, *track, reply),
                Err(e) => Err(e),
            },
            Command::Add { .. } | Command::TrackDelete { .. } | Command::TrackRetry { .. } => {
                Err(CmdError(ErrorCode::Internal, "not implemented".into()))
            }
        };
        self.player_changed(&prev, false);
        let _ = reply.send(Reply {
            response: respond(id, result),
            events,
        });
    }

    async fn on_mpv_event(&mut self, generation: u64, ev: MpvEvent) {
        let prev = self.engine.state();
        if self
            .engine
            .on_mpv_event(generation, ev, &Resolver(&self.db))
            .await
        {
            self.player_changed(&prev, true);
        }
    }

    fn on_internal(&mut self, msg: Internal) {
        match msg {
            Internal::Reconciled(result) => self.on_reconciled(result),
            Internal::Lyrics { track_id, outcome } => self.on_lyrics(track_id, outcome),
            Internal::Updated(outcome) => {
                eprintln!("yt-dlp update check: {outcome:?}");
                if let UpdateOutcome::Upgraded { .. } = outcome {
                    self.worker.restart_when_idle();
                }
            }
        }
    }

    fn emit(&self, ev: Event) {
        let _ = self.events.send(ev);
    }

    /// Emit `player` when forced or the state differs from `prev`; save the
    /// session on a track change, pause or stop.
    fn player_changed(&mut self, prev: &PlayerState, force: bool) {
        let state = self.engine.state();
        if force || state != *prev {
            self.emit(Event::Player(state.clone()));
        }
        let halted = state.state != prev.state && state.state != PlayState::Playing;
        if state.track_id != prev.track_id || halted {
            self.save_session(&state);
        }
    }

    fn save_session(&mut self, state: &PlayerState) {
        self.session.volume = Some(state.volume);
        self.session.loop_mode = Some(state.loop_mode);
        if state.track_id.is_some() {
            self.session.last_track_id = state.track_id.clone();
            self.session.last_position = Some(state.position);
            self.session.last_context = state
                .context
                .as_ref()
                .and_then(|c| serde_json::to_value(c).ok());
        }
        self.session.save(&self.config.state_path());
    }

    fn library_get(&self) -> Result<Value, CmdError> {
        Ok(json!({
            "tracks": self.db.list_tracks()?,
            "albums": self.db.list_albums()?,
            "library_version": self.library_version,
        }))
    }

    fn now(&self) -> Result<Value, CmdError> {
        let player = self.engine.state();
        let track = match &player.track_id {
            Some(id) => self.db.get_track(id)?,
            None => None,
        };
        Ok(json!({"player": player, "track": track}))
    }

    fn require_track(&self, track_id: &str) -> Result<Track, CmdError> {
        self.db
            .get_track(track_id)?
            .ok_or_else(|| not_found("track"))
    }

    fn require_album(&self, album_id: i64) -> Result<Album, CmdError> {
        self.db
            .get_album(album_id)?
            .ok_or_else(|| not_found("album"))
    }

    async fn play(&mut self, track_id: String, context: ContextRef) -> Result<Value, CmdError> {
        self.require_track(&track_id)?;
        let order = match &context {
            ContextRef::Album(album_id) => self.require_album(*album_id)?.track_ids,
            ContextRef::Library => self.db.library_order()?,
        };
        self.engine
            .play(context.clone(), order, &track_id, &Resolver(&self.db))
            .await?;
        self.db.touch_last_played(&track_id)?;
        if let ContextRef::Album(album_id) = context {
            self.db.touch_album(album_id)?;
        }
        Ok(json!({}))
    }

    async fn play_next(&mut self, track_id: &str) -> Result<Value, CmdError> {
        self.require_track(track_id)?;
        ok(self.engine.play_next(track_id, &Resolver(&self.db)).await)
    }

    /// Emit `album.upsert` for `album_id` and bump the library version.
    fn album_changed(&mut self, album_id: i64) -> Result<(), CmdError> {
        if let Some(album) = self.db.get_album(album_id)? {
            self.emit(Event::AlbumUpsert { album });
        }
        self.library_version += 1;
        Ok(())
    }

    /// If `album_id` is the playing context, hand the engine its new order.
    async fn sync_context(&mut self, album_id: i64) -> Result<(), CmdError> {
        if self.engine.state().context != Some(ContextRef::Album(album_id)) {
            return Ok(());
        }
        let order = self
            .db
            .get_album(album_id)?
            .map(|a| a.track_ids)
            .unwrap_or_default();
        let _ = self.engine.replace_order(order, &Resolver(&self.db)).await;
        Ok(())
    }

    fn album_create(&mut self, name: &str) -> Result<Value, CmdError> {
        let album_id = self.db.create_album(name)?;
        let album = self.require_album(album_id)?;
        self.emit(Event::AlbumUpsert {
            album: album.clone(),
        });
        self.library_version += 1;
        Ok(json!({ "album": album }))
    }

    fn album_rename(&mut self, album_id: i64, name: &str) -> Result<Value, CmdError> {
        self.db.rename_album(album_id, name)?;
        self.album_changed(album_id)?;
        Ok(json!({}))
    }

    async fn album_delete(&mut self, album_id: i64) -> Result<Value, CmdError> {
        self.db.delete_album(album_id)?;
        self.emit(Event::AlbumRemoved { album_id });
        self.library_version += 1;
        self.sync_context(album_id).await?;
        Ok(json!({}))
    }

    async fn album_add(&mut self, album_id: i64, track_id: &str) -> Result<Value, CmdError> {
        if self.db.album_add(album_id, track_id)? {
            self.album_changed(album_id)?;
            self.sync_context(album_id).await?;
        }
        Ok(json!({}))
    }

    async fn album_remove(&mut self, album_id: i64, track_id: &str) -> Result<Value, CmdError> {
        self.require_album(album_id)?;
        if self.db.album_remove(album_id, track_id)? {
            self.album_changed(album_id)?;
            self.sync_context(album_id).await?;
        }
        Ok(json!({}))
    }

    async fn album_reorder(
        &mut self,
        album_id: i64,
        track_ids: &[String],
    ) -> Result<Value, CmdError> {
        self.require_album(album_id)?;
        self.db.album_reorder(album_id, track_ids)?;
        self.album_changed(album_id)?;
        self.sync_context(album_id).await?;
        Ok(json!({}))
    }

    fn track_rename(&mut self, track_id: &str, title: &str) -> Result<Value, CmdError> {
        self.db.rename_track(track_id, title)?;
        let track = self.require_track(track_id)?;
        self.emit(Event::TrackUpsert { track });
        self.library_version += 1;
        Ok(json!({}))
    }

    fn spawn_reconcile(&mut self) {
        if self.reconciling {
            return;
        }
        self.reconciling = true;
        let db_path = self.config.db_path();
        let cache_dir = self.config.cache_dir.clone();
        let tx = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            let result = Db::open(&db_path, &cache_dir)
                .map_err(anyhow::Error::from)
                .and_then(|db| reconcile(&cache_dir, &db, &HashSet::new()))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Internal::Reconciled(result));
        });
    }

    fn on_reconciled(&mut self, result: Result<ReconcileReport, String>) {
        self.reconciling = false;
        let report = match result {
            Ok(report) => report,
            Err(e) => {
                eprintln!("reconcile failed: {e}");
                return;
            }
        };
        for id in &report.imported {
            if let Ok(Some(track)) = self.db.get_track(id) {
                self.emit(Event::TrackUpsert { track });
            }
        }
        for track_id in &report.removed {
            self.emit(Event::TrackRemoved {
                track_id: track_id.clone(),
            });
        }
        if !report.imported.is_empty() || !report.removed.is_empty() {
            self.library_version += 1;
        }
    }

    /// Run the updater on a blocking thread when it is due and no download
    /// is in flight; re-check in 24 h.
    fn check_update(&mut self) {
        let Some(updater) = self.updater.clone() else {
            return;
        };
        self.update_at = Some(Instant::now() + UPDATE_RECHECK);
        if self.worker.busy() || !updater.due(SystemTime::now()) {
            return;
        }
        let tx = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Internal::Updated(updater.check_and_upgrade()));
        });
    }

    fn lyrics_lookup(&self, track_id: &str) -> Result<LyricsLookup, CmdError> {
        let track = self.require_track(track_id)?;
        Ok(match self.db.get_lyrics(track_id)? {
            Some(LyricsRow {
                synced,
                body: Some(body),
                ..
            }) => LyricsLookup::Cached(lyrics_value(synced, &body)),
            Some(LyricsRow {
                body: None,
                fetched_at,
                ..
            }) if now_secs() - fetched_at < LYRICS_MISS_TTL => {
                LyricsLookup::Cached(json!({"missing": true}))
            }
            _ => LyricsLookup::Fetch(Box::new(track)),
        })
    }

    /// Queue the reply; concurrent requests for one track share one fetch.
    fn fetch_lyrics(&mut self, id: u64, track: Track, reply: oneshot::Sender<Reply>) {
        let waiters = self.lyrics_waiters.entry(track.id.clone()).or_default();
        waiters.push((id, reply));
        if waiters.len() > 1 {
            return;
        }
        let http = self.http.clone();
        let tx = self.internal.clone();
        tokio::task::spawn_blocking(move || {
            let outcome = lyrics::fetch(
                http.as_ref(),
                &track.title,
                track.uploader.as_deref(),
                track.duration,
            );
            let _ = tx.send(Internal::Lyrics {
                track_id: track.id,
                outcome,
            });
        });
    }

    /// Cache Found/Missing (never Error) and answer every waiter.
    fn on_lyrics(&mut self, track_id: String, outcome: LyricsOutcome) {
        let result = match outcome {
            LyricsOutcome::Found { synced, body } => {
                let _ = self.db.put_lyrics(&track_id, synced, Some(&body));
                Ok(lyrics_value(synced, &body))
            }
            LyricsOutcome::Missing => {
                let _ = self.db.put_lyrics(&track_id, false, None);
                Ok(json!({"missing": true}))
            }
            LyricsOutcome::Error(e) => Err(CmdError(ErrorCode::Internal, e)),
        };
        for (id, reply) in self.lyrics_waiters.remove(&track_id).unwrap_or_default() {
            let _ = reply.send(Reply {
                response: respond(id, result.clone()),
                events: None,
            });
        }
    }
}

fn ok(result: Result<(), EngineError>) -> Result<Value, CmdError> {
    result?;
    Ok(json!({}))
}

fn lyrics_value(synced: bool, body: &str) -> Value {
    let lines: Vec<Value> = if synced {
        lyrics::parse_lrc(body)
            .into_iter()
            .map(|(t, text)| json!({"t_ms": (t * 1000.0).round() as i64, "text": text}))
            .collect()
    } else {
        body.lines()
            .map(|text| json!({"t_ms": null, "text": text}))
            .collect()
    };
    json!({"synced": synced, "lines": lines})
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}
