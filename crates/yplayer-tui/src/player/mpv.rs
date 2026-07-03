use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};

#[derive(Debug)]
pub struct MpvPlayer {
    process: Option<Child>,
    socket_path: PathBuf,
    reader: Option<BufReader<OwnedReadHalf>>,
    writer: Option<OwnedWriteHalf>,
    next_id: u64,
    pub is_playing: bool,
    pub is_paused: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f64,
}

/// One newline-delimited message from mpv's JSON IPC.
#[derive(Debug, PartialEq)]
pub enum MpvLine {
    Reply {
        request_id: u64,
        error: String,
        data: Value,
    },
    Event {
        name: String,
        reason: Option<String>,
    },
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
    if let Some(name) = v.get("event").and_then(|e| e.as_str()) {
        return MpvLine::Event {
            name: name.to_string(),
            reason: v.get("reason").and_then(|r| r.as_str()).map(String::from),
        };
    }
    MpvLine::Other
}

impl MpvPlayer {
    pub fn new() -> Self {
        let socket_path = std::env::temp_dir().join(format!("yplayer_mpv_{}", std::process::id()));
        Self {
            process: None,
            socket_path,
            reader: None,
            writer: None,
            next_id: 1,
            is_playing: false,
            is_paused: false,
            position: 0.0,
            duration: 0.0,
            volume: 100.0,
        }
    }

    pub async fn play(&mut self, filepath: &str, volume: Option<f64>) -> Result<()> {
        self.stop().await;

        let vol = volume
            .map(|v| (v * 100.0).clamp(0.0, 100.0) as u32)
            .unwrap_or(100);
        self.volume = vol as f64;

        let _ = std::fs::remove_file(&self.socket_path);

        let child = Command::new("mpv")
            .arg("--no-video")
            .arg("--idle=no")
            .arg("--keep-open=no")
            .arg(format!("--input-ipc-server={}", self.socket_path.display()))
            .arg(format!("--volume={}", vol))
            .arg("--")
            .arg(filepath)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("Failed to spawn mpv — is it installed?")?;

        self.process = Some(child);
        self.is_playing = true;
        self.is_paused = false;
        self.position = 0.0;
        self.duration = 0.0;

        // Connect with a short retry loop: the socket file existing does not
        // mean mpv is accepting connections yet.
        let mut connected = None;
        for _ in 0..40 {
            if let Ok(stream) = UnixStream::connect(&self.socket_path).await {
                connected = Some(stream);
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        if let Some(stream) = connected {
            let (rd, wr) = stream.into_split();
            self.reader = Some(BufReader::new(rd));
            self.writer = Some(wr);
        } else {
            // Playback still works; we just won't have IPC control this track.
            self.reader = None;
            self.writer = None;
        }
        Ok(())
    }

    pub async fn pause_resume(&mut self) -> Result<()> {
        let new_state = !self.is_paused;
        self.write_command(&serde_json::json!({
            "command": ["set_property", "pause", new_state]
        }))
        .await?;
        self.is_paused = new_state;
        Ok(())
    }

    pub async fn stop(&mut self) {
        // Ask mpv to quit only if we actually have a live connection.
        let sent_quit = if let Some(writer) = self.writer.as_mut() {
            let msg = serde_json::json!({"command": ["quit"]}).to_string() + "\n";
            writer.write_all(msg.as_bytes()).await.is_ok()
        } else {
            false
        };
        self.reader = None;
        self.writer = None;

        if let Some(mut child) = self.process.take() {
            // Only wait for a clean exit if we managed to send quit; otherwise
            // don't burn the timeout — kill immediately.
            if sent_quit {
                let _ = tokio::time::timeout(Duration::from_millis(500), child.wait()).await;
            }
            let _ = child.kill().await;
        }

        let _ = std::fs::remove_file(&self.socket_path);
        self.is_playing = false;
        self.is_paused = false;
        self.position = 0.0;
        self.duration = 0.0;
    }

    pub async fn seek(&mut self, offset: f64) -> Result<()> {
        self.write_command(&serde_json::json!({
            "command": ["seek", offset, "relative"]
        }))
        .await
    }

    pub async fn set_volume(&mut self, vol: f64) -> Result<()> {
        self.volume = vol.clamp(0.0, 100.0);
        self.write_command(&serde_json::json!({
            "command": ["set_property", "volume", self.volume]
        }))
        .await
    }

    /// Poll mpv for position/pause/duration. Called from the event loop tick.
    pub async fn poll_status(&mut self) {
        // Detect process exit (end of track or crash).
        if let Some(child) = self.process.as_mut() {
            if let Ok(Some(_)) = child.try_wait() {
                self.on_exit();
                return;
            }
        } else {
            self.is_playing = false;
            return;
        }

        if self.reader.is_none() {
            return;
        }

        if let Ok(val) = self.get_property("time-pos").await {
            if let Some(pos) = val.as_f64() {
                self.position = pos;
            }
        }
        if let Ok(val) = self.get_property("pause").await {
            if let Some(paused) = val.as_bool() {
                self.is_paused = paused;
            }
        }
        if self.duration <= 0.0 {
            if let Ok(val) = self.get_property("duration").await {
                if let Some(dur) = val.as_f64() {
                    self.duration = dur;
                }
            }
        }
    }

    fn on_exit(&mut self) {
        self.is_playing = false;
        self.is_paused = false;
        self.process = None;
        self.reader = None;
        self.writer = None;
        let _ = std::fs::remove_file(&self.socket_path);
    }

    /// Query a property, correlating the reply by request_id and skipping any
    /// interleaved event / other-command lines. Bounded by an overall deadline.
    async fn get_property(&mut self, name: &str) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_command(&serde_json::json!({
            "command": ["get_property", name],
            "request_id": id,
        }))
        .await?;

        let reader = self.reader.as_mut().context("not connected to mpv")?;
        let deadline = Instant::now() + Duration::from_millis(200);
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .context("timed out waiting for mpv reply")?;
            let mut line = String::new();
            let n = tokio::time::timeout(remaining, reader.read_line(&mut line)).await??;
            if n == 0 {
                bail!("mpv socket closed");
            }
            match classify_mpv_line(&line) {
                MpvLine::Reply {
                    request_id,
                    error,
                    data,
                } if request_id == id => {
                    if error != "success" {
                        bail!("mpv get_property {name} failed: {error}");
                    }
                    return Ok(data);
                }
                // Skip replies to fire-and-forget commands and any events.
                _ => continue,
            }
        }
    }

    async fn write_command(&mut self, cmd: &Value) -> Result<()> {
        let writer = self.writer.as_mut().context("not connected to mpv")?;
        let msg = cmd.to_string() + "\n";
        writer.write_all(msg.as_bytes()).await?;
        Ok(())
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing && self.process.is_some()
    }

    pub async fn wait_until_done(&mut self) {
        if let Some(child) = self.process.as_mut() {
            let _ = child.wait().await;
        }
        self.is_playing = false;
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            MpvLine::Event { name, .. } => assert_eq!(name, "property-change"),
            other => panic!("expected event, got {other:?}"),
        }
        match classify_mpv_line(r#"{"event":"end-file","reason":"eof"}"#) {
            MpvLine::Event { name, reason } => {
                assert_eq!(name, "end-file");
                assert_eq!(reason.as_deref(), Some("eof"));
            }
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
        assert!(matches!(classified[0], MpvLine::Event { .. }));
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
}
