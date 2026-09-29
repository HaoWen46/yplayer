use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);

/// Properties observed on every process; `time-pos` is deliberately absent.
const OBSERVED: [&str; 6] = [
    "pause",
    "duration",
    "volume",
    "idle-active",
    "path",
    "playlist-pos",
];

#[derive(Debug, Clone)]
pub struct MpvOptions {
    pub socket_path: PathBuf,
    pub ao: Option<String>,
    pub volume: f64,
    pub extra_args: Vec<String>,
}

/// An mpv event, delivered with the generation of the process that sent it.
#[derive(Debug, Clone, PartialEq)]
pub enum MpvEvent {
    PropertyChange {
        name: String,
        data: Value,
    },
    EndFile {
        reason: String,
    },
    FileLoaded,
    PlaybackRestart,
    Seek,
    /// The IPC connection closed: the process quit or died.
    Exited,
}

/// A command got no reply within `COMMAND_TIMEOUT`; carries the command name.
#[derive(Debug)]
pub struct CommandTimeout(pub String);

impl std::fmt::Display for CommandTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "mpv {} timed out", self.0)
    }
}

impl std::error::Error for CommandTimeout {}

#[allow(async_fn_in_trait)]
pub trait MpvApi {
    /// Errors with `CommandTimeout` when mpv does not answer in time.
    async fn command(&mut self, args: Vec<Value>) -> Result<Value>;
    async fn quit(&mut self);
    /// SIGKILL, for a process that stopped answering.
    async fn kill(&mut self);
    fn alive(&self) -> bool;
}

#[allow(async_fn_in_trait)]
pub trait MpvSpawner {
    type Mpv: MpvApi;
    async fn spawn(
        &self,
        volume: f64,
        generation: u64,
        events: mpsc::UnboundedSender<(u64, MpvEvent)>,
    ) -> Result<Self::Mpv>;
}

/// One newline-delimited message from mpv's JSON IPC.
#[derive(Debug, PartialEq)]
pub enum MpvLine {
    Reply {
        request_id: u64,
        error: String,
        data: Value,
    },
    Event(MpvEvent),
    /// Unparseable, or an event nothing consumes.
    Other,
}

/// Classify one line of mpv IPC output. Pure, so it can be unit-tested with
/// canned payloads — the old code read a whole 4KB chunk and parsed it as a
/// single JSON document, which broke whenever several messages were queued.
pub fn classify_mpv_line(line: &str) -> MpvLine {
    let v: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return MpvLine::Other,
    };
    if let Some(request_id) = v.get("request_id").and_then(|x| x.as_u64()) {
        return MpvLine::Reply {
            request_id,
            error: v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("")
                .to_string(),
            data: v.get("data").cloned().unwrap_or(Value::Null),
        };
    }
    let str_field = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let event = match v.get("event").and_then(|e| e.as_str()) {
        Some("property-change") => MpvEvent::PropertyChange {
            name: str_field("name"),
            data: v.get("data").cloned().unwrap_or(Value::Null),
        },
        Some("end-file") => MpvEvent::EndFile {
            reason: str_field("reason"),
        },
        Some("file-loaded") => MpvEvent::FileLoaded,
        Some("playback-restart") => MpvEvent::PlaybackRestart,
        Some("seek") => MpvEvent::Seek,
        _ => return MpvLine::Other,
    };
    MpvLine::Event(event)
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A persistent `mpv --idle` process driven over its JSON IPC socket.
#[derive(Debug)]
pub struct MpvProcess {
    child: Child,
    writer: OwnedWriteHalf,
    pending: Pending,
    alive: Arc<AtomicBool>,
    next_id: u64,
    socket_path: PathBuf,
}

impl MpvProcess {
    pub async fn spawn(
        opts: &MpvOptions,
        generation: u64,
        events: mpsc::UnboundedSender<(u64, MpvEvent)>,
    ) -> Result<MpvProcess> {
        quit_orphan(&opts.socket_path).await;

        let mut cmd = Command::new("mpv");
        cmd.arg("--no-config")
            .arg("--load-scripts=no")
            .arg("--ytdl=no")
            .arg("--idle=yes")
            .arg("--no-video")
            .arg("--no-terminal")
            .arg(format!("--input-ipc-server={}", opts.socket_path.display()))
            .arg("--input-media-keys=no")
            // the menu-bar app owns Now Playing; mpv's own entry would steal the media keys.
            .arg("--media-controls=no")
            .arg("--demuxer-max-bytes=32MiB")
            .arg("--demuxer-max-back-bytes=8MiB")
            .arg("--prefetch-playlist=yes")
            // coreaudio rejects the default planar float on macOS 27 and mpv falls
            // back to avfoundation, which buffers ~4 s (early handoffs, cut endings).
            .arg("--audio-format=float")
            .arg(format!("--volume={}", opts.volume));
        if let Some(ao) = &opts.ao {
            cmd.arg(format!("--ao={ao}"));
        }
        let child = cmd
            .args(&opts.extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn mpv — is it installed?")?;

        // Connect with a retry loop: the socket file existing does not mean mpv
        // is accepting connections yet. The window is generous (~3s) because a
        // cold first-ever mpv launch can be slow to create the socket.
        let mut connected = None;
        for _ in 0..120 {
            if let Ok(stream) = UnixStream::connect(&opts.socket_path).await {
                connected = Some(stream);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let stream = connected.context("mpv IPC socket did not accept connections")?;
        let (rd, writer) = stream.into_split();

        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        {
            let pending = pending.clone();
            let alive = alive.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(rd).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    match classify_mpv_line(&line) {
                        MpvLine::Reply {
                            request_id,
                            error,
                            data,
                        } => {
                            let tx = pending.lock().unwrap().remove(&request_id);
                            if let Some(tx) = tx {
                                let _ = tx.send(if error == "success" {
                                    Ok(data)
                                } else {
                                    Err(error)
                                });
                            }
                        }
                        MpvLine::Event(ev) => {
                            let _ = events.send((generation, ev));
                        }
                        MpvLine::Other => {}
                    }
                }
                // Mark dead before failing the waiters, so a command that
                // registers after the drain sees `alive == false`.
                alive.store(false, Ordering::SeqCst);
                pending.lock().unwrap().clear();
                let _ = events.send((generation, MpvEvent::Exited));
            });
        }

        let mut mpv = MpvProcess {
            child,
            writer,
            pending,
            alive,
            next_id: 1,
            socket_path: opts.socket_path.clone(),
        };
        for (i, name) in OBSERVED.iter().enumerate() {
            mpv.command(vec![json!("observe_property"), json!(i + 1), json!(name)])
                .await?;
        }
        Ok(mpv)
    }
}

/// An mpv still serving `socket_path` was left behind by a killed service:
/// ask it to quit and wait up to 1 s for it to close the connection. Then
/// remove the socket file.
pub async fn quit_orphan(socket_path: &Path) {
    if let Ok(mut stream) = UnixStream::connect(socket_path).await
        && stream
            .write_all(b"{\"command\":[\"quit\"]}\n")
            .await
            .is_ok()
    {
        let mut buf = [0u8; 4096];
        let _ = tokio::time::timeout(Duration::from_secs(1), async {
            while matches!(stream.read(&mut buf).await, Ok(n) if n > 0) {}
        })
        .await;
    }
    let _ = std::fs::remove_file(socket_path);
}

impl MpvApi for MpvProcess {
    async fn command(&mut self, args: Vec<Value>) -> Result<Value> {
        if !self.alive() {
            bail!("mpv is not running");
        }
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if !self.alive() {
            self.pending.lock().unwrap().remove(&id);
            bail!("mpv is not running");
        }
        let name = args
            .first()
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .to_string();
        let msg = json!({"command": args, "request_id": id}).to_string() + "\n";
        if let Err(e) = self.writer.write_all(msg.as_bytes()).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e).context("writing to mpv");
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(Ok(data))) => Ok(data),
            Ok(Ok(Err(error))) => bail!("mpv {name} failed: {error}"),
            Ok(Err(_)) => bail!("mpv exited during {name}"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(CommandTimeout(name).into())
            }
        }
    }

    async fn quit(&mut self) {
        if self.alive() {
            let _ = self.writer.write_all(b"{\"command\":[\"quit\"]}\n").await;
        }
        self.alive.store(false, Ordering::SeqCst);
        if tokio::time::timeout(Duration::from_secs(1), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }

    async fn kill(&mut self) {
        self.alive.store(false, Ordering::SeqCst);
        let _ = self.child.kill().await;
        let _ = std::fs::remove_file(&self.socket_path);
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

/// Spawns `MpvProcess`es from a template; the engine supplies the volume.
#[derive(Debug, Clone)]
pub struct ProcessSpawner {
    pub opts_template: MpvOptions,
}

impl MpvSpawner for ProcessSpawner {
    type Mpv = MpvProcess;

    async fn spawn(
        &self,
        volume: f64,
        generation: u64,
        events: mpsc::UnboundedSender<(u64, MpvEvent)>,
    ) -> Result<MpvProcess> {
        let opts = MpvOptions {
            volume,
            ..self.opts_template.clone()
        };
        MpvProcess::spawn(&opts, generation, events).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_reply_event_and_garbage() {
        match classify_mpv_line(r#"{"request_id":5,"error":"success","data":123.5}"#) {
            MpvLine::Reply {
                request_id,
                error,
                data,
            } => {
                assert_eq!(request_id, 5);
                assert_eq!(error, "success");
                assert_eq!(data.as_f64(), Some(123.5));
            }
            other => panic!("expected reply, got {other:?}"),
        }
        match classify_mpv_line(r#"{"event":"property-change","name":"time-pos","data":12.0}"#) {
            MpvLine::Event(MpvEvent::PropertyChange { name, .. }) => assert_eq!(name, "time-pos"),
            other => panic!("expected event, got {other:?}"),
        }
        match classify_mpv_line(r#"{"event":"end-file","reason":"eof"}"#) {
            MpvLine::Event(MpvEvent::EndFile { reason }) => assert_eq!(reason, "eof"),
            other => panic!("expected end-file event, got {other:?}"),
        }
        assert_eq!(classify_mpv_line("not json"), MpvLine::Other);
        assert_eq!(classify_mpv_line(""), MpvLine::Other);
    }

    #[test]
    fn selects_the_matching_reply_among_interleaved_lines() {
        // The exact shape that broke the old get_property: an event first, then
        // an unrelated command reply, then the reply we actually want.
        let lines = [
            r#"{"event":"property-change","name":"time-pos","data":1.0}"#,
            r#"{"request_id":1,"error":"success","data":null}"#,
            r#"{"request_id":2,"error":"success","data":42.0}"#,
        ];
        let classified: Vec<MpvLine> = lines.iter().map(|l| classify_mpv_line(l)).collect();
        assert!(matches!(classified[0], MpvLine::Event(_)));
        assert!(matches!(
            classified[1],
            MpvLine::Reply { request_id: 1, .. }
        ));
        match &classified[2] {
            MpvLine::Reply {
                request_id: 2,
                data,
                ..
            } => assert_eq!(data.as_f64(), Some(42.0)),
            other => panic!("expected reply id 2, got {other:?}"),
        }
    }

    #[test]
    fn classifies_property_change_with_name_and_data() {
        assert_eq!(
            classify_mpv_line(r#"{"event":"property-change","id":1,"name":"pause","data":true}"#),
            MpvLine::Event(MpvEvent::PropertyChange {
                name: "pause".into(),
                data: json!(true),
            })
        );
        assert_eq!(
            classify_mpv_line(
                r#"{"event":"property-change","id":5,"name":"path","data":"/a/b.opus"}"#
            ),
            MpvLine::Event(MpvEvent::PropertyChange {
                name: "path".into(),
                data: json!("/a/b.opus"),
            })
        );
        // Unavailable property: mpv omits `data`.
        assert_eq!(
            classify_mpv_line(r#"{"event":"property-change","id":2,"name":"duration"}"#),
            MpvLine::Event(MpvEvent::PropertyChange {
                name: "duration".into(),
                data: Value::Null,
            })
        );
    }

    #[test]
    fn classifies_end_file_reason() {
        assert_eq!(
            classify_mpv_line(
                r#"{"event":"end-file","reason":"error","playlist_entry_id":3,"file_error":"loading failed"}"#
            ),
            MpvLine::Event(MpvEvent::EndFile {
                reason: "error".into()
            })
        );
        assert_eq!(
            classify_mpv_line(r#"{"event":"end-file","reason":"stop","playlist_entry_id":1}"#),
            MpvLine::Event(MpvEvent::EndFile {
                reason: "stop".into()
            })
        );
    }

    #[test]
    fn classifies_file_loaded_restart_and_seek() {
        assert_eq!(
            classify_mpv_line(r#"{"event":"file-loaded"}"#),
            MpvLine::Event(MpvEvent::FileLoaded)
        );
        assert_eq!(
            classify_mpv_line(r#"{"event":"playback-restart"}"#),
            MpvLine::Event(MpvEvent::PlaybackRestart)
        );
        assert_eq!(
            classify_mpv_line(r#"{"event":"seek"}"#),
            MpvLine::Event(MpvEvent::Seek)
        );
        // Events nothing consumes are not forwarded.
        assert_eq!(
            classify_mpv_line(r#"{"event":"audio-reconfig"}"#),
            MpvLine::Other
        );
    }

    #[test]
    fn classifies_error_reply() {
        assert_eq!(
            classify_mpv_line(r#"{"request_id":9,"error":"property unavailable","data":null}"#),
            MpvLine::Reply {
                request_id: 9,
                error: "property unavailable".into(),
                data: Value::Null,
            }
        );
    }

    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    fn test_opts(dir: &tempfile::TempDir) -> MpvOptions {
        MpvOptions {
            socket_path: dir.path().join("mpv.sock"),
            ao: None,
            volume: 50.0,
            extra_args: vec!["--ao=null".into()],
        }
    }

    /// Next event from the channel, bounded so a broken mpv fails the test.
    async fn next_event(rx: &mut mpsc::UnboundedReceiver<(u64, MpvEvent)>) -> (u64, MpvEvent) {
        tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for an mpv event")
            .expect("event channel closed")
    }

    #[tokio::test]
    #[ignore = "needs real mpv"]
    async fn real_mpv_spawn_is_idle() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut mpv = MpvProcess::spawn(&test_opts(&dir), 1, tx).await.unwrap();
        assert!(mpv.alive());
        let idle = mpv
            .command(vec![json!("get_property"), json!("idle-active")])
            .await
            .unwrap();
        assert_eq!(idle, json!(true));
        mpv.quit().await;
    }

    #[tokio::test]
    #[ignore = "needs real mpv"]
    async fn real_mpv_loadfile_emits_file_loaded_and_duration() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut mpv = MpvProcess::spawn(&test_opts(&dir), 7, tx).await.unwrap();
        mpv.command(vec![json!("loadfile"), json!(fixture("tone.opus"))])
            .await
            .unwrap();
        let (mut loaded, mut duration) = (false, None);
        while !loaded || duration.is_none() {
            let (generation, ev) = next_event(&mut rx).await;
            assert_eq!(generation, 7);
            match ev {
                MpvEvent::FileLoaded => loaded = true,
                MpvEvent::PropertyChange { name, data } if name == "duration" => {
                    duration = data.as_f64();
                }
                _ => {}
            }
        }
        let d = duration.unwrap();
        assert!((d - 4.0).abs() < 0.1, "duration {d}");
        mpv.quit().await;
    }

    #[tokio::test]
    #[ignore = "needs real mpv"]
    async fn real_mpv_quit_emits_exited() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut mpv = MpvProcess::spawn(&test_opts(&dir), 3, tx).await.unwrap();
        mpv.quit().await;
        assert!(!mpv.alive());
        loop {
            if let (3, MpvEvent::Exited) = next_event(&mut rx).await {
                break;
            }
        }
    }

    #[tokio::test]
    #[ignore = "needs real mpv"]
    async fn real_mpv_spawn_quits_an_orphan_on_the_socket() {
        let dir = tempfile::Builder::new()
            .prefix("yphB")
            .tempdir_in("/tmp")
            .unwrap();
        let opts = test_opts(&dir);
        // An mpv left behind by a killed service, still serving the socket.
        let mut orphan = std::process::Command::new("mpv")
            .args(["--no-config", "--idle=yes", "--no-video", "--no-terminal"])
            .arg("--ao=null")
            .arg(format!("--input-ipc-server={}", opts.socket_path.display()))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::os::unix::net::UnixStream::connect(&opts.socket_path).is_err() {
            assert!(
                std::time::Instant::now() < deadline,
                "orphan never listened"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let (tx, _rx) = mpsc::unbounded_channel();
        let mut mpv = MpvProcess::spawn(&opts, 1, tx).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while orphan.try_wait().unwrap().is_none() {
            if std::time::Instant::now() >= deadline {
                let _ = orphan.kill();
                let _ = orphan.wait();
                mpv.quit().await;
                panic!("the orphan mpv is still running");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let idle = mpv
            .command(vec![json!("get_property"), json!("idle-active")])
            .await
            .unwrap();
        assert_eq!(idle, json!(true));
        mpv.quit().await;
    }

    #[tokio::test]
    #[ignore = "needs real mpv"]
    async fn real_mpv_command_after_exit_errors_quickly() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut mpv = MpvProcess::spawn(&test_opts(&dir), 1, tx).await.unwrap();
        mpv.quit().await;
        let started = std::time::Instant::now();
        let res = mpv
            .command(vec![json!("get_property"), json!("idle-active")])
            .await;
        assert!(res.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
