use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

use crate::config::Config;
use crate::types::Track;

#[derive(Debug, Serialize)]
struct WorkerCommand {
    cmd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    native: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    embed_meta: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct WorkerResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub meta: Option<Value>,
    #[serde(default)]
    pub results: Option<Vec<Value>>,
    #[serde(default)]
    pub entries: Option<Vec<Value>>,
    #[serde(default)]
    pub formats: Option<Vec<Value>>,
}

#[derive(Debug)]
pub struct DownloadResult {
    pub path: String,
    pub track: Track,
}

pub struct Bridge {
    child: Child,
    stdin: tokio::process::ChildStdin,
    reader: BufReader<tokio::process::ChildStdout>,
}

impl Bridge {
    pub async fn new(cfg: &Config) -> Result<Self> {
        // Find Python executable — prefer the venv Python next to the Rust binary
        let python = find_python()?;

        // Send worker stderr (tracebacks, yt-dlp / pip output) to a log file rather
        // than the terminal — inheriting it would corrupt the raw-mode alternate screen.
        let log_path = cfg.cache_dir.join(".worker.log");
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());

        let mut child = Command::new(&python)
            .arg("-m")
            .arg("yplayer.worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .with_context(|| {
                format!(
                    "Failed to spawn Python worker with: {} (stderr log: {})",
                    python,
                    log_path.display()
                )
            })?;

        let stdin = child.stdin.take().context("No stdin on worker")?;
        let stdout = child.stdout.take().context("No stdout on worker")?;
        let reader = BufReader::new(stdout);

        Ok(Self {
            child,
            stdin,
            reader,
        })
    }

    async fn send(&mut self, cmd: WorkerCommand) -> Result<WorkerResponse> {
        let mut line = serde_json::to_string(&cmd)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        let mut response_line = String::new();
        self.reader.read_line(&mut response_line).await?;

        if response_line.is_empty() {
            anyhow::bail!("Python worker closed unexpectedly");
        }

        let resp: WorkerResponse =
            serde_json::from_str(&response_line).context("Invalid JSON from worker")?;

        if !resp.ok {
            let err = resp.error.unwrap_or_else(|| "Unknown error".to_string());
            anyhow::bail!("Worker error: {}", err);
        }

        Ok(resp)
    }

    pub async fn download(&mut self, url: &str, cfg: &Config) -> Result<DownloadResult> {
        let resp = self
            .send(WorkerCommand {
                cmd: "download".to_string(),
                url: Some(url.to_string()),
                cache_dir: Some(cfg.cache_dir.to_string_lossy().to_string()),
                format: Some(cfg.format.clone()),
                api_key: cfg.api_key.clone(),
                native: Some(cfg.native),
                embed_meta: Some(cfg.embed_meta),
                query: None,
                limit: None,
            })
            .await?;

        let path = resp.path.context("No path in download response")?;
        let meta = resp.meta.unwrap_or(Value::Null);

        let track = Track {
            id: meta
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            title: meta
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown")
                .to_string(),
            uploader: meta.get("uploader").and_then(|v| v.as_str()).map(String::from),
            duration: meta.get("duration").and_then(|v| v.as_i64()),
            webpage_url: meta.get("webpage_url").and_then(|v| v.as_str()).map(String::from),
            audio_path: Some(path.clone()),
            format: Some(cfg.format.clone()),
            file_size: None,
            added_at: None,
            last_played: None,
        };

        Ok(DownloadResult { path, track })
    }

    pub async fn search(&mut self, query: &str, limit: usize, cfg: &Config) -> Result<Vec<Track>> {
        let resp = self
            .send(WorkerCommand {
                cmd: "search".to_string(),
                query: Some(query.to_string()),
                limit: Some(limit),
                api_key: cfg.api_key.clone(),
                url: None,
                cache_dir: None,
                format: None,
                native: None,
                embed_meta: None,
            })
            .await?;

        let results = resp.results.unwrap_or_default();
        Ok(results.into_iter().filter_map(|v| parse_track_from_value(&v)).collect())
    }

    pub async fn video_info(&mut self, url: &str, cfg: &Config) -> Result<Track> {
        let resp = self
            .send(WorkerCommand {
                cmd: "video_info".to_string(),
                url: Some(url.to_string()),
                api_key: cfg.api_key.clone(),
                query: None,
                limit: None,
                cache_dir: None,
                format: None,
                native: None,
                embed_meta: None,
            })
            .await?;

        let meta = resp.meta.context("No meta in video_info response")?;
        parse_track_from_value(&meta).context("Failed to parse track from response")
    }

    pub async fn playlist_entries(&mut self, url: &str) -> Result<Vec<Track>> {
        let resp = self
            .send(WorkerCommand {
                cmd: "playlist_entries".to_string(),
                url: Some(url.to_string()),
                query: None,
                limit: None,
                cache_dir: None,
                format: None,
                api_key: None,
                native: None,
                embed_meta: None,
            })
            .await?;

        let entries = resp.entries.unwrap_or_default();
        Ok(entries.into_iter().filter_map(|v| parse_track_from_value(&v)).collect())
    }

    pub async fn list_formats(&mut self, url: &str) -> Result<Vec<String>> {
        let resp = self
            .send(WorkerCommand {
                cmd: "list_formats".to_string(),
                url: Some(url.to_string()),
                query: None,
                limit: None,
                cache_dir: None,
                format: None,
                api_key: None,
                native: None,
                embed_meta: None,
            })
            .await?;

        let formats = resp
            .formats
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.to_string())
            .collect();
        Ok(formats)
    }
}

fn parse_track_from_value(v: &Value) -> Option<Track> {
    let id = v.get("id")?.as_str()?.to_string();
    Some(Track {
        id,
        title: v
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown")
            .to_string(),
        uploader: v.get("uploader").and_then(|v| v.as_str()).map(String::from),
        duration: v.get("duration").and_then(|v| v.as_i64()),
        webpage_url: v.get("webpage_url").and_then(|v| v.as_str()).map(String::from),
        audio_path: v.get("path").and_then(|v| v.as_str()).map(String::from),
        format: None,
        file_size: None,
        added_at: None,
        last_played: None,
    })
}

fn find_python() -> Result<String> {
    // Try venv Python first (relative to executable or cwd)
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();

    for base in [exe.as_ref().and_then(|p| p.parent().map(|p| p.to_path_buf())), cwd].into_iter().flatten() {
        // Walk up to find .venv
        let mut dir = base.as_path();
        for _ in 0..5 {
            let venv_python = dir.join(".venv/bin/python3");
            if venv_python.exists() {
                return Ok(venv_python.to_string_lossy().to_string());
            }
            let venv_python = dir.join(".venv/bin/python");
            if venv_python.exists() {
                return Ok(venv_python.to_string_lossy().to_string());
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
    }

    // Fall back to system python
    for name in &["python3", "python"] {
        if which::which(name).is_ok() {
            return Ok(name.to_string());
        }
    }

    // Last resort
    Ok("python3".to_string())
}
