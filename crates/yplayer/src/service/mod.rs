pub mod conn;
pub mod core;
pub mod downloads;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;

use self::core::{Core, CoreDeps, Inbox, Request};
use crate::config::Config;
use crate::download::bridge::worker_command;
use crate::download::worker::{WorkerHandle, WorkerOptions};
use crate::http::{CurlHttp, HttpGet};
use crate::library::db::Db;
use crate::library::reconcile::recover_interrupted;
use crate::lyrics::USER_AGENT;
use crate::player::mpv::{MpvOptions, MpvSpawner, ProcessSpawner};
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
        config,
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
    let (db, rebuilt) = open_db(&config)?;
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
    use crate::player::engine::testing::FakeSpawner;
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

    fn config(dir: &Path) -> Config {
        Config::new(Some(dir.join("cache").to_string_lossy().into_owned()), None)
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
        let fake = FakeSpawner::default();
        let listener = bind_socket(&sock).await.unwrap();
        let (mut core, inbox) = new_core(dir.path(), &fake, capacity, worker_cmd);
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
                assert_eq!(
                    svc.fake.commands(),
                    vec![vec![json!("loadfile"), json!(path), json!("replace")]]
                );
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

    fn loadfile(path: &str) -> Vec<Value> {
        vec![json!("loadfile"), json!(path), json!("replace")]
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

    // Moves a real folder into the user's Trash, so it only runs on request
    // (`cargo test -- --ignored delete_to_trash`), never in the default suite.
    #[tokio::test]
    #[ignore]
    async fn delete_to_trash_moves_folder_to_trash() {
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
