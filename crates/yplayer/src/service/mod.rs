pub mod conn;
pub mod core;
pub mod downloads;

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use self::core::{Core, CoreDeps, Inbox, Request};
use crate::config::{self, Config, FileConfig, PendingMove, SettingEdit};
use crate::download::bridge::worker_command;
use crate::download::worker::{WorkerHandle, WorkerOptions};
use crate::http::{CurlHttp, HttpGet};
use crate::library::db::Db;
use crate::library::reconcile::recover_interrupted;
use crate::loudness::Ffmpeg;
use crate::lyrics::USER_AGENT;
use crate::player::mpv::{MpvOptions, MpvSpawner, ProcessSpawner};
use crate::protocol::Severity;
use crate::updater::{SystemRunner, Updater};

const WORKER_IDLE: Duration = Duration::from_secs(60);
const WORKER_INACTIVITY: Duration = Duration::from_secs(180);

pub struct ServeOptions {
    pub config: Config,
    pub socket_path: PathBuf,
    pub state_dir: PathBuf,
}

/// `yplay serve`: run until SIGTERM/SIGINT.
pub async fn serve(opts: ServeOptions) -> Result<()> {
    let ServeOptions {
        mut config,
        socket_path,
        state_dir,
    } = opts;
    owner_only_umask();
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("creating {}", state_dir.display()))?;
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
    // A second instance must exit before it touches (or moves aside) the live DB.
    if UnixStream::connect(&socket_path).await.is_ok() {
        bail!("yplay serve is already running ({})", socket_path.display());
    }
    config.settings_dir = Some(state_dir.clone());
    if let Some(file) = config.config_file()
        && let Some(on) = FileConfig::load_from(&file).level_loudness
    {
        config.level_loudness = on;
    }
    let moved = take_pending_move(&mut config);
    let (db, rebuilt) = open_db(&config)?;
    let move_toast = moved.as_ref().map(|outcome| finish_move(&db, outcome));
    owner_only_files(&config);
    let listener = bind_socket(&socket_path).await?;
    // An mpv left playing by a killed previous instance stops now, not at the
    // next play.
    crate::player::mpv::quit_orphan(&socket_path.with_extension("mpv.sock")).await;
    recover_interrupted(&config.cache_dir, &db)?;

    let spawner = ProcessSpawner {
        opts_template: MpvOptions {
            socket_path: socket_path.with_extension("mpv.sock"),
            ao: None,
            volume: 100.0,
            extra_args: mpv_extra_args(),
        },
    };
    let worker = WorkerHandle::spawn(WorkerOptions {
        worker_python: config.worker_python.clone(),
        worker_cmd: None,
        log_path: Config::worker_log_path(&state_dir),
        idle_timeout: WORKER_IDLE,
        inactivity_timeout: WORKER_INACTIVITY,
    });
    let http: Arc<dyn HttpGet> = Arc::new(CurlHttp {
        user_agent: USER_AGENT.to_string(),
    });
    // YPLAY_NO_UPDATE disables the yt-dlp update check (tests, offline use).
    let updater = worker_command(config.worker_python.as_deref())
        .ok()
        .filter(|_| std::env::var_os("YPLAY_NO_UPDATE").is_none())
        .map(|(python, _)| {
            Arc::new(Updater {
                python,
                uv: which::which("uv")
                    .ok()
                    .map(|p| p.to_string_lossy().into_owned()),
                stamp: Config::update_stamp_path(&state_dir),
                http: http.clone(),
                runner: Arc::new(SystemRunner),
            })
        });
    let (mut core, inbox) = Core::new(CoreDeps {
        config,
        db,
        spawner,
        worker,
        http,
        updater,
    });
    if rebuilt {
        core.warn_after_startup(DB_REBUILT);
    }
    if let Some((severity, message)) = move_toast {
        core.toast_after_startup(severity, &message);
    }
    match Ffmpeg::find() {
        Some(ffmpeg) => core.set_measurer(Arc::new(ffmpeg)),
        None => eprintln!("ffmpeg not found: loudness leveling is unavailable"),
    }
    core.start();

    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let stop = async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    };
    run(core, inbox, listener, socket_path, stop).await;
    Ok(())
}

const DB_REBUILT: &str = "Your library database was damaged and has been rebuilt from your music folder; albums could not be recovered.";

/// What became of a `library.move` request at startup.
#[derive(Debug, PartialEq)]
enum MoveOutcome {
    Moved { from: PathBuf, to: PathBuf },
    Failed(String),
}

/// Apply a pending `library.move` before the DB opens. The request file is
/// deleted first, so a move is never retried; the folder is renamed only
/// when `from` is still the music folder, exists, and `to` does not. On
/// success the music folder becomes `to`, in `config` and in `config.toml`.
fn take_pending_move(config: &mut Config) -> Option<MoveOutcome> {
    let file = config.pending_move_file()?;
    let text = match std::fs::read_to_string(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        text => text,
    };
    if let Err(e) = std::fs::remove_file(&file) {
        return Some(MoveOutcome::Failed(format!(
            "couldn't remove {}: {e}",
            file.display()
        )));
    }
    let request = match text
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<PendingMove>(&t).map_err(|e| e.to_string()))
    {
        Ok(request) => request,
        Err(e) => return Some(MoveOutcome::Failed(e)),
    };
    let PendingMove { from, to } = request;
    let failed = |reason: String| Some(MoveOutcome::Failed(reason));
    if from != config.cache_dir {
        return failed(format!(
            "your music folder is now {}",
            config.cache_dir.display()
        ));
    }
    if !from.is_dir() {
        return failed(format!("{} is missing", from.display()));
    }
    if to.symlink_metadata().is_ok() {
        return failed(format!("{} already exists", to.display()));
    }
    if let Err(e) = std::fs::rename(&from, &to) {
        return failed(e.to_string());
    }
    if let Some(file) = config.config_file()
        && let Err(e) = config::save_settings(&file, &[SettingEdit::CacheDir(&to)])
    {
        // Without the new path in config.toml the next start would use an
        // empty `from`: put the folder back.
        let _ = std::fs::rename(&to, &from);
        return failed(format!("couldn't save the new location: {e}"));
    }
    config.cache_dir = to.clone();
    Some(MoveOutcome::Moved { from, to })
}

/// After the DB opened: rewrite the moved folder's paths; the startup toast.
fn finish_move(db: &Db, outcome: &MoveOutcome) -> (Severity, String) {
    match outcome {
        MoveOutcome::Moved { from, to } => {
            if let Err(e) = db.move_paths(from, to) {
                eprintln!("could not rewrite the moved paths: {e}");
            }
            (
                Severity::Info,
                format!("Moved your music folder to {}.", to.display()),
            )
        }
        MoveOutcome::Failed(e) => (
            Severity::Warn,
            format!("Couldn't move your music folder: {e}."),
        ),
    }
}

/// Check a `library.move` target for the music folder `cache_dir`; `device`
/// gives a path's device (tests inject it). Returns the new location, or the
/// `bad_request` message.
fn check_move(
    cache_dir: &Path,
    to: &str,
    device: impl Fn(&Path) -> std::io::Result<u64>,
) -> Result<PathBuf, &'static str> {
    const EXISTS: &str = "Something already exists at that location.";
    let to = Path::new(to);
    if !to.is_absolute() {
        return Err("The new location must be a full path.");
    }
    let (Some(parent), Some(name)) = (to.parent(), to.file_name()) else {
        return Err(EXISTS);
    };
    let real_parent = match parent.canonicalize() {
        Ok(p) if p.is_dir() => p,
        _ => return Err("That folder's parent doesn't exist."),
    };
    let target = parent.join(name);
    if target.symlink_metadata().is_ok() {
        return Err(EXISTS);
    }
    let real_cache = cache_dir
        .canonicalize()
        .unwrap_or_else(|_| cache_dir.to_path_buf());
    if real_parent.join(name).starts_with(&real_cache) {
        return Err("The new location can't be inside your music folder.");
    }
    match (device(&real_parent), device(&real_cache)) {
        (Ok(a), Ok(b)) if a == b => Ok(target),
        _ => Err("Choose a folder on the same disk as your music folder."),
    }
}

/// The device a path lives on.
fn device_of(path: &Path) -> std::io::Result<u64> {
    std::fs::metadata(path).map(|m| m.dev())
}

/// Files the service creates (DB, session file, logs, mpv socket) are
/// owner-only.
fn owner_only_umask() {
    #[cfg(target_os = "macos")]
    type Mode = u16;
    #[cfg(not(target_os = "macos"))]
    type Mode = u32;
    unsafe extern "C" {
        fn umask(mask: Mode) -> Mode;
    }
    // SAFETY: umask only swaps the process's file-mode creation mask.
    unsafe {
        umask(0o077);
    }
}

/// Open the library DB. One that is not a valid database is moved (with its
/// `-wal`/`-shm`) to `.yplayer.db.corrupt-<unix time>` and replaced by a
/// fresh one, which reconcile fills from the folders; `true` says so.
fn open_db(config: &Config) -> Result<(Db, bool)> {
    let path = config.db_path();
    match Db::open(&path, &config.cache_dir) {
        Err(e) if e.is_corrupt() => {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let aside = format!("{}.corrupt-{secs}", path.display());
            eprintln!("{}: {e}; moving it to {aside}", path.display());
            std::fs::rename(&path, &aside).with_context(|| format!("moving {}", path.display()))?;
            for suffix in ["-wal", "-shm"] {
                let side = format!("{}{suffix}", path.display());
                if Path::new(&side).exists() {
                    std::fs::rename(&side, format!("{aside}{suffix}"))
                        .with_context(|| format!("moving {side}"))?;
                }
            }
            Ok((Db::open(&path, &config.cache_dir)?, true))
        }
        other => Ok((other?, false)),
    }
}

/// Make the library DB (and its `-wal`/`-shm`) and the session file owner-only;
/// files created before the umask existed keep their old mode otherwise.
fn owner_only_files(config: &Config) {
    let db = config.db_path();
    let candidates = [
        db.clone(),
        PathBuf::from(format!("{}-wal", db.display())),
        PathBuf::from(format!("{}-shm", db.display())),
        config.state_path(),
    ];
    for path in candidates {
        if path.exists()
            && let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        {
            eprintln!("could not make {} owner-only: {e}", path.display());
        }
    }
}

/// `YPLAY_MPV_EXTRA_ARGS`, whitespace-split, appended to mpv's arguments.
fn mpv_extra_args() -> Vec<String> {
    std::env::var("YPLAY_MPV_EXTRA_ARGS")
        .map(|s| s.split_whitespace().map(String::from).collect())
        .unwrap_or_default()
}

/// Bind the service socket (0600). A live socket means another instance is
/// running; a dead one is a stale file and is replaced.
pub async fn bind_socket(path: &Path) -> Result<UnixListener> {
    if UnixStream::connect(path).await.is_ok() {
        bail!("yplay serve is already running ({})", path.display());
    }
    let _ = std::fs::remove_file(path);
    let listener =
        UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Accept connections and run the core until `stop`, then shut down.
async fn run<S: MpvSpawner>(
    mut core: Core<S>,
    inbox: Inbox,
    listener: UnixListener,
    socket_path: PathBuf,
    stop: impl Future<Output = ()>,
) {
    let accept = tokio::spawn(accept_loop(listener, core.requests()));
    core.run(inbox, stop).await;
    accept.abort();
    shutdown(&mut core, &socket_path).await;
}

/// Save the session, quit mpv and remove the socket.
pub async fn shutdown<S: MpvSpawner>(core: &mut Core<S>, socket_path: &Path) {
    core.shutdown().await;
    let _ = std::fs::remove_file(socket_path);
}

async fn accept_loop(listener: UnixListener, requests: mpsc::UnboundedSender<Request>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(conn::handle(stream, requests.clone()));
            }
            Err(e) => eprintln!("accept failed: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::core::EVENT_CAPACITY;
    use super::*;
    use crate::config::SessionState;
    use crate::loudness::{Measure, MeasureFuture, Measurement};
    use crate::player::engine::testing::FakeSpawner;
    use crate::player::mpv::MpvEvent;
    use crate::protocol::{Command, ContextRef, MAX_LINE};
    use crate::types::{LoopMode, Track, TrackState};
    use serde_json::{Value, json};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
    use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
    use tokio::sync::oneshot;
    use tokio::task::{JoinHandle, LocalSet};

    const A: &str = "aaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbb";
    const WAIT: Duration = Duration::from_secs(10);

    struct Svc {
        dir: tempfile::TempDir,
        sock: PathBuf,
        fake: FakeSpawner,
        stop: Option<oneshot::Sender<()>>,
        task: Option<JoinHandle<()>>,
    }

    impl Svc {
        fn cache(&self) -> PathBuf {
            self.dir.path().join("cache")
        }

        async fn stop(&mut self) {
            let _ = self.stop.take().unwrap().send(());
            self.task.take().unwrap().await.unwrap();
        }

        /// Worker processes started (the fake appends its pid to `spawns`).
        fn spawns(&self) -> usize {
            lines(&self.dir.path().join("spawns")).len()
        }

        /// Let the fake worker finish a `hold…` download.
        fn release(&self, vid: &str) {
            std::fs::write(self.dir.path().join(format!("release-{vid}")), b"").unwrap();
        }

        fn track_dir(&self, vid: &str) -> PathBuf {
            self.cache().join(format!("Title {vid} [{}]", &vid[..8]))
        }
    }

    impl Drop for Svc {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.sock);
        }
    }

    fn lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    }

    /// Cache `<dir>/cache`; `config.toml` and `pending-move.json` in `dir`.
    fn config(dir: &Path) -> Config {
        let mut config = Config::new(Some(dir.join("cache").to_string_lossy().into_owned()), None);
        config.settings_dir = Some(dir.to_path_buf());
        config
    }

    /// Two complete tracks with (empty) audio files: A added after B.
    fn seed(config: &Config) -> Db {
        std::fs::create_dir_all(&config.cache_dir).unwrap();
        let db = Db::open(&config.db_path(), &config.cache_dir).unwrap();
        for (id, added_at) in [(A, 2), (B, 1)] {
            let dir = config.cache_dir.join(format!("Song {id} [{}]", &id[..8]));
            std::fs::create_dir_all(&dir).unwrap();
            let audio = dir.join("audio.opus");
            std::fs::write(&audio, b"").unwrap();
            db.upsert_track(&Track {
                id: id.into(),
                title: format!("Song {id}"),
                uploader: Some("Up".into()),
                duration: Some(4),
                webpage_url: None,
                audio_path: Some(audio.to_string_lossy().into_owned()),
                format: Some("opus".into()),
                file_size: Some(0),
                added_at: Some(added_at),
                last_played: None,
                state: TrackState::Complete,
                thumb_path: None,
            })
            .unwrap();
        }
        db
    }

    fn new_core(
        dir: &Path,
        fake: &FakeSpawner,
        capacity: usize,
        worker_cmd: Option<Vec<String>>,
    ) -> (Core<FakeSpawner>, Inbox) {
        let config = config(dir);
        let db = seed(&config);
        let worker = WorkerHandle::spawn(WorkerOptions {
            worker_python: None,
            worker_cmd,
            log_path: dir.join("worker.log"),
            idle_timeout: WORKER_IDLE,
            inactivity_timeout: WORKER_INACTIVITY,
        });
        let http: Arc<dyn HttpGet> = Arc::new(CurlHttp {
            user_agent: USER_AGENT.to_string(),
        });
        Core::with_capacity(
            CoreDeps {
                config,
                db,
                spawner: fake.clone(),
                worker,
                http,
                updater: None,
            },
            capacity,
        )
    }

    /// A running service on a temp socket; call inside a `LocalSet`.
    async fn start(capacity: usize) -> Svc {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("yplay.sock");
        launch(dir, sock, capacity, None).await
    }

    static SOCK_SEQ: AtomicUsize = AtomicUsize::new(0);

    /// A running service whose worker is `FAKE_WORKER`; its socket is a
    /// short path under /tmp.
    async fn start_with_worker() -> Svc {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake_worker.py");
        std::fs::write(&script, FAKE_WORKER).unwrap();
        let sock = PathBuf::from(format!(
            "/tmp/yp13-{}-{}.sock",
            std::process::id(),
            SOCK_SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let cmd = vec![
            "/usr/bin/python3".to_string(),
            script.to_string_lossy().into_owned(),
        ];
        launch(dir, sock, EVENT_CAPACITY, Some(cmd)).await
    }

    async fn launch(
        dir: tempfile::TempDir,
        sock: PathBuf,
        capacity: usize,
        worker_cmd: Option<Vec<String>>,
    ) -> Svc {
        launch_with(dir, sock, capacity, worker_cmd, None).await
    }

    /// `launch`, measuring loudness with `measurer`.
    async fn launch_with(
        dir: tempfile::TempDir,
        sock: PathBuf,
        capacity: usize,
        worker_cmd: Option<Vec<String>>,
        measurer: Option<Arc<dyn Measure>>,
    ) -> Svc {
        let fake = FakeSpawner::default();
        let listener = bind_socket(&sock).await.unwrap();
        let (mut core, inbox) = new_core(dir.path(), &fake, capacity, worker_cmd);
        if let Some(measurer) = measurer {
            core.set_measurer(measurer);
        }
        core.start();
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::task::spawn_local(run(core, inbox, listener, sock.clone(), async move {
            let _ = stopped.await;
        }));
        Svc {
            dir,
            sock,
            fake,
            stop: Some(stop),
            task: Some(task),
        }
    }

    struct Client {
        lines: Lines<BufReader<OwnedReadHalf>>,
        wr: OwnedWriteHalf,
    }

    impl Client {
        async fn connect(sock: &Path) -> Client {
            let (rd, wr) = UnixStream::connect(sock).await.unwrap().into_split();
            Client {
                lines: BufReader::new(rd).lines(),
                wr,
            }
        }

        async fn send(&mut self, v: Value) {
            let mut line = v.to_string();
            line.push('\n');
            self.wr.write_all(line.as_bytes()).await.unwrap();
        }

        /// Next line, or `None` at EOF.
        async fn next(&mut self) -> Option<Value> {
            let line = tokio::time::timeout(WAIT, self.lines.next_line())
                .await
                .expect("timed out waiting for a line")
                .ok()??;
            Some(serde_json::from_str(&line).unwrap())
        }

        async fn recv(&mut self) -> Value {
            self.next().await.expect("connection closed")
        }

        /// Send a request and return its response (the client is not subscribed).
        async fn call(&mut self, v: Value) -> Value {
            let id = v["id"].clone();
            self.send(v).await;
            let resp = self.recv().await;
            assert_eq!(resp["id"], id, "{resp}");
            resp
        }

        /// A connection that has subscribed; it then only receives events.
        async fn subscribed(sock: &Path) -> Client {
            let mut c = Client::connect(sock).await;
            c.call(json!({"id": 0, "cmd": "subscribe"})).await;
            c
        }

        /// Events up to and including the first one matching `pred`.
        async fn until(&mut self, pred: impl Fn(&Value) -> bool) -> Vec<Value> {
            let mut seen = Vec::new();
            loop {
                let ev = self.recv().await;
                let hit = pred(&ev);
                seen.push(ev);
                if hit {
                    return seen;
                }
            }
        }
    }

    #[tokio::test]
    async fn hello_subscribe_and_library_get_shapes() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start(EVENT_CAPACITY).await;
                let mut c = Client::connect(&svc.sock).await;

                let resp = c
                    .call(json!({"id": 1, "cmd": "hello", "protocol": 1}))
                    .await;
                assert_eq!(
                    resp,
                    json!({"id": 1, "ok": true, "result": {
                        "protocol": 1,
                        "server_version": env!("CARGO_PKG_VERSION"),
                    }})
                );
                let resp = c
                    .call(json!({"id": 2, "cmd": "hello", "protocol": 2}))
                    .await;
                assert_eq!(resp["ok"], json!(false));
                assert_eq!(resp["error"]["code"], json!("protocol_mismatch"));

                let resp = c.call(json!({"id": 3, "cmd": "subscribe"})).await;
                let result = &resp["result"];
                assert_eq!(result["library_version"], json!(0));
                let player = &result["player"];
                assert_eq!(player["state"], json!("stopped"));
                assert_eq!(player["track_id"], Value::Null);
                assert_eq!(player["volume"], json!(100.0));
                assert_eq!(player["loop"], json!("none"));
                for key in ["context", "position", "at_ms", "duration"] {
                    assert!(player.get(key).is_some(), "player.{key} missing");
                }

                let mut c = Client::connect(&svc.sock).await;
                let resp = c.call(json!({"id": 4, "cmd": "library.get"})).await;
                let result = &resp["result"];
                assert_eq!(result["library_version"], json!(0));
                assert_eq!(result["albums"], json!([]));
                let tracks = result["tracks"].as_array().unwrap();
                let ids: Vec<&str> = tracks.iter().map(|t| t["id"].as_str().unwrap()).collect();
                assert_eq!(ids, [A, B]);
                assert_eq!(tracks[0]["title"], json!(format!("Song {A}")));
                assert_eq!(tracks[0]["state"], json!("complete"));
                for key in [
                    "uploader",
                    "duration",
                    "added_at",
                    "last_played",
                    "thumb_path",
                    "audio_path",
                ] {
                    assert!(tracks[0].get(key).is_some(), "track.{key} missing");
                }
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn album_edits_emit_events_and_bump_library_version() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start(EVENT_CAPACITY).await;
                let mut sub = Client::connect(&svc.sock).await;
                let v0 = sub.call(json!({"id": 1, "cmd": "subscribe"})).await["result"]
                    ["library_version"]
                    .as_u64()
                    .unwrap();
                let mut c = Client::connect(&svc.sock).await;

                let resp = c
                    .call(json!({"id": 2, "cmd": "album.create", "name": "Mix"}))
                    .await;
                let album = &resp["result"]["album"];
                assert_eq!(album["name"], json!("Mix"));
                assert_eq!(album["track_ids"], json!([]));
                let aid = album["id"].as_i64().unwrap();
                let ev = sub.recv().await;
                assert_eq!(ev["event"], json!("album.upsert"));
                assert_eq!(ev["album"], *album);

                let resp = c
                    .call(json!({"id": 3, "cmd": "album.create", "name": "Mix"}))
                    .await;
                assert_eq!(resp["error"]["code"], json!("conflict"));

                let expected = [
                    (
                        json!({"id": 4, "cmd": "album.rename", "album_id": aid, "name": "Mix 2"}),
                        json!([]),
                    ),
                    (
                        json!({"id": 5, "cmd": "album.add", "album_id": aid, "track_id": A}),
                        json!([A]),
                    ),
                    (
                        json!({"id": 6, "cmd": "album.add", "album_id": aid, "track_id": B}),
                        json!([A, B]),
                    ),
                    (
                        json!({"id": 7, "cmd": "album.reorder", "album_id": aid, "track_ids": [B, A]}),
                        json!([B, A]),
                    ),
                    (
                        json!({"id": 8, "cmd": "album.remove", "album_id": aid, "track_id": B}),
                        json!([A]),
                    ),
                ];
                for (req, track_ids) in expected {
                    let resp = c.call(req).await;
                    assert_eq!(resp["ok"], json!(true), "{resp}");
                    let ev = sub.recv().await;
                    assert_eq!(ev["event"], json!("album.upsert"));
                    assert_eq!(ev["album"]["id"], json!(aid));
                    assert_eq!(ev["album"]["name"], json!("Mix 2"));
                    assert_eq!(ev["album"]["track_ids"], track_ids);
                }

                let resp = c
                    .call(json!({"id": 9, "cmd": "album.delete", "album_id": aid}))
                    .await;
                assert_eq!(resp["ok"], json!(true));
                assert_eq!(
                    sub.recv().await,
                    json!({"event": "album.removed", "album_id": aid})
                );

                let resp = c.call(json!({"id": 10, "cmd": "library.get"})).await;
                assert_eq!(resp["result"]["library_version"], json!(v0 + 7));
                assert_eq!(resp["result"]["albums"], json!([]));
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn play_album_context_emits_player_with_context() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start(EVENT_CAPACITY).await;
                let mut c = Client::connect(&svc.sock).await;
                let aid = c
                    .call(json!({"id": 1, "cmd": "album.create", "name": "Mix"}))
                    .await["result"]["album"]["id"]
                    .as_i64()
                    .unwrap();
                for (id, track) in [(2, B), (3, A)] {
                    c.call(json!({"id": id, "cmd": "album.add", "album_id": aid, "track_id": track}))
                        .await;
                }
                let mut sub = Client::connect(&svc.sock).await;
                sub.call(json!({"id": 1, "cmd": "subscribe"})).await;

                let resp = c
                    .call(json!({"id": 4, "cmd": "play", "track_id": A, "context": {"album_id": aid}}))
                    .await;
                assert_eq!(resp, json!({"id": 4, "ok": true, "result": {}}));
                let ev = sub.recv().await;
                assert_eq!(ev["event"], json!("player"));
                assert_eq!(ev["state"], json!("playing"));
                assert_eq!(ev["track_id"], json!(A));
                assert_eq!(ev["context"], json!({"album_id": aid}));

                let path = svc
                    .cache()
                    .join(format!("Song {A} [aaaaaaaa]/audio.opus"))
                    .to_string_lossy()
                    .into_owned();
                assert_eq!(svc.fake.commands(), vec![loadfile(&path)]);
                let lib = c.call(json!({"id": 5, "cmd": "library.get"})).await;
                let track = &lib["result"]["tracks"][0];
                assert_eq!(track["id"], json!(A));
                assert!(track["last_played"].is_i64());
                assert!(lib["result"]["albums"][0]["last_used_at"].is_i64());
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn second_instance_refuses_to_start() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start(EVENT_CAPACITY).await;
                let err = bind_socket(&svc.sock).await.unwrap_err();
                assert!(err.to_string().contains("already running"), "{err}");
                let mut c = Client::connect(&svc.sock).await;
                let resp = c
                    .call(json!({"id": 1, "cmd": "hello", "protocol": 1}))
                    .await;
                assert_eq!(resp["ok"], json!(true));
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("yplay.sock");
        drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
        assert!(sock.exists());
        assert!(UnixStream::connect(&sock).await.is_err());

        let listener = bind_socket(&sock).await.unwrap();
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let (client, accepted) = tokio::join!(UnixStream::connect(&sock), listener.accept());
        client.unwrap();
        accepted.unwrap();
    }

    #[tokio::test]
    async fn oversized_line_gets_bad_request_and_close() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start(EVENT_CAPACITY).await;
                let (rd, mut wr) = UnixStream::connect(&svc.sock).await.unwrap().into_split();
                let writer = tokio::spawn(async move {
                    let mut line = "a".repeat(MAX_LINE + 10).into_bytes();
                    line.push(b'\n');
                    let _ = wr.write_all(&line).await;
                    wr
                });
                let mut c = Client {
                    lines: BufReader::new(rd).lines(),
                    wr: writer.await.unwrap(),
                };
                let resp = c.recv().await;
                assert_eq!(resp["id"], json!(0));
                assert_eq!(resp["ok"], json!(false));
                assert_eq!(resp["error"]["code"], json!("bad_request"));
                assert_eq!(c.next().await, None);

                // The service keeps serving other connections.
                let mut c = Client::connect(&svc.sock).await;
                let resp = c
                    .call(json!({"id": 1, "cmd": "hello", "protocol": 1}))
                    .await;
                assert_eq!(resp["ok"], json!(true));
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn lagging_subscriber_gets_resync() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start(2).await;
                let mut sub = Client::connect(&svc.sock).await;
                sub.call(json!({"id": 1, "cmd": "subscribe"})).await;

                // `sub` reads nothing while ~120 KB of events are produced
                // (names are capped at 200 characters).
                let mut c = Client::connect(&svc.sock).await;
                let pad = "x".repeat(196);
                for i in 0..400 {
                    let resp = c
                        .call(
                            json!({"id": i, "cmd": "album.create", "name": format!("{i:04}{pad}")}),
                        )
                        .await;
                    assert_eq!(resp["ok"], json!(true));
                }
                loop {
                    let ev = sub.recv().await;
                    if ev == json!({"event": "resync"}) {
                        break;
                    }
                    assert_eq!(ev["event"], json!("album.upsert"));
                }
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn shutdown_saves_session() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("yplay.sock");
        let _listener = bind_socket(&sock).await.unwrap();
        let fake = FakeSpawner::default();
        let (mut core, _inbox) = new_core(dir.path(), &fake, EVENT_CAPACITY, None);
        for cmd in [
            Command::Volume { value: 42.0 },
            Command::Loop {
                mode: LoopMode::All,
            },
            Command::Play {
                track_id: A.into(),
                context: ContextRef::Library,
            },
        ] {
            let (reply, rx) = oneshot::channel();
            core.on_request(Request { id: 1, cmd, reply }).await;
            let resp = rx.await.unwrap().response;
            assert!(resp.ok, "{resp:?}");
        }
        assert_eq!(fake.spawns(), 1);

        shutdown(&mut core, &sock).await;
        assert_eq!(fake.quits(), 1);
        assert!(!sock.exists());
        let session = SessionState::load(&config(dir.path()).state_path());
        assert_eq!(session.volume, Some(42.0));
        assert_eq!(session.loop_mode, Some(LoopMode::All));
        assert_eq!(session.last_track_id.as_deref(), Some(A));
        assert_eq!(session.last_context, Some(json!({"library": true})));
    }

    // Speaks worker protocol v2. The video id's first four characters pick
    // the behaviour: `okay` completes at once; `hold` waits for a
    // `release-<id>` file (or a cancel); `lagc` is `hold` whose cancel only
    // ends once released; `fail` fails after `started`; `flak` fails the
    // first time and completes afterwards. Spawns and cancels are appended
    // to files next to the script.
    const FAKE_WORKER: &str = r#"
import json, os, sys, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
lock = threading.Lock()
cancels = {}


def note(name, text):
    with open(os.path.join(HERE, name), "a") as f:
        f.write(text + "\n")


def send(obj):
    with lock:
        sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
        sys.stdout.flush()


def held(n, vid):
    release = os.path.join(HERE, "release-" + vid)
    deadline = time.time() + 30
    while time.time() < deadline:
        if cancels[n].is_set():
            return "cancelled"
        if os.path.exists(release):
            return "released"
        time.sleep(0.02)
    return "timeout"


def download(req):
    n = req["id"]
    vid = req["url"].split("v=", 1)[1][:11]
    kind = vid[:4]
    if kind == "flak":
        marker = os.path.join(HERE, "flaky-" + vid)
        kind = "okay" if os.path.exists(marker) else "fail"
        open(marker, "a").close()
    d = os.path.join(req["cache_dir"], "Title " + vid + " [" + vid[:8] + "]")
    path = os.path.join(d, "audio.webm")
    meta = {"id": vid, "title": "Title " + vid, "uploader": "Up", "duration": 42,
            "webpage_url": "https://www.youtube.com/watch?v=" + vid}
    os.makedirs(d, exist_ok=True)
    with open(path, "wb") as f:
        f.write(b"x" * 100)
    send({"id": n, "event": "started", "path": path, "dir": d, "meta": meta})
    send({"id": n, "event": "progress", "bytes": 50, "total": 100})
    send({"id": n, "event": "progress", "bytes": 100, "total": 100})
    if kind == "fail":
        send({"id": n, "ok": False, "error": "ERROR: [youtube] " + vid + ": Video unavailable",
              "cancelled": False})
        return
    if kind in ("hold", "lagc"):
        outcome = held(n, vid)
        if outcome == "cancelled":
            deadline = time.time() + 30
            while kind == "lagc" and time.time() < deadline and not os.path.exists(
                    os.path.join(HERE, "release-" + vid)):
                time.sleep(0.02)
            send({"id": n, "ok": False, "error": "cancelled", "cancelled": True})
            return
        if outcome == "timeout":
            send({"id": n, "ok": False, "error": "hold timed out", "cancelled": False})
            return
    thumb = os.path.join(d, "cover.jpg")
    open(thumb, "wb").close()
    with open(os.path.join(d, "meta.json"), "w") as f:
        json.dump(meta, f)
    send({"id": n, "ok": True, "path": path, "dir": d, "meta": meta, "thumb": thumb,
          "format": "webm", "file_size": 100})


note("spawns", str(os.getpid()))
send({"event": "ready", "ok": True, "protocol": 2})
while True:
    line = sys.stdin.readline()
    if not line:
        break
    req = json.loads(line)
    if req["cmd"] == "cancel":
        note("cancels", str(req["target"]))
        if req["target"] in cancels:
            cancels[req["target"]].set()
        send({"id": req["id"], "ok": True})
    elif req["cmd"] == "download":
        cancels[req["id"]] = threading.Event()
        threading.Thread(target=download, args=(req,), daemon=True).start()
"#;

    fn is_download(ev: &Value, vid: &str, phase: &str) -> bool {
        ev["event"] == "download" && ev["track_id"] == vid && ev["phase"] == phase
    }

    fn is_player(ev: &Value, vid: &str) -> bool {
        ev["event"] == "player" && ev["track_id"] == vid
    }

    /// `loadfile replace` at gain 0 (nothing measured).
    fn loadfile(path: &str) -> Vec<Value> {
        loadfile_gain(path, "replace", "0.00")
    }

    fn loadfile_gain(path: &str, mode: &str, gain: &str) -> Vec<Value> {
        vec![
            json!("loadfile"),
            json!(path),
            json!(mode),
            json!(-1),
            json!(format!("volume-gain={gain}")),
        ]
    }

    fn seeded_path(svc: &Svc, id: &str) -> String {
        svc.cache()
            .join(format!("Song {id} [{}]/audio.opus", &id[..8]))
            .to_string_lossy()
            .into_owned()
    }

    fn appending(svc: &Svc, vid: &str) -> String {
        format!(
            "appending://{}",
            svc.track_dir(vid).join("audio.webm").display()
        )
    }

    fn track<'a>(lib: &'a Value, id: &str) -> Option<&'a Value> {
        lib["result"]["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
    }

    #[tokio::test]
    async fn add_cache_hit_plays_without_worker_spawn() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;
                let aid = c
                    .call(json!({"id": 1, "cmd": "album.create", "name": "Mix"}))
                    .await["result"]["album"]["id"]
                    .as_i64()
                    .unwrap();
                c.call(json!({"id": 2, "cmd": "album.add", "album_id": aid, "track_id": B}))
                    .await;

                let resp = c
                    .call(json!({"id": 3, "cmd": "add",
                        "url": format!("https://www.youtube.com/watch?v={A}&list=PLx"),
                        "album": {"name": "Mix"}}))
                    .await;
                assert_eq!(
                    resp["result"],
                    json!({"track_id": A, "album_id": aid, "was_new": false, "was_in_album": false})
                );
                let evs = sub.until(|ev| is_player(ev, A)).await;
                let player = evs.last().unwrap();
                assert_eq!(player["state"], json!("playing"));
                assert_eq!(player["context"], json!({"album_id": aid}));
                assert_eq!(svc.fake.commands(), vec![loadfile(&seeded_path(&svc, A))]);

                // Drop-play context: the dropped track, then the album's others.
                c.call(json!({"id": 4, "cmd": "next"})).await;
                assert_eq!(
                    svc.fake.commands().last().unwrap(),
                    &loadfile(&seeded_path(&svc, B))
                );

                // Default album: last used; already linked now.
                let resp = c
                    .call(
                        json!({"id": 5, "cmd": "add", "url": format!("https://youtu.be/{A}"),
                        "album": null, "play": false}),
                    )
                    .await;
                assert_eq!(resp["result"]["album_id"], json!(aid));
                assert_eq!(resp["result"]["was_in_album"], json!(true));
                let lib = c.call(json!({"id": 6, "cmd": "library.get"})).await;
                assert_eq!(lib["result"]["albums"][0]["track_ids"], json!([B, A]));
                assert_eq!(svc.spawns(), 0);
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn add_new_url_streams_via_appending_then_completes() {
        const V: &str = "okayaaaaaaa";
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;

                let resp = c
                    .call(
                        json!({"id": 1, "cmd": "add", "url": format!("https://youtu.be/{V}?si=x"),
                        "album": null}),
                    )
                    .await;
                let result = &resp["result"];
                assert_eq!(result["track_id"], json!(V));
                assert_eq!(result["was_new"], json!(true));
                assert_eq!(result["was_in_album"], json!(false));

                let evs = sub.until(|ev| is_download(ev, V, "done")).await;
                let steps: Vec<String> = evs
                    .iter()
                    .filter(|ev| ev["event"] == "download" || ev["event"] == "player")
                    .map(|ev| match ev["event"].as_str().unwrap() {
                        "player" => format!("player:{}", ev["state"].as_str().unwrap()),
                        _ => format!("{}:{}", ev["phase"].as_str().unwrap(), ev["bytes"]),
                    })
                    .collect();
                // The second progress line arrives within 500 ms: throttled.
                assert_eq!(
                    steps,
                    [
                        "fetching:null",
                        "downloading:null",
                        "player:playing",
                        "downloading:50",
                        "done:null",
                    ]
                );
                assert!(evs.iter().any(|ev| ev["event"] == "album.upsert"
                    && ev["album"]["name"] == "Inbox"
                    && ev["album"]["track_ids"] == json!([V])));
                assert_eq!(svc.fake.commands(), vec![loadfile(&appending(&svc, V))]);

                let lib = c.call(json!({"id": 2, "cmd": "library.get"})).await;
                let t = track(&lib, V).unwrap();
                let dir = svc.track_dir(V);
                assert_eq!(t["state"], json!("complete"));
                assert_eq!(t["title"], json!(format!("Title {V}")));
                assert_eq!(t["audio_path"], json!(dir.join("audio.webm")));
                assert_eq!(t["thumb_path"], json!(dir.join("cover.jpg")));
                assert_eq!(t["format"], json!("webm"));
                assert_eq!(t["file_size"], json!(100));
                assert_eq!(svc.spawns(), 1);
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn second_drop_plays_on_its_started_while_first_still_downloads() {
        const V1: &str = "holdaaaaaaa";
        const V2: &str = "holdbbbbbbb";
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;

                c.call(json!({"id": 1, "cmd": "add", "url": format!("https://youtu.be/{V1}"), "album": null}))
                    .await;
                sub.until(|ev| is_player(ev, V1)).await;
                c.call(json!({"id": 2, "cmd": "add", "url": format!("https://youtu.be/{V2}"), "album": null}))
                    .await;
                sub.until(|ev| is_player(ev, V2)).await;
                assert_eq!(
                    svc.fake.commands(),
                    vec![loadfile(&appending(&svc, V1)), loadfile(&appending(&svc, V2))]
                );
                let lib = c.call(json!({"id": 3, "cmd": "library.get"})).await;
                for v in [V1, V2] {
                    assert_eq!(track(&lib, v).unwrap()["state"], json!("downloading"));
                }

                svc.release(V1);
                svc.release(V2);
                sub.until(|ev| is_download(ev, V1, "done")).await;
                let lib = c.call(json!({"id": 4, "cmd": "library.get"})).await;
                assert_eq!(track(&lib, V1).unwrap()["state"], json!("complete"));
                assert_eq!(svc.spawns(), 1);
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn add_rejects_playlist_and_non_youtube_urls() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut c = Client::connect(&svc.sock).await;
                let resp = c
                    .call(json!({"id": 1, "cmd": "add", "url": "https://www.youtube.com/playlist?list=PLabc", "album": null}))
                    .await;
                assert_eq!(
                    resp["error"],
                    json!({"code": "unsupported_url", "message": "Playlists are not supported yet"})
                );
                for (id, url) in [
                    (2, "https://vimeo.com/123"),
                    (3, "https://www.youtube.com/watch?v=short"),
                ] {
                    let resp = c
                        .call(json!({"id": id, "cmd": "add", "url": url, "album": null}))
                        .await;
                    assert_eq!(resp["error"]["code"], json!("invalid_url"), "{url}");
                }
                let lib = c.call(json!({"id": 4, "cmd": "library.get"})).await;
                assert_eq!(lib["result"]["albums"], json!([]));
                assert_eq!(svc.spawns(), 0);
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn undo_during_download_cancels_job_and_removes_dir() {
        const V: &str = "holdccccccc";
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;

                let resp = c
                    .call(json!({"id": 1, "cmd": "add", "url": format!("https://youtu.be/{V}"), "album": null}))
                    .await;
                let aid = resp["result"]["album_id"].as_i64().unwrap();
                sub.until(|ev| is_player(ev, V)).await;
                assert!(svc.track_dir(V).is_dir());

                let resp = c
                    .call(json!({"id": 2, "cmd": "album.remove", "album_id": aid, "track_id": V}))
                    .await;
                assert_eq!(resp["ok"], json!(true));
                let resp = c
                    .call(json!({"id": 3, "cmd": "track.delete", "track_id": V, "to_trash": false}))
                    .await;
                assert_eq!(resp["ok"], json!(true), "{resp}");

                let evs = sub.until(|ev| is_download(ev, V, "cancelled")).await;
                assert!(evs.contains(&json!({"event": "track.removed", "track_id": V})));
                assert!(!svc.track_dir(V).exists());
                assert_eq!(lines(&svc.dir.path().join("cancels")).len(), 1);
                let lib = c.call(json!({"id": 4, "cmd": "library.get"})).await;
                assert!(track(&lib, V).is_none());
                assert_eq!(lib["result"]["albums"][0]["track_ids"], json!([]));
                let now = c.call(json!({"id": 5, "cmd": "now"})).await;
                assert_eq!(now["result"]["player"]["state"], json!("stopped"));
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn failed_download_deletes_dir_marks_failed_and_toasts() {
        const V: &str = "failaaaaaaa";
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;

                c.call(
                    json!({"id": 1, "cmd": "add", "url": format!("https://youtu.be/{V}"),
                    "album": null, "play": false}),
                )
                .await;
                let evs = sub.until(|ev| ev["event"] == "toast").await;
                let error = format!("ERROR: [youtube] {V}: Video unavailable");
                assert_eq!(
                    evs.last().unwrap(),
                    &json!({"event": "toast", "severity": "error", "message": error})
                );
                assert!(
                    evs.iter()
                        .any(|ev| is_download(ev, V, "failed") && ev["error"] == error)
                );
                assert!(evs.iter().any(|ev| ev["event"] == "track.upsert"
                    && ev["track"]["id"] == V
                    && ev["track"]["state"] == "failed"));
                assert!(!svc.track_dir(V).exists());
                let lib = c.call(json!({"id": 2, "cmd": "library.get"})).await;
                assert_eq!(track(&lib, V).unwrap()["state"], json!("failed"));
                assert!(svc.fake.commands().is_empty());
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn retry_redownloads_failed_track() {
        const V: &str = "flakaaaaaaa";
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;

                c.call(
                    json!({"id": 1, "cmd": "add", "url": format!("https://youtu.be/{V}"),
                    "album": null, "play": false}),
                )
                .await;
                sub.until(|ev| is_download(ev, V, "failed")).await;

                let resp = c
                    .call(json!({"id": 2, "cmd": "track.retry", "track_id": A}))
                    .await;
                assert_eq!(resp["error"]["code"], json!("bad_request"));
                let resp = c
                    .call(json!({"id": 3, "cmd": "track.retry", "track_id": V}))
                    .await;
                assert_eq!(resp, json!({"id": 3, "ok": true, "result": {}}));
                let evs = sub.until(|ev| is_download(ev, V, "done")).await;
                assert!(evs.iter().any(|ev| is_download(ev, V, "fetching")));
                assert!(!evs.iter().any(|ev| ev["event"] == "album.upsert"));

                let lib = c.call(json!({"id": 4, "cmd": "library.get"})).await;
                assert_eq!(track(&lib, V).unwrap()["state"], json!("complete"));
                assert_eq!(lib["result"]["albums"][0]["track_ids"], json!([V]));
                assert!(svc.track_dir(V).join("audio.webm").is_file());
                assert!(svc.fake.commands().is_empty());
                assert_eq!(svc.spawns(), 1);
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn deleting_current_track_advances_playback() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut c = Client::connect(&svc.sock).await;
                let aid = c
                    .call(json!({"id": 1, "cmd": "album.create", "name": "Mix"}))
                    .await["result"]["album"]["id"]
                    .as_i64()
                    .unwrap();
                for (id, t) in [(2, A), (3, B)] {
                    c.call(json!({"id": id, "cmd": "album.add", "album_id": aid, "track_id": t}))
                        .await;
                }
                c.call(
                    json!({"id": 4, "cmd": "play", "track_id": A, "context": {"album_id": aid}}),
                )
                .await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let a_dir = svc.cache().join(format!("Song {A} [aaaaaaaa]"));
                assert!(a_dir.is_dir());

                let resp = c
                    .call(json!({"id": 5, "cmd": "track.delete", "track_id": A, "to_trash": false}))
                    .await;
                assert_eq!(resp, json!({"id": 5, "ok": true, "result": {}}));
                let evs = sub.until(|ev| is_player(ev, B)).await;
                assert_eq!(evs.last().unwrap()["state"], json!("playing"));
                assert!(evs.contains(&json!({"event": "track.removed", "track_id": A})));
                assert!(
                    evs.iter().any(|ev| ev["event"] == "album.upsert"
                        && ev["album"]["track_ids"] == json!([B]))
                );
                assert_eq!(
                    svc.fake.commands().last().unwrap(),
                    &loadfile(&seeded_path(&svc, B))
                );
                assert!(!a_dir.exists());
                assert!(Path::new(&seeded_path(&svc, B)).exists());
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn readd_after_undo_while_old_job_ends_completes_new_download() {
        const V: &str = "lagcaaaaaaa";
        let started = |ev: &Value| is_download(ev, V, "downloading") && ev["bytes"].is_null();
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;
                let add = json!({"id": 1, "cmd": "add", "url": format!("https://youtu.be/{V}"),
                    "album": null, "play": false});

                c.call(add.clone()).await;
                sub.until(started).await;
                let resp = c
                    .call(json!({"id": 2, "cmd": "track.delete", "track_id": V, "to_trash": false}))
                    .await;
                assert_eq!(resp["ok"], json!(true), "{resp}");
                // The old job ends only once released: re-add while it is still ending.
                c.call(add).await;
                sub.until(started).await;
                svc.release(V);
                let (mut done, mut cancelled) = (false, false);
                while !(done && cancelled) {
                    let ev = sub.recv().await;
                    done |= is_download(&ev, V, "done");
                    cancelled |= is_download(&ev, V, "cancelled");
                }

                let dir = svc.track_dir(V);
                assert!(dir.join("audio.webm").is_file());
                assert!(dir.join("meta.json").is_file());
                let lib = c.call(json!({"id": 3, "cmd": "library.get"})).await;
                let t = track(&lib, V).unwrap();
                assert_eq!(t["state"], json!("complete"));
                assert_eq!(t["audio_path"], json!(dir.join("audio.webm")));
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn garbage_db_is_moved_aside_and_library_rebuilt() {
        LocalSet::new()
            .run_until(async {
                let dir = tempfile::tempdir().unwrap();
                let config = config(dir.path());
                std::fs::create_dir_all(&config.cache_dir).unwrap();
                let garbage: Vec<u8> = (0..8192u32).map(|i| (i * 7 % 251) as u8).collect();
                std::fs::write(config.db_path(), &garbage).unwrap();
                let folder = config.cache_dir.join(format!("Song {A} [aaaaaaaa]"));
                std::fs::create_dir(&folder).unwrap();
                std::fs::write(folder.join("audio.opus"), b"x").unwrap();
                std::fs::write(
                    folder.join("meta.json"),
                    format!(r#"{{"id": "{A}", "title": "Rebuilt"}}"#),
                )
                .unwrap();

                let (db, rebuilt) = open_db(&config).unwrap();
                assert!(rebuilt);
                let aside: Vec<PathBuf> = std::fs::read_dir(&config.cache_dir)
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .filter(|p| {
                        p.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with(".yplayer.db.corrupt-")
                    })
                    .collect();
                assert_eq!(aside.len(), 1, "{aside:?}");
                assert_eq!(std::fs::read(&aside[0]).unwrap(), garbage);

                let sock = PathBuf::from(format!(
                    "/tmp/yphA-{}-{}.sock",
                    std::process::id(),
                    SOCK_SEQ.fetch_add(1, Ordering::SeqCst)
                ));
                let listener = bind_socket(&sock).await.unwrap();
                let fake = FakeSpawner::default();
                let worker = WorkerHandle::spawn(WorkerOptions {
                    worker_python: None,
                    worker_cmd: None,
                    log_path: dir.path().join("worker.log"),
                    idle_timeout: WORKER_IDLE,
                    inactivity_timeout: WORKER_INACTIVITY,
                });
                let http: Arc<dyn HttpGet> = Arc::new(CurlHttp {
                    user_agent: USER_AGENT.to_string(),
                });
                let (mut core, inbox) = Core::new(CoreDeps {
                    config,
                    db,
                    spawner: fake.clone(),
                    worker,
                    http,
                    updater: None,
                });
                core.warn_after_startup(DB_REBUILT);
                core.start();
                let (stop, stopped) = oneshot::channel::<()>();
                let task = tokio::task::spawn_local(run(
                    core,
                    inbox,
                    listener,
                    sock.clone(),
                    async move {
                        let _ = stopped.await;
                    },
                ));
                let mut svc = Svc {
                    dir,
                    sock,
                    fake,
                    stop: Some(stop),
                    task: Some(task),
                };

                let mut sub = Client::subscribed(&svc.sock).await;
                let evs = sub.until(|ev| ev["event"] == "toast").await;
                assert_eq!(
                    evs.last().unwrap(),
                    &json!({"event": "toast", "severity": "warn", "message": DB_REBUILT})
                );
                let mut c = Client::connect(&svc.sock).await;
                let lib = c.call(json!({"id": 1, "cmd": "library.get"})).await;
                assert_eq!(track(&lib, A).unwrap()["title"], json!("Rebuilt"));
                assert!(aside[0].exists());
                svc.stop().await;
            })
            .await;
    }

    /// Announces each measured path on `started`, then waits for a permit on
    /// `release`; answers from `levels` by track id (others fail).
    struct FakeMeasurer {
        started: mpsc::UnboundedSender<String>,
        release: Arc<tokio::sync::Semaphore>,
        levels: Vec<(&'static str, Measurement)>,
        running: Arc<AtomicUsize>,
        most_running: Arc<AtomicUsize>,
    }

    impl Measure for FakeMeasurer {
        fn measure(&self, path: PathBuf) -> MeasureFuture {
            let path = path.to_string_lossy().into_owned();
            let result = self
                .levels
                .iter()
                .find(|(id, _)| path.contains(id))
                .map(|(_, m)| *m)
                .ok_or_else(|| "no usable loudness summary".to_string());
            let _ = self.started.send(path);
            let release = self.release.clone();
            let (running, most) = (self.running.clone(), self.most_running.clone());
            Box::pin(async move {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                release.acquire().await.unwrap().forget();
                running.fetch_sub(1, Ordering::SeqCst);
                result
            })
        }
    }

    fn lvl(integrated: f64, sample_peak: f64) -> Measurement {
        Measurement {
            integrated,
            sample_peak,
        }
    }

    /// Poll `done` until it holds (the core works in the background).
    async fn eventually(what: &str, done: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + WAIT;
        while !done() {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn loudness_is_measured_one_at_a_time_and_reaches_mpv() {
        const V: &str = "okayddddddd";
        LocalSet::new()
            .run_until(async {
                let (started_tx, mut started) = mpsc::unbounded_channel();
                let release = Arc::new(tokio::sync::Semaphore::new(0));
                let most_running = Arc::new(AtomicUsize::new(0));
                let measurer = FakeMeasurer {
                    started: started_tx,
                    release: release.clone(),
                    // A: +6 dB; B: −2 dB; V fails (plays at the median, +2).
                    levels: vec![(A, lvl(-20.0, -10.0)), (B, lvl(-12.0, -3.0))],
                    running: Arc::new(AtomicUsize::new(0)),
                    most_running: most_running.clone(),
                };
                let dir = tempfile::tempdir().unwrap();
                let script = dir.path().join("fake_worker.py");
                std::fs::write(&script, FAKE_WORKER).unwrap();
                let sock = PathBuf::from(format!(
                    "/tmp/yp13-{}-{}.sock",
                    std::process::id(),
                    SOCK_SEQ.fetch_add(1, Ordering::SeqCst)
                ));
                let cmd = vec![
                    "/usr/bin/python3".to_string(),
                    script.to_string_lossy().into_owned(),
                ];
                let mut svc = launch_with(
                    dir,
                    sock,
                    EVENT_CAPACITY,
                    Some(cmd),
                    Some(Arc::new(measurer)),
                )
                .await;
                let next_started = async |started: &mut mpsc::UnboundedReceiver<String>| {
                    tokio::time::timeout(WAIT, started.recv())
                        .await
                        .expect("no measurement started")
                        .unwrap()
                };

                // The backfill after the startup reconcile: newest first.
                assert_eq!(next_started(&mut started).await, seeded_path(&svc, A));
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;
                let settings = c.call(json!({"id": 1, "cmd": "settings.get"})).await;
                assert_eq!(settings["result"]["loudness_available"], json!(true));

                // A download that finishes waits for the running measurement,
                // then goes before the rest of the backfill.
                c.call(
                    json!({"id": 2, "cmd": "add", "url": format!("https://youtu.be/{V}"),
                    "album": null, "play": false}),
                )
                .await;
                sub.until(|ev| is_download(ev, V, "done")).await;
                c.call(json!({"id": 3, "cmd": "now"})).await;
                assert!(started.try_recv().is_err());
                release.add_permits(1);
                let v_path = svc.track_dir(V).join("audio.webm");
                assert_eq!(next_started(&mut started).await, v_path.to_string_lossy());
                release.add_permits(1);
                assert_eq!(next_started(&mut started).await, seeded_path(&svc, B));
                release.add_permits(1);
                let db = Db::open(&svc.cache().join(".yplayer.db"), &svc.cache()).unwrap();
                eventually("B stored", || matches!(db.measure_path(B), Ok(None))).await;
                assert_eq!(db.measured().unwrap().len(), 2);
                assert!(db.unmeasured().unwrap().is_empty());
                assert_eq!(most_running.load(Ordering::SeqCst), 1);

                // Gains reach mpv: V (failed) at the median, A at its own.
                c.call(
                    json!({"id": 4, "cmd": "play", "track_id": V, "context": {"library": true}}),
                )
                .await;
                assert_eq!(
                    svc.fake.commands().last().unwrap(),
                    &loadfile_gain(&v_path.to_string_lossy(), "replace", "2.00")
                );
                c.call(
                    json!({"id": 5, "cmd": "play", "track_id": A, "context": {"library": true}}),
                )
                .await;
                assert_eq!(
                    svc.fake.commands().last().unwrap(),
                    &loadfile_gain(&seeded_path(&svc, A), "replace", "6.00")
                );
                svc.fake.emit(MpvEvent::FileLoaded);
                let preload_b = loadfile_gain(&seeded_path(&svc, B), "append", "-2.00");
                eventually("B preloaded", || svc.fake.commands().contains(&preload_b)).await;

                // Switching leveling off sets the gain live and re-preloads.
                svc.fake.clear_commands();
                let resp = c
                    .call(json!({"id": 6, "cmd": "settings.set", "level_loudness": false}))
                    .await;
                assert_eq!(resp["result"]["level_loudness"], json!(false));
                assert_eq!(
                    svc.fake.commands(),
                    vec![
                        vec![json!("set_property"), json!("volume-gain"), json!(0.0)],
                        vec![json!("playlist-clear")],
                        loadfile_gain(&seeded_path(&svc, B), "append", "0.00"),
                    ]
                );
                let evs = sub.until(|ev| ev["event"] == "settings").await;
                assert_eq!(evs.last().unwrap()["level_loudness"], json!(false));
                svc.fake.clear_commands();
                c.call(json!({"id": 7, "cmd": "settings.set", "level_loudness": true}))
                    .await;
                assert_eq!(
                    svc.fake.commands()[0],
                    vec![json!("set_property"), json!("volume-gain"), json!(6.0)]
                );
                // Everything is measured: nothing else runs.
                c.call(json!({"id": 8, "cmd": "now"})).await;
                assert!(started.try_recv().is_err());
                svc.stop().await;
            })
            .await;
    }

    #[tokio::test]
    async fn settings_are_validated_saved_and_announced() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start(EVENT_CAPACITY).await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;
                let file = svc.dir.path().join("config.toml");
                let music = svc.cache().to_string_lossy().into_owned();
                let resp = c.call(json!({"id": 1, "cmd": "settings.get"})).await;
                assert_eq!(
                    resp["result"],
                    json!({"level_loudness": true, "loudness_available": false,
                        "api_key": std::env::var("YT_API_KEY").ok(), "music_folder": music})
                );

                // A bad key changes nothing, not even the other field.
                for (id, key) in [
                    (2, "abc def".to_string()),
                    (3, "x".repeat(201)),
                    (4, "tab\tkey".to_string()),
                    (5, "bell\u{7}".to_string()),
                ] {
                    let resp = c
                        .call(
                            json!({"id": id, "cmd": "settings.set", "level_loudness": false,
                            "api_key": key}),
                        )
                        .await;
                    assert_eq!(resp["error"]["code"], json!("bad_request"), "{key:?}");
                }
                assert!(!file.exists());
                let resp = c.call(json!({"id": 6, "cmd": "settings.get"})).await;
                assert_eq!(resp["result"]["level_loudness"], json!(true));

                let resp = c
                    .call(
                        json!({"id": 7, "cmd": "settings.set", "level_loudness": false,
                        "api_key": "  AIza-鍵_123  "}),
                    )
                    .await;
                let expected = json!({"level_loudness": false, "loudness_available": false,
                    "api_key": "AIza-鍵_123", "music_folder": music});
                assert_eq!(resp["result"], expected);
                let mut event = expected.clone();
                event["event"] = json!("settings");
                assert_eq!(sub.recv().await, event);
                let saved = FileConfig::load_from(&file);
                assert_eq!(saved.level_loudness, Some(false));
                assert_eq!(saved.api_key.as_deref(), Some("AIza-鍵_123"));

                // No change: no event. A blank key removes it (the next event
                // proves the no-op sent none).
                c.call(json!({"id": 8, "cmd": "settings.set", "level_loudness": false}))
                    .await;
                let resp = c
                    .call(json!({"id": 9, "cmd": "settings.set", "api_key": "   "}))
                    .await;
                assert_eq!(resp["result"]["api_key"], Value::Null);
                let ev = sub.recv().await;
                assert_eq!(ev["event"], json!("settings"));
                assert_eq!(ev["api_key"], Value::Null);
                assert!(FileConfig::load_from(&file).api_key.is_none());

                let key = "k".repeat(200);
                let resp = c
                    .call(json!({"id": 10, "cmd": "settings.set", "api_key": key}))
                    .await;
                assert_eq!(resp["result"]["api_key"], json!(key));
                assert_eq!(sub.recv().await["api_key"], json!(key));
                let resp = c
                    .call(json!({"id": 11, "cmd": "settings.set", "api_key": null}))
                    .await;
                assert_eq!(resp["result"]["api_key"], Value::Null);
                assert_eq!(sub.recv().await["api_key"], Value::Null);
                assert_eq!(FileConfig::load_from(&file).level_loudness, Some(false));
                svc.stop().await;
            })
            .await;
    }

    #[test]
    fn check_move_validates_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("yt-audio");
        std::fs::create_dir(&cache).unwrap();
        let other = dir.path().join("外付け");
        std::fs::create_dir(&other).unwrap();
        let a_file = dir.path().join("file");
        std::fs::write(&a_file, b"").unwrap();
        let real_other = other.canonicalize().unwrap();
        // Everything under `other` is on another disk.
        let device = |p: &Path| -> std::io::Result<u64> {
            Ok(if p.starts_with(&real_other) { 2 } else { 1 })
        };
        let s = |p: &Path| p.to_string_lossy().into_owned();
        const SAME_DISK: &str = "Choose a folder on the same disk as your music folder.";

        let target = dir.path().join("音楽");
        assert_eq!(check_move(&cache, &s(&target), device), Ok(target.clone()));
        assert_eq!(
            check_move(&cache, &format!("{}/", target.display()), device),
            Ok(target.clone())
        );
        assert_eq!(
            check_move(&cache, &s(&other.join("yt-audio")), device),
            Err(SAME_DISK)
        );
        for (to, message) in [
            (
                "yt-audio".to_string(),
                "The new location must be a full path.",
            ),
            (
                "~/Music/yt-audio".to_string(),
                "The new location must be a full path.",
            ),
            (
                s(&dir.path().join("missing/yt-audio")),
                "That folder's parent doesn't exist.",
            ),
            (
                s(&a_file.join("yt-audio")),
                "That folder's parent doesn't exist.",
            ),
            (s(&other), "Something already exists at that location."),
            (s(&a_file), "Something already exists at that location."),
            (
                "/".to_string(),
                "Something already exists at that location.",
            ),
            (s(&cache), "Something already exists at that location."),
            (
                s(&cache.join("inner")),
                "The new location can't be inside your music folder.",
            ),
        ] {
            assert_eq!(check_move(&cache, &to, device), Err(message), "{to}");
        }
        let unreadable = |_: &Path| -> std::io::Result<u64> { Err(std::io::Error::other("no")) };
        assert_eq!(check_move(&cache, &s(&target), unreadable), Err(SAME_DISK));
        // The real device check: a sibling folder is on the same disk.
        assert_eq!(check_move(&cache, &s(&target), device_of), Ok(target));
    }

    #[tokio::test]
    async fn library_move_records_the_request_and_restarts() {
        LocalSet::new()
            .run_until(async {
                let mut svc = start(EVENT_CAPACITY).await;
                let mut c = Client::connect(&svc.sock).await;
                let root = svc.dir.path().to_path_buf();
                let cache = svc.cache();
                for (id, to, message) in [
                    (
                        1,
                        "music".to_string(),
                        "The new location must be a full path.",
                    ),
                    (
                        2,
                        root.join("missing/yt-audio").to_string_lossy().into_owned(),
                        "That folder's parent doesn't exist.",
                    ),
                    (
                        3,
                        cache.to_string_lossy().into_owned(),
                        "Something already exists at that location.",
                    ),
                    (
                        4,
                        cache.join("inner").to_string_lossy().into_owned(),
                        "The new location can't be inside your music folder.",
                    ),
                ] {
                    let resp = c
                        .call(json!({"id": id, "cmd": "library.move", "to": to}))
                        .await;
                    assert_eq!(
                        resp["error"],
                        json!({"code": "bad_request", "message": message}),
                        "{to}"
                    );
                }
                let pending = root.join("pending-move.json");
                assert!(!pending.exists());

                let to = root.join("新しい場所");
                let resp = c
                    .call(json!({"id": 5, "cmd": "library.move", "to": to}))
                    .await;
                let replied = tokio::time::Instant::now();
                assert_eq!(resp["result"], json!({"restarting": true}));
                let request: PendingMove =
                    serde_json::from_str(&std::fs::read_to_string(&pending).unwrap()).unwrap();
                assert_eq!(
                    request,
                    PendingMove {
                        from: cache.clone(),
                        to
                    }
                );

                // The service shuts down by itself; launchd restarts it.
                let task = svc.task.take().unwrap();
                tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
                assert!(replied.elapsed() >= Duration::from_millis(250));
                assert!(!svc.sock.exists());
                assert!(cache.is_dir());
            })
            .await;
    }

    #[tokio::test]
    async fn library_move_waits_for_downloads() {
        const V: &str = "holdeeeeeee";
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut sub = Client::subscribed(&svc.sock).await;
                let mut c = Client::connect(&svc.sock).await;
                c.call(
                    json!({"id": 1, "cmd": "add", "url": format!("https://youtu.be/{V}"),
                    "album": null, "play": false}),
                )
                .await;
                sub.until(|ev| is_download(ev, V, "downloading")).await;

                let to = svc.dir.path().join("moved");
                let resp = c
                    .call(json!({"id": 2, "cmd": "library.move", "to": to}))
                    .await;
                assert_eq!(
                    resp["error"],
                    json!({"code": "conflict",
                        "message": "Wait for downloads to finish before moving your music folder."})
                );
                assert!(!svc.dir.path().join("pending-move.json").exists());
                svc.release(V);
                sub.until(|ev| is_download(ev, V, "done")).await;
                svc.stop().await;
            })
            .await;
    }

    #[test]
    fn startup_move_renames_the_folder_and_rewrites_the_db() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        let db = seed(&config);
        let a_path = db.get_track(A).unwrap().unwrap().audio_path.unwrap();
        assert!(db.set_loudness(A, &a_path, Some(lvl(-9.0, -0.5))).unwrap());
        drop(db);
        let file = config.config_file().unwrap();
        std::fs::write(&file, "# mine\nworker_python = \"/py\"\n").unwrap();
        let from = config.cache_dir.clone();
        let to = dir.path().join("ミュージック");
        let pending = config.pending_move_file().unwrap();
        PendingMove {
            from: from.clone(),
            to: to.clone(),
        }
        .save(&pending)
        .unwrap();

        let outcome = take_pending_move(&mut config).unwrap();
        assert_eq!(
            outcome,
            MoveOutcome::Moved {
                from: from.clone(),
                to: to.clone()
            }
        );
        assert!(!pending.exists());
        assert!(!from.exists());
        let a_folder = to.join(format!("Song {A} [aaaaaaaa]"));
        assert!(a_folder.join("audio.opus").is_file());
        assert_eq!(config.cache_dir, to);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.starts_with("# mine\nworker_python = \"/py\"\n"),
            "{text}"
        );
        assert_eq!(
            FileConfig::load_from(&file).cache_dir,
            Some(to.to_string_lossy().into_owned())
        );

        let (db, rebuilt) = open_db(&config).unwrap();
        assert!(!rebuilt);
        assert_eq!(
            finish_move(&db, &outcome),
            (
                Severity::Info,
                format!("Moved your music folder to {}.", to.display())
            )
        );
        assert_eq!(
            db.get_track(A).unwrap().unwrap().audio_path,
            Some(a_folder.join("audio.opus").to_string_lossy().into_owned())
        );
        assert_eq!(db.measured().unwrap(), [(A.to_string(), lvl(-9.0, -0.5))]);
        assert_eq!(take_pending_move(&mut config), None);
    }

    #[test]
    fn startup_move_that_cannot_happen_is_dropped_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        std::fs::create_dir_all(&config.cache_dir).unwrap();
        let from = config.cache_dir.clone();
        let to = dir.path().join("taken");
        std::fs::create_dir(&to).unwrap();
        let pending = config.pending_move_file().unwrap();
        PendingMove {
            from: from.clone(),
            to: to.clone(),
        }
        .save(&pending)
        .unwrap();

        let outcome = take_pending_move(&mut config).unwrap();
        let reason = format!("{} already exists", to.display());
        assert_eq!(outcome, MoveOutcome::Failed(reason.clone()));
        assert!(!pending.exists());
        assert!(from.is_dir());
        assert_eq!(config.cache_dir, from);
        assert!(!dir.path().join("config.toml").exists());
        let db = Db::open(&config.db_path(), &config.cache_dir).unwrap();
        assert_eq!(
            finish_move(&db, &outcome),
            (
                Severity::Warn,
                format!("Couldn't move your music folder: {reason}.")
            )
        );

        // A request for another folder and an unreadable file are dropped too.
        let elsewhere = PendingMove {
            from: dir.path().join("elsewhere"),
            to: dir.path().join("new"),
        };
        elsewhere.save(&pending).unwrap();
        assert!(matches!(
            take_pending_move(&mut config),
            Some(MoveOutcome::Failed(_))
        ));
        std::fs::write(&pending, "{not json").unwrap();
        assert!(matches!(
            take_pending_move(&mut config),
            Some(MoveOutcome::Failed(_))
        ));
        assert!(!pending.exists());
        assert!(!dir.path().join("new").exists());
        assert_eq!(config.cache_dir, from);
    }

    // Moves a real folder into the user's Trash, so it only runs on explicit
    // request: `YPLAY_TEST_TRASH=1 cargo test -- --ignored delete_to_trash`
    // (a plain `--ignored` run, e.g. `just e2e`, skips it).
    #[tokio::test]
    #[ignore]
    async fn delete_to_trash_moves_folder_to_trash() {
        if std::env::var_os("YPLAY_TEST_TRASH").is_none() {
            eprintln!("skipped: set YPLAY_TEST_TRASH=1 to move a test folder to the Trash");
            return;
        }
        LocalSet::new()
            .run_until(async {
                let mut svc = start_with_worker().await;
                let mut c = Client::connect(&svc.sock).await;
                let b_dir = svc.cache().join(format!("Song {B} [bbbbbbbb]"));
                assert!(b_dir.is_dir());

                let resp = c
                    .call(json!({"id": 1, "cmd": "track.delete", "track_id": B}))
                    .await;
                assert_eq!(resp, json!({"id": 1, "ok": true, "result": {}}));
                assert!(!b_dir.exists());
                let lib = c.call(json!({"id": 2, "cmd": "library.get"})).await;
                assert!(track(&lib, B).is_none());
                svc.stop().await;
            })
            .await;
    }
}
