pub mod conn;
pub mod core;

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
    std::fs::create_dir_all(&state_dir)
        .with_context(|| format!("creating {}", state_dir.display()))?;
    std::fs::set_permissions(&state_dir, std::fs::Permissions::from_mode(0o700))?;
    let listener = bind_socket(&socket_path).await?;
    let db = Db::open(&config.db_path(), &config.cache_dir)?;
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
        log_path: Config::worker_log_path(&state_dir),
        idle_timeout: WORKER_IDLE,
        inactivity_timeout: WORKER_INACTIVITY,
    });
    let http: Arc<dyn HttpGet> = Arc::new(CurlHttp {
        user_agent: USER_AGENT.to_string(),
    });
    let updater = worker_command(config.worker_python.as_deref())
        .ok()
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

    fn new_core(dir: &Path, fake: &FakeSpawner, capacity: usize) -> (Core<FakeSpawner>, Inbox) {
        let config = config(dir);
        let db = seed(&config);
        let worker = WorkerHandle::spawn(WorkerOptions {
            worker_python: None,
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
        let fake = FakeSpawner::default();
        let listener = bind_socket(&sock).await.unwrap();
        let (mut core, inbox) = new_core(dir.path(), &fake, capacity);
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

                // `sub` reads nothing while ~1.6 MB of events are produced.
                let mut c = Client::connect(&svc.sock).await;
                let pad = "x".repeat(4000);
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
        let (mut core, _inbox) = new_core(dir.path(), &fake, EVENT_CAPACITY);
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
}
