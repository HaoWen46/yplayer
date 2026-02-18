use anyhow::{Context, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct MpvPlayer {
    process: Option<Child>,
    socket_path: PathBuf,
    stream: Option<UnixStream>,
    pub is_playing: bool,
    pub is_paused: bool,
    pub position: f64,
    pub duration: f64,
    pub volume: f64,
    current_file: Option<String>,
    event_rx: Option<mpsc::UnboundedReceiver<MpvEvent>>,
}

#[derive(Debug, Clone)]
pub enum MpvEvent {
    PropertyChange { name: String, data: Value },
    EndFile,
}

impl MpvPlayer {
    pub fn new() -> Self {
        let socket_path = std::env::temp_dir().join(format!("yplayer_mpv_{}", std::process::id()));
        Self {
            process: None,
            socket_path,
            stream: None,
            is_playing: false,
            is_paused: false,
            position: 0.0,
            duration: 0.0,
            volume: 100.0,
            current_file: None,
            event_rx: None,
        }
    }

    pub async fn play(&mut self, filepath: &str, volume: Option<f64>) -> Result<()> {
        self.stop().await;

        let vol = volume.map(|v| (v * 100.0).clamp(0.0, 100.0) as u32).unwrap_or(100);
        self.volume = vol as f64;

        // Clean up old socket
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
        self.current_file = Some(filepath.to_string());
        self.is_playing = true;
        self.is_paused = false;
        self.position = 0.0;

        // Wait for socket to appear
        for _ in 0..20 {
            if self.socket_path.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }

        // Connect
        if let Ok(stream) = UnixStream::connect(&self.socket_path).await {
            self.stream = Some(stream);
            // Observe properties for real-time updates
            self.send_command(&["observe_property", "1", "time-pos"]).await.ok();
            self.send_command(&["observe_property", "2", "pause"]).await.ok();
            self.send_command(&["observe_property", "3", "duration"]).await.ok();
        }

        Ok(())
    }

    pub async fn load_file(&mut self, filepath: &str) -> Result<()> {
        self.current_file = Some(filepath.to_string());
        self.is_playing = true;
        self.is_paused = false;
        self.position = 0.0;
        self.send_command(&["loadfile", filepath, "replace"]).await
    }

    pub async fn pause_resume(&mut self) -> Result<()> {
        let new_state = !self.is_paused;
        let _val = if new_state { "yes" } else { "no" };
        self.send_command_json(&serde_json::json!({
            "command": ["set_property", "pause", new_state]
        }))
        .await?;
        self.is_paused = new_state;
        Ok(())
    }

    pub async fn stop(&mut self) {
        if let Some(ref mut stream) = self.stream {
            let msg = serde_json::json!({"command": ["quit"]}).to_string() + "\n";
            let _ = stream.write_all(msg.as_bytes()).await;
        }
        self.stream = None;

        if let Some(mut child) = self.process.take() {
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                child.wait(),
            )
            .await;
            let _ = child.kill().await;
        }

        let _ = std::fs::remove_file(&self.socket_path);
        self.is_playing = false;
        self.is_paused = false;
        self.position = 0.0;
        self.duration = 0.0;
        self.current_file = None;
    }

    pub async fn seek(&mut self, offset: f64) -> Result<()> {
        self.send_command_json(&serde_json::json!({
            "command": ["seek", offset, "relative"]
        }))
        .await
    }

    pub async fn set_volume(&mut self, vol: f64) -> Result<()> {
        self.volume = vol.clamp(0.0, 100.0);
        self.send_command_json(&serde_json::json!({
            "command": ["set_property", "volume", self.volume]
        }))
        .await
    }

    /// Poll mpv for position/duration updates. Call this in the event loop.
    pub async fn poll_status(&mut self) {
        // Check if process is still alive
        if let Some(ref mut child) = self.process {
            match child.try_wait() {
                Ok(Some(_)) => {
                    // Process exited
                    self.is_playing = false;
                    self.is_paused = false;
                    self.process = None;
                    self.stream = None;
                    let _ = std::fs::remove_file(&self.socket_path);
                    return;
                }
                Ok(None) => {} // still running
                Err(_) => {}
            }
        } else {
            self.is_playing = false;
            return;
        }

        // Read property values if connected
        if self.stream.is_none() {
            return;
        }

        // Query time-pos and duration
        if let Ok(val) = self.get_property("time-pos").await {
            if let Some(pos) = val.as_f64() {
                self.position = pos;
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

    async fn get_property(&mut self, name: &str) -> Result<Value> {
        let stream = self.stream.as_mut().context("Not connected to mpv")?;

        let msg = serde_json::json!({"command": ["get_property", name]}).to_string() + "\n";
        stream.write_all(msg.as_bytes()).await?;

        let mut buf = vec![0u8; 4096];
        let n = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            stream.read(&mut buf),
        )
        .await??;

        if n == 0 {
            anyhow::bail!("mpv socket closed");
        }

        let response: Value = serde_json::from_slice(&buf[..n])?;
        Ok(response.get("data").cloned().unwrap_or(Value::Null))
    }

    async fn send_command(&mut self, args: &[&str]) -> Result<()> {
        let cmd = serde_json::json!({"command": args});
        self.send_command_json(&cmd).await
    }

    async fn send_command_json(&mut self, cmd: &Value) -> Result<()> {
        let stream = self.stream.as_mut().context("Not connected to mpv")?;
        let msg = cmd.to_string() + "\n";
        stream.write_all(msg.as_bytes()).await?;
        Ok(())
    }

    pub fn is_playing(&self) -> bool {
        self.is_playing && self.process.is_some()
    }

    pub async fn wait_until_done(&mut self) {
        if let Some(ref mut child) = self.process {
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

use tokio::io::AsyncReadExt;
