//! Persistent, supervised worker actor.
//!
//! One long-lived Python worker is owned by a background task and reused for
//! every request (amortizing interpreter + yt-dlp startup). Requests carry a
//! oneshot reply channel; each is bounded by a timeout, and the worker is
//! respawned on transport failure or timeout while application errors (e.g.
//! "video unavailable") leave it running.

use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

use crate::config::Config;
use crate::download::bridge::Bridge;
use crate::types::Track;

/// A request to the worker actor. The `String` error is already display-ready.
enum WorkerRequest {
    Download {
        url: String,
        reply: oneshot::Sender<Result<Track, String>>,
    },
    Lyrics {
        track_name: String,
        artist_name: Option<String>,
        duration: Option<i64>,
        reply: oneshot::Sender<Result<Option<String>, String>>,
    },
}

/// Cloneable handle the app uses to talk to the worker actor.
#[derive(Clone)]
pub struct WorkerHandle {
    tx: mpsc::UnboundedSender<WorkerRequest>,
}

impl WorkerHandle {
    /// Spawn the actor task and return a handle to it. Must be called from
    /// within a Tokio runtime.
    pub fn spawn(cfg: Config) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(actor(cfg, rx));
        Self { tx }
    }

    /// Download `url`, returning the resulting track or a display-ready error.
    pub async fn download(&self, url: String) -> Result<Track, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(WorkerRequest::Download { url, reply })
            .map_err(|_| "worker actor is gone".to_string())?;
        rx.await
            .map_err(|_| "worker dropped the request".to_string())?
    }

    /// Fetch synced lyrics for a track. Ok(None) means "none found".
    pub async fn lyrics(
        &self,
        track_name: String,
        artist_name: Option<String>,
        duration: Option<i64>,
    ) -> Result<Option<String>, String> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(WorkerRequest::Lyrics {
                track_name,
                artist_name,
                duration,
                reply,
            })
            .map_err(|_| "worker actor is gone".to_string())?;
        rx.await
            .map_err(|_| "worker dropped the request".to_string())?
    }
}

/// Downloads can be slow, but not unbounded — a wedged worker must not hang forever.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Lyrics fetch runs on its own worker. Must exceed the Python side's worst
/// case (up to 3 LRCLIB calls at 6s each) so a slow LRCLIB doesn't trip the
/// actor timeout and needlessly respawn the worker.
const LYRICS_TIMEOUT: Duration = Duration::from_secs(25);

async fn actor(cfg: Config, mut rx: mpsc::UnboundedReceiver<WorkerRequest>) {
    let mut bridge: Option<Bridge> = None;

    while let Some(req) = rx.recv().await {
        match req {
            WorkerRequest::Download { url, reply } => {
                // Lazily (re)spawn the worker.
                if bridge.is_none() {
                    match Bridge::new(&cfg).await {
                        Ok(b) => bridge = Some(b),
                        Err(e) => {
                            let _ = reply.send(Err(format!("worker start failed: {e}")));
                            continue;
                        }
                    }
                }
                let b = bridge.as_mut().unwrap();

                let outcome = tokio::time::timeout(DOWNLOAD_TIMEOUT, b.download(&url, &cfg)).await;
                match outcome {
                    Ok(Ok(dl)) => {
                        let _ = reply.send(Ok(dl.track));
                    }
                    Ok(Err(fail)) => {
                        // Transport failures mean the worker is unhealthy; drop it
                        // so the next request respawns. App errors keep it alive.
                        if fail.is_transport() {
                            bridge = None;
                        }
                        let _ = reply.send(Err(fail.to_string()));
                    }
                    Err(_elapsed) => {
                        bridge = None;
                        let _ = reply.send(Err(format!(
                            "download timed out after {}s",
                            DOWNLOAD_TIMEOUT.as_secs()
                        )));
                    }
                }
            }
            WorkerRequest::Lyrics {
                track_name,
                artist_name,
                duration,
                reply,
            } => {
                if bridge.is_none() {
                    match Bridge::new(&cfg).await {
                        Ok(b) => bridge = Some(b),
                        Err(e) => {
                            let _ = reply.send(Err(format!("worker start failed: {e}")));
                            continue;
                        }
                    }
                }
                let b = bridge.as_mut().unwrap();

                let outcome = tokio::time::timeout(
                    LYRICS_TIMEOUT,
                    b.lyrics(&track_name, artist_name.as_deref(), duration),
                )
                .await;
                match outcome {
                    Ok(Ok(lrc)) => {
                        let _ = reply.send(Ok(lrc));
                    }
                    Ok(Err(fail)) => {
                        if fail.is_transport() {
                            bridge = None;
                        }
                        let _ = reply.send(Err(fail.to_string()));
                    }
                    Err(_elapsed) => {
                        bridge = None;
                        let _ = reply.send(Err("lyrics request timed out".to_string()));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_OK: &str = r#"
import sys, json
sys.stdout.write(json.dumps({"event": "ready", "ok": True}) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    sys.stdout.write(json.dumps({"id": req.get("id"), "ok": True, "path": "/tmp/audio.mp3", "meta": {"id": "abc123", "title": "Fake Song"}}) + "\n")
    sys.stdout.flush()
"#;

    const FAKE_ERR: &str = r#"
import sys, json
sys.stdout.write(json.dumps({"event": "ready", "ok": True}) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    sys.stdout.write(json.dumps({"id": req.get("id"), "ok": False, "error": "video unavailable"}) + "\n")
    sys.stdout.flush()
"#;

    fn fake_cmd(dir: &std::path::Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        // set_var is unsafe on edition 2024 (not thread-safe); this test is
        // single-threaded and the only consumer of YPLAY_WORKER_CMD.
        unsafe {
            std::env::set_var("YPLAY_WORKER_CMD", format!("python3 {}", path.display()));
        }
    }

    // One serial test to avoid racing on the process-global YPLAY_WORKER_CMD.
    #[tokio::test]
    async fn actor_roundtrips_and_propagates_errors() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::new(Some(dir.path().to_string_lossy().to_string()), None);

        // Happy path: handshake + id-correlated download reply -> Track.
        fake_cmd(dir.path(), "ok.py", FAKE_OK);
        let track = WorkerHandle::spawn(cfg.clone())
            .download("https://youtu.be/abc".to_string())
            .await
            .expect("download should succeed");
        assert_eq!(track.id, "abc123");
        assert_eq!(track.title, "Fake Song");

        // Application error (ok:false) is propagated verbatim, worker kept alive.
        fake_cmd(dir.path(), "err.py", FAKE_ERR);
        let err = WorkerHandle::spawn(cfg)
            .download("https://youtu.be/xyz".to_string())
            .await
            .expect_err("worker reported an error");
        assert!(err.contains("video unavailable"), "got: {err}");

        unsafe {
            std::env::remove_var("YPLAY_WORKER_CMD");
        }
    }
}
