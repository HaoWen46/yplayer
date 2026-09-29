use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::bridge::worker_command;
use crate::types::TrackMeta;

pub type JobId = u64;

const PROTOCOL: u64 = 2;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_LINE: usize = 4 << 20;
const BACKGROUND_SLOTS: usize = 3;
/// Concurrent jobs assumed when the worker's handshake does not declare `slots`.
const DEFAULT_SLOTS: usize = 8;

#[derive(Debug, Clone)]
pub struct WorkerOptions {
    pub worker_python: Option<String>,
    /// Exact program + args; overrides `YPLAY_WORKER_CMD` and python discovery.
    pub worker_cmd: Option<Vec<String>>,
    pub log_path: PathBuf,
    pub idle_timeout: Duration,       // 60 s in the service
    pub inactivity_timeout: Duration, // 180 s in the service
}

/// One message of a download job's stream. `Done` and `Failed` are terminal.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerMsg {
    Started {
        path: String,
        dir: String,
        meta: TrackMeta,
    },
    Progress {
        bytes: u64,
        total: Option<u64>,
    },
    Done {
        path: String,
        dir: String,
        meta: TrackMeta,
        thumb: Option<String>,
        format: Option<String>,
        file_size: Option<i64>,
    },
    Failed {
        error: String,
        cancelled: bool,
        transport: bool,
        dir: Option<String>,
    },
}

/// Handle to the actor that owns the Python worker process. The worker is
/// spawned on the first job, runs jobs concurrently (protocol v2), and is
/// closed after `idle_timeout` without jobs.
#[derive(Clone)]
pub struct WorkerHandle {
    cmds: mpsc::UnboundedSender<Cmd>,
    next_id: Arc<AtomicU64>,
    in_flight: Arc<AtomicUsize>,
}

enum Cmd {
    Download(Queued),
    Cancel(JobId),
    RestartWhenIdle,
}

impl WorkerHandle {
    /// Must be called inside a tokio runtime: it spawns the actor task.
    pub fn spawn(opts: WorkerOptions) -> WorkerHandle {
        let (cmds, cmd_rx) = mpsc::unbounded_channel();
        let (lines_tx, lines_rx) = mpsc::unbounded_channel();
        let next_id = Arc::new(AtomicU64::new(1));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let actor = Actor {
            opts,
            next_id: next_id.clone(),
            in_flight: in_flight.clone(),
            proc: None,
            generation: 0,
            jobs: HashMap::new(),
            queue: VecDeque::new(),
            slots: DEFAULT_SLOTS,
            idle_deadline: None,
            restart_pending: false,
            lines_tx,
        };
        tokio::spawn(actor.run(cmd_rx, lines_rx));
        WorkerHandle {
            cmds,
            next_id,
            in_flight,
        }
    }

    pub fn download(
        &self,
        url: String,
        cache_dir: PathBuf,
        priority: bool,
    ) -> (JobId, mpsc::UnboundedReceiver<WorkerMsg>) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.in_flight.fetch_add(1, Ordering::SeqCst);
        let cmd = Cmd::Download(Queued {
            id,
            url,
            cache_dir,
            tx,
            priority,
        });
        if self.cmds.send(cmd).is_err() {
            // Actor gone: the dropped sender closes `rx` without a message.
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
        }
        (id, rx)
    }

    pub fn cancel(&self, job: JobId) {
        let _ = self.cmds.send(Cmd::Cancel(job));
    }

    pub fn restart_when_idle(&self) {
        let _ = self.cmds.send(Cmd::RestartWhenIdle);
    }

    pub fn busy(&self) -> bool {
        self.in_flight.load(Ordering::SeqCst) > 0
    }
}

struct Proc {
    child: Child,
    stdin: ChildStdin,
    generation: u64,
}

struct Job {
    tx: mpsc::UnboundedSender<WorkerMsg>,
    deadline: Option<Instant>,
    priority: bool,
}

struct Queued {
    id: JobId,
    url: String,
    cache_dir: PathBuf,
    tx: mpsc::UnboundedSender<WorkerMsg>,
    priority: bool,
}

enum ReaderEvent {
    Line(Value),
    Eof(u64),
}

struct Actor {
    opts: WorkerOptions,
    next_id: Arc<AtomicU64>,
    in_flight: Arc<AtomicUsize>,
    proc: Option<Proc>,
    generation: u64,
    jobs: HashMap<JobId, Job>,
    queue: VecDeque<Queued>,
    /// Jobs the worker runs at once (its handshake's `slots`); never exceeded,
    /// so every job sent starts at once and its deadline can start with it.
    slots: usize,
    idle_deadline: Option<Instant>,
    restart_pending: bool,
    lines_tx: mpsc::UnboundedSender<ReaderEvent>,
}

impl Actor {
    async fn run(
        mut self,
        mut cmds: mpsc::UnboundedReceiver<Cmd>,
        mut lines: mpsc::UnboundedReceiver<ReaderEvent>,
    ) {
        loop {
            let deadline = self.next_deadline();
            tokio::select! {
                cmd = cmds.recv() => match cmd {
                    Some(cmd) => self.on_cmd(cmd).await,
                    None => break,
                },
                Some(ev) = lines.recv() => self.on_reader(ev),
                _ = sleep_until_opt(deadline) => self.on_deadline().await,
            }
            self.dispatch().await;
        }
        self.close();
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.jobs
            .values()
            .filter_map(|j| j.deadline)
            .chain(self.idle_deadline)
            .min()
    }

    /// Send queued jobs while the worker has free slots: background jobs use at
    /// most `BACKGROUND_SLOTS` (keeping one slot free when the worker has more
    /// than one), play-requested jobs any free slot.
    async fn dispatch(&mut self) {
        loop {
            if self.jobs.len() >= self.slots {
                return;
            }
            let background_cap = BACKGROUND_SLOTS.min(self.slots.saturating_sub(1).max(1));
            let background = self.jobs.values().filter(|j| !j.priority).count();
            let Some(i) = self
                .queue
                .iter()
                .position(|q| q.priority || background < background_cap)
            else {
                return;
            };
            let Some(job) = self.queue.remove(i) else {
                return;
            };
            self.start_job(job).await;
        }
    }

    async fn on_cmd(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Download(job) => {
                self.idle_deadline = None;
                self.queue.push_back(job);
            }
            Cmd::Cancel(job) => {
                if self.jobs.contains_key(&job) {
                    self.send_cancel(job).await;
                } else if let Some(i) = self.queue.iter().position(|q| q.id == job)
                    && let Some(q) = self.queue.remove(i)
                {
                    self.in_flight.fetch_sub(1, Ordering::SeqCst);
                    let _ = q.tx.send(WorkerMsg::Failed {
                        error: "cancelled".to_string(),
                        cancelled: true,
                        transport: false,
                        dir: None,
                    });
                    self.after_job_end();
                }
            }
            Cmd::RestartWhenIdle => {
                if self.jobs.is_empty() && self.queue.is_empty() {
                    self.close();
                } else {
                    self.restart_pending = true;
                }
            }
        }
    }

    async fn start_job(
        &mut self,
        Queued {
            id,
            url,
            cache_dir,
            tx,
            priority,
        }: Queued,
    ) {
        self.idle_deadline = None;
        if self.proc.is_none()
            && let Err(e) = self.spawn_proc().await
        {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            let _ = tx.send(WorkerMsg::Failed {
                error: format!("{e:#}"),
                cancelled: false,
                transport: true,
                dir: None,
            });
            return;
        }
        self.jobs.insert(
            id,
            Job {
                tx,
                // The worker has a free slot, so its `running` line is due at
                // once; a job that never starts still times out.
                deadline: Some(Instant::now() + self.opts.inactivity_timeout),
                priority,
            },
        );
        let req = json!({
            "id": id,
            "cmd": "download",
            "url": url,
            "cache_dir": cache_dir.to_string_lossy(),
        });
        if let Err(e) = self.write(&req).await {
            self.close();
            self.fail_all_transport(&format!("failed writing to worker: {e}"));
        }
    }

    async fn spawn_proc(&mut self) -> Result<()> {
        let (program, args) = match &self.opts.worker_cmd {
            Some(cmd) => {
                let (program, args) = cmd.split_first().context("worker_cmd is empty")?;
                (program.clone(), args.to_vec())
            }
            None => worker_command(self.opts.worker_python.as_deref())?,
        };
        let stderr = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.opts.log_path)
            .map(Stdio::from)
            .unwrap_or_else(|_| Stdio::null());
        let mut cmd = Command::new(&program);
        if let Some(dir) = self
            .opts
            .log_path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
        {
            cmd.current_dir(dir);
        }
        let mut child = cmd
            .args(&args)
            // launchd gives no terminal locale; titles cross stdout as UTF-8.
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUNBUFFERED", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn worker: {program} {}", args.join(" ")))?;
        let stdin = child.stdin.take().context("no stdin on worker")?;
        let stdout = child.stdout.take().context("no stdout on worker")?;
        let mut stdout = BufReader::new(stdout);
        self.slots = await_ready(&mut stdout, &self.opts.log_path).await?;

        self.generation += 1;
        tokio::spawn(read_lines(self.generation, stdout, self.lines_tx.clone()));
        self.proc = Some(Proc {
            child,
            stdin,
            generation: self.generation,
        });
        Ok(())
    }

    async fn write(&mut self, req: &Value) -> std::io::Result<()> {
        let Some(proc) = self.proc.as_mut() else {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        };
        let mut line = req.to_string();
        line.push('\n');
        proc.stdin.write_all(line.as_bytes()).await?;
        proc.stdin.flush().await
    }

    async fn send_cancel(&mut self, job: JobId) {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        // Best effort: a dead worker is reported by the reader's EOF.
        let _ = self
            .write(&json!({"id": id, "cmd": "cancel", "target": job}))
            .await;
    }

    fn on_reader(&mut self, ev: ReaderEvent) {
        match ev {
            ReaderEvent::Line(v) => self.on_line(v),
            ReaderEvent::Eof(generation) => {
                if self
                    .proc
                    .as_ref()
                    .is_some_and(|p| p.generation == generation)
                {
                    self.close();
                    self.fail_all_transport("worker exited unexpectedly");
                }
            }
        }
    }

    fn on_line(&mut self, v: Value) {
        // Ids are unique across spawns, so lines of an old process never match.
        let Some(id) = v.get("id").and_then(Value::as_u64) else {
            return;
        };
        let Some(job) = self.jobs.get_mut(&id) else {
            return;
        };
        job.deadline = Some(Instant::now() + self.opts.inactivity_timeout);
        if let Some(event) = v.get("event") {
            let msg = match event.as_str() {
                Some("started") => parse_started(&v),
                Some("progress") => parse_progress(&v),
                _ => None,
            };
            if let Some(msg) = msg {
                let _ = job.tx.send(msg);
            }
        } else if let Some(ok) = v.get("ok").and_then(Value::as_bool) {
            let msg = if ok {
                parse_done(&v).unwrap_or_else(|| WorkerMsg::Failed {
                    error: format!("malformed worker reply: {v}"),
                    cancelled: false,
                    transport: false,
                    dir: None,
                })
            } else {
                WorkerMsg::Failed {
                    error: str_field(&v, "error").unwrap_or_else(|| "unknown worker error".into()),
                    cancelled: v.get("cancelled").and_then(Value::as_bool).unwrap_or(false),
                    transport: false,
                    dir: str_field(&v, "dir"),
                }
            };
            self.finish(id, msg);
        }
    }

    async fn on_deadline(&mut self) {
        let now = Instant::now();
        let stalled: Vec<JobId> = self
            .jobs
            .iter()
            .filter(|(_, j)| j.deadline.is_some_and(|d| d <= now))
            .map(|(id, _)| *id)
            .collect();
        for id in stalled {
            self.send_cancel(id).await;
            let error = format!(
                "download stalled: no worker activity for {:?}",
                self.opts.inactivity_timeout
            );
            self.finish(
                id,
                WorkerMsg::Failed {
                    error,
                    cancelled: false,
                    transport: false,
                    dir: None,
                },
            );
        }
        if self.jobs.is_empty() && self.idle_deadline.is_some_and(|d| d <= now) {
            self.close();
        }
    }

    fn finish(&mut self, id: JobId, msg: WorkerMsg) {
        if let Some(job) = self.jobs.remove(&id) {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            let _ = job.tx.send(msg);
        }
        self.after_job_end();
    }

    fn fail_all_transport(&mut self, error: &str) {
        for (_, job) in self.jobs.drain() {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            let _ = job.tx.send(WorkerMsg::Failed {
                error: error.to_string(),
                cancelled: false,
                transport: true,
                dir: None,
            });
        }
        self.after_job_end();
    }

    fn after_job_end(&mut self) {
        if !self.jobs.is_empty() || !self.queue.is_empty() {
            return;
        }
        if self.restart_pending {
            self.close();
        } else if self.proc.is_some() {
            self.idle_deadline = Some(Instant::now() + self.opts.idle_timeout);
        }
    }

    /// Close stdin (the worker exits on EOF) and reap the process.
    fn close(&mut self) {
        self.idle_deadline = None;
        self.restart_pending = false;
        if let Some(Proc {
            mut child, stdin, ..
        }) = self.proc.take()
        {
            drop(stdin);
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
        }
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(d) => tokio::time::sleep_until(d).await,
        None => std::future::pending().await,
    }
}

/// Read the `ready` line, so a worker that failed to start (or speaks another
/// protocol) is reported on the job that spawned it.
/// Read the `ready` line; returns the worker's declared `slots` (concurrent jobs).
async fn await_ready(stdout: &mut BufReader<ChildStdout>, log_path: &Path) -> Result<usize> {
    let mut line = Vec::new();
    let read = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_line_bounded(stdout, &mut line))
        .await
        .context("worker did not send its readiness handshake in time")?
        .context("failed reading worker handshake")?;
    match read {
        LineRead::Eof => anyhow::bail!(
            "worker exited before handshake (see {})",
            log_path.display()
        ),
        LineRead::TooLong => anyhow::bail!("worker handshake line longer than {MAX_LINE} bytes"),
        LineRead::Line => {}
    }
    let text = String::from_utf8_lossy(&line);
    let v: Value = serde_json::from_str(text.trim())
        .with_context(|| format!("worker handshake was not JSON: {:?}", text.trim()))?;
    if v.get("event").and_then(Value::as_str) != Some("ready")
        || v.get("protocol").and_then(Value::as_u64) != Some(PROTOCOL)
    {
        anyhow::bail!("unexpected worker handshake: {}", text.trim());
    }
    Ok(v.get("slots")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_SLOTS, |n| (n as usize).max(1)))
}

/// Forward every JSON stdout line to the actor; non-JSON lines (stray prints)
/// are skipped. EOF is reported with the process generation.
async fn read_lines(
    generation: u64,
    mut stdout: BufReader<ChildStdout>,
    tx: mpsc::UnboundedSender<ReaderEvent>,
) {
    let mut line = Vec::new();
    loop {
        match read_line_bounded(&mut stdout, &mut line).await {
            Ok(LineRead::Eof) | Err(_) => break,
            Ok(LineRead::TooLong) => {
                eprintln!("worker: discarded a stdout line longer than {MAX_LINE} bytes");
            }
            Ok(LineRead::Line) => {
                if let Ok(v) = serde_json::from_slice::<Value>(&line)
                    && tx.send(ReaderEvent::Line(v)).is_err()
                {
                    return;
                }
            }
        }
    }
    let _ = tx.send(ReaderEvent::Eof(generation));
}

enum LineRead {
    Line,
    TooLong,
    Eof,
}

/// Read one line into `line`; a line over `MAX_LINE` bytes is skipped to its
/// end without being buffered and reported as `TooLong`.
async fn read_line_bounded<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> std::io::Result<LineRead> {
    line.clear();
    let mut too_long = false;
    loop {
        let (n, done) = {
            let buf = reader.fill_buf().await?;
            if buf.is_empty() {
                return Ok(if too_long {
                    LineRead::TooLong
                } else if line.is_empty() {
                    LineRead::Eof
                } else {
                    LineRead::Line
                });
            }
            let (n, done) = match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (i + 1, true),
                None => (buf.len(), false),
            };
            if !too_long {
                if line.len() + n > MAX_LINE {
                    too_long = true;
                    line.clear();
                } else {
                    line.extend_from_slice(&buf[..n]);
                }
            }
            (n, done)
        };
        reader.consume(n);
        if done {
            return Ok(if too_long {
                LineRead::TooLong
            } else {
                LineRead::Line
            });
        }
    }
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(String::from)
}

fn i64_field(v: &Value, key: &str) -> Option<i64> {
    let f = v.get(key)?;
    f.as_i64().or_else(|| f.as_f64().map(|x| x as i64))
}

fn u64_field(v: &Value, key: &str) -> Option<u64> {
    let f = v.get(key)?;
    f.as_u64().or_else(|| f.as_f64().map(|x| x as u64))
}

fn parse_meta(v: &Value) -> Option<TrackMeta> {
    let m = v.get("meta")?;
    Some(TrackMeta {
        id: str_field(m, "id")?,
        title: str_field(m, "title")?,
        uploader: str_field(m, "uploader"),
        duration: i64_field(m, "duration"),
        webpage_url: str_field(m, "webpage_url"),
    })
}

fn parse_started(v: &Value) -> Option<WorkerMsg> {
    Some(WorkerMsg::Started {
        path: str_field(v, "path")?,
        dir: str_field(v, "dir")?,
        meta: parse_meta(v)?,
    })
}

fn parse_progress(v: &Value) -> Option<WorkerMsg> {
    Some(WorkerMsg::Progress {
        bytes: u64_field(v, "bytes")?,
        total: u64_field(v, "total"),
    })
}

fn parse_done(v: &Value) -> Option<WorkerMsg> {
    Some(WorkerMsg::Done {
        path: str_field(v, "path")?,
        dir: str_field(v, "dir")?,
        meta: parse_meta(v)?,
        thumb: str_field(v, "thumb"),
        format: str_field(v, "format"),
        file_size: i64_field(v, "file_size"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // YPLAY_WORKER_CMD is process-global: every test that sets it holds this.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    // Speaks worker protocol v2. The url `<kind>:<video id>` picks the
    // behaviour; spawns, cancels and clean exits are appended to files next
    // to the script so tests can observe the process from outside.
    const FAKE_WORKER: &str = r#"
import json, os, sys, threading, time
from concurrent.futures import ThreadPoolExecutor

HERE = os.path.dirname(os.path.abspath(__file__))
lock = threading.Lock()
cancels = {}
pool = ThreadPoolExecutor(int(sys.argv[1])) if len(sys.argv) > 1 else None


def note(name, text):
    with open(os.path.join(HERE, name), "a") as f:
        f.write(text + "\n")


def send(obj):
    with lock:
        sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
        sys.stdout.flush()


def meta(vid):
    return {"id": vid, "title": "Title " + vid, "uploader": "Up",
            "duration": 42, "webpage_url": "https://youtu.be/" + vid}


def download(req):
    n = req["id"]
    kind, _, vid = req["url"].partition(":")
    if kind == "mute":
        return
    send({"id": n, "event": "running"})
    d = os.path.join(req["cache_dir"], "T [" + vid[:8] + "]")
    path = os.path.join(d, "audio.webm")
    m = meta(vid)
    if kind == "silent":
        return
    if kind == "slow":
        time.sleep(0.5)
    if kind == "stream":
        with lock:
            sys.stdout.write("not json\n")
            sys.stdout.flush()
    if kind == "env":
        m = {"id": vid, "title": "秒針を噛む",
             "uploader": os.environ.get("PYTHONIOENCODING"),
             "duration": None, "webpage_url": os.environ.get("PYTHONUNBUFFERED")}
    send({"id": n, "event": "started", "path": path, "dir": d, "meta": m})
    if kind == "crash":
        os._exit(3)
    if kind == "wait":
        cancels[n].wait()
        send({"id": n, "ok": False, "error": "cancelled", "cancelled": True, "dir": d})
        return
    if kind == "tick":
        for k in range(3):
            time.sleep(0.1)
            send({"id": n, "event": "progress", "bytes": k, "total": None})
    send({"id": n, "event": "progress", "bytes": 100, "total": 200})
    send({"id": n, "event": "progress", "bytes": 200, "total": None})
    send({"id": n, "ok": True, "path": path, "dir": d, "meta": m,
          "thumb": os.path.join(d, "cover.webp"), "format": "webm", "file_size": 200})


note("spawns", str(os.getpid()))
sys.stderr.write("fake worker stderr pid=%d\n" % os.getpid())
sys.stderr.flush()
ready = {"event": "ready", "ok": True, "protocol": 2}
if pool:
    ready["slots"] = int(sys.argv[1])
send(ready)
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
        if pool:
            pool.submit(download, req)
        else:
            threading.Thread(target=download, args=(req,), daemon=True).start()
note("exits", str(os.getpid()))
"#;

    const LONG: Duration = Duration::from_secs(30);
    const WAIT: Duration = Duration::from_secs(5);

    fn setup(idle: Duration, inactivity: Duration) -> (tempfile::TempDir, WorkerHandle) {
        setup_args(idle, inactivity, "")
    }

    fn setup_args(
        idle: Duration,
        inactivity: Duration,
        args: &str,
    ) -> (tempfile::TempDir, WorkerHandle) {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fake_worker.py");
        std::fs::write(&script, FAKE_WORKER).unwrap();
        let cmd = format!("/usr/bin/python3 {} {args}", script.display());
        // SAFETY: every test that touches the environment holds ENV_LOCK.
        unsafe { std::env::set_var("YPLAY_WORKER_CMD", cmd) };
        let worker = WorkerHandle::spawn(WorkerOptions {
            worker_python: None,
            worker_cmd: None,
            log_path: tmp.path().join("worker.log"),
            idle_timeout: idle,
            inactivity_timeout: inactivity,
        });
        (tmp, worker)
    }

    fn meta(vid: &str) -> TrackMeta {
        TrackMeta {
            id: vid.to_string(),
            title: format!("Title {vid}"),
            uploader: Some("Up".to_string()),
            duration: Some(42),
            webpage_url: Some(format!("https://youtu.be/{vid}")),
        }
    }

    fn read_lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path)
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    }

    async fn wait_for_lines(path: &Path, n: usize) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let lines = read_lines(path);
            if lines.len() >= n || tokio::time::Instant::now() >= deadline {
                return lines;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn recv(rx: &mut mpsc::UnboundedReceiver<WorkerMsg>) -> WorkerMsg {
        tokio::time::timeout(WAIT, rx.recv())
            .await
            .expect("timed out waiting for a worker message")
            .expect("job channel closed without a terminal message")
    }

    async fn until_terminal(rx: &mut mpsc::UnboundedReceiver<WorkerMsg>) -> Vec<WorkerMsg> {
        let mut msgs = Vec::new();
        loop {
            let msg = recv(rx).await;
            let terminal = matches!(msg, WorkerMsg::Done { .. } | WorkerMsg::Failed { .. });
            msgs.push(msg);
            if terminal {
                return msgs;
            }
        }
    }

    #[tokio::test]
    async fn streamed_download_maps_started_progress_done() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup(LONG, LONG);
        let cache = tmp.path().join("cache");
        assert!(!worker.busy());

        let (_job, mut rx) = worker.download("stream:aaaaaaaaaaa".into(), cache.clone(), false);
        assert!(worker.busy());
        let dir = cache.join("T [aaaaaaaa]").display().to_string();
        let path = format!("{dir}/audio.webm");
        assert_eq!(
            recv(&mut rx).await,
            WorkerMsg::Started {
                path: path.clone(),
                dir: dir.clone(),
                meta: meta("aaaaaaaaaaa"),
            }
        );
        assert_eq!(
            recv(&mut rx).await,
            WorkerMsg::Progress {
                bytes: 100,
                total: Some(200),
            }
        );
        assert_eq!(
            recv(&mut rx).await,
            WorkerMsg::Progress {
                bytes: 200,
                total: None,
            }
        );
        assert_eq!(
            recv(&mut rx).await,
            WorkerMsg::Done {
                path,
                dir: dir.clone(),
                meta: meta("aaaaaaaaaaa"),
                thumb: Some(format!("{dir}/cover.webp")),
                format: Some("webm".to_string()),
                file_size: Some(200),
            }
        );
        assert!(!worker.busy());
    }

    #[tokio::test]
    async fn concurrent_jobs_route_by_id() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup(LONG, LONG);
        let cache = tmp.path().join("cache");

        let (a, mut rx_a) = worker.download("slow:aaaaaaaaaaa".into(), cache.clone(), false);
        let (b, mut rx_b) = worker.download("stream:bbbbbbbbbbb".into(), cache.clone(), false);
        assert_ne!(a, b);

        // b completes while a is still sleeping inside the fake.
        let b_msgs = until_terminal(&mut rx_b).await;
        assert!(rx_a.try_recv().is_err());
        assert!(worker.busy());
        let a_msgs = until_terminal(&mut rx_a).await;
        assert!(!worker.busy());

        for (msgs, vid) in [(&a_msgs, "aaaaaaaaaaa"), (&b_msgs, "bbbbbbbbbbb")] {
            assert_eq!(msgs.len(), 4);
            assert!(matches!(&msgs[0], WorkerMsg::Started { meta: m, .. } if *m == meta(vid)));
            assert!(matches!(&msgs[3], WorkerMsg::Done { meta: m, .. } if *m == meta(vid)));
        }
        assert_eq!(read_lines(&tmp.path().join("spawns")).len(), 1);
    }

    #[tokio::test]
    async fn cancel_produces_failed_cancelled() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup(LONG, LONG);

        let (job, mut rx) =
            worker.download("wait:ccccccccccc".into(), tmp.path().join("cache"), false);
        assert!(matches!(recv(&mut rx).await, WorkerMsg::Started { .. }));
        worker.cancel(job);
        assert_eq!(
            recv(&mut rx).await,
            WorkerMsg::Failed {
                error: "cancelled".to_string(),
                cancelled: true,
                transport: false,
                dir: Some(tmp.path().join("cache/T [cccccccc]").display().to_string()),
            }
        );
        assert!(!worker.busy());
        assert_eq!(
            read_lines(&tmp.path().join("cancels")),
            vec![job.to_string()]
        );
    }

    #[tokio::test]
    async fn crash_mid_job_fails_transport_and_next_job_respawns() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup(LONG, LONG);
        let cache = tmp.path().join("cache");

        let (_job, mut rx) = worker.download("crash:ddddddddddd".into(), cache.clone(), false);
        assert!(matches!(recv(&mut rx).await, WorkerMsg::Started { .. }));
        let msg = recv(&mut rx).await;
        assert!(
            matches!(
                msg,
                WorkerMsg::Failed {
                    cancelled: false,
                    transport: true,
                    ..
                }
            ),
            "{msg:?}"
        );
        assert!(!worker.busy());

        let (_job, mut rx) = worker.download("stream:eeeeeeeeeee".into(), cache, false);
        let msgs = until_terminal(&mut rx).await;
        assert!(
            matches!(msgs.last(), Some(WorkerMsg::Done { meta: m, .. }) if *m == meta("eeeeeeeeeee"))
        );
        let spawns = read_lines(&tmp.path().join("spawns"));
        assert_eq!(spawns.len(), 2);
        assert_ne!(spawns[0], spawns[1]);
    }

    #[tokio::test]
    async fn idle_timeout_closes_stdin_and_worker_exits() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup(Duration::from_millis(200), LONG);
        let exits = tmp.path().join("exits");

        let (_job, mut rx) =
            worker.download("stream:fffffffffff".into(), tmp.path().join("cache"), false);
        until_terminal(&mut rx).await;
        assert!(read_lines(&exits).is_empty());

        let exited = wait_for_lines(&exits, 1).await;
        assert_eq!(exited, read_lines(&tmp.path().join("spawns")));
    }

    #[tokio::test]
    async fn inactivity_timeout_fails_silent_job() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup(LONG, Duration::from_millis(300));

        let t0 = tokio::time::Instant::now();
        let (job, mut rx) =
            worker.download("silent:ggggggggggg".into(), tmp.path().join("cache"), false);
        let msg = recv(&mut rx).await;
        assert!(
            matches!(
                msg,
                WorkerMsg::Failed {
                    cancelled: false,
                    transport: false,
                    ..
                }
            ),
            "{msg:?}"
        );
        assert!(t0.elapsed() >= Duration::from_millis(300));
        assert!(!worker.busy());
        assert_eq!(
            wait_for_lines(&tmp.path().join("cancels"), 1).await,
            vec![job.to_string()]
        );
    }

    #[tokio::test]
    async fn queued_jobs_do_not_stall_before_running() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup_args(LONG, Duration::from_millis(300), "1");
        let cache = tmp.path().join("cache");

        let mut rxs = Vec::new();
        for i in 0..8 {
            let (_job, rx) = worker.download(format!("tick:job{i:0>8}"), cache.clone(), false);
            rxs.push(rx);
        }
        for (i, rx) in rxs.iter_mut().enumerate() {
            let msgs = until_terminal(rx).await;
            assert!(
                matches!(msgs.last(), Some(WorkerMsg::Done { .. })),
                "job {i}: {msgs:?}"
            );
        }
        assert!(!worker.busy());
    }

    #[tokio::test]
    async fn a_job_the_worker_never_starts_times_out() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup_args(LONG, Duration::from_millis(300), "4");
        let cache = tmp.path().join("cache");
        let (_job, mut rx) = worker.download("mute:muteaaaaaa".into(), cache, true);
        let msgs = until_terminal(&mut rx).await;
        assert!(
            matches!(
                msgs.last(),
                Some(WorkerMsg::Failed {
                    cancelled: false,
                    ..
                })
            ),
            "{msgs:?}"
        );
        assert!(!worker.busy());
    }

    #[tokio::test]
    async fn priority_job_starts_while_three_background_jobs_run() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup(LONG, LONG);
        let cache = tmp.path().join("cache");

        let mut waits = Vec::new();
        for i in 0..3 {
            let (job, mut rx) = worker.download(format!("wait:bg{i:0>9}"), cache.clone(), false);
            assert!(matches!(recv(&mut rx).await, WorkerMsg::Started { .. }));
            waits.push((job, rx));
        }
        let (_job, mut queued) = worker.download("stream:qqqqqqqqqqq".into(), cache.clone(), false);
        let (_job, mut prio) = worker.download("stream:ppppppppppp".into(), cache.clone(), true);
        let msgs = until_terminal(&mut prio).await;
        assert!(
            matches!(msgs.last(), Some(WorkerMsg::Done { meta: m, .. }) if *m == meta("ppppppppppp"))
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(queued.try_recv().is_err());

        let (job, rx) = &mut waits[0];
        worker.cancel(*job);
        until_terminal(rx).await;
        let msgs = until_terminal(&mut queued).await;
        assert!(
            matches!(msgs.last(), Some(WorkerMsg::Done { meta: m, .. }) if *m == meta("qqqqqqqqqqq"))
        );
        for (job, rx) in &mut waits[1..] {
            worker.cancel(*job);
            until_terminal(rx).await;
        }
        assert!(!worker.busy());
    }

    #[tokio::test]
    async fn stderr_is_appended_to_log_across_spawns() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup(LONG, LONG);
        let cache = tmp.path().join("cache");
        let log = tmp.path().join("worker.log");
        std::fs::write(&log, "earlier line\n").unwrap();

        let (_job, mut rx) = worker.download("stream:hhhhhhhhhhh".into(), cache.clone(), false);
        until_terminal(&mut rx).await;
        worker.restart_when_idle();
        let (_job, mut rx) = worker.download("stream:iiiiiiiiiii".into(), cache, false);
        until_terminal(&mut rx).await;

        let spawns = read_lines(&tmp.path().join("spawns"));
        assert_eq!(spawns.len(), 2);
        assert_eq!(
            wait_for_lines(&tmp.path().join("exits"), 1).await,
            vec![spawns[0].clone()]
        );
        assert_eq!(
            read_lines(&log),
            vec![
                "earlier line".to_string(),
                format!("fake worker stderr pid={}", spawns[0]),
                format!("fake worker stderr pid={}", spawns[1]),
            ]
        );
    }

    #[tokio::test]
    async fn worker_gets_utf8_env_and_cjk_title_round_trips() {
        let _env = ENV_LOCK.lock().await;
        let (tmp, worker) = setup(LONG, LONG);

        let (_job, mut rx) =
            worker.download("env:jjjjjjjjjjj".into(), tmp.path().join("cache"), false);
        match recv(&mut rx).await {
            WorkerMsg::Started { meta, .. } => {
                assert_eq!(meta.title, "秒針を噛む");
                assert_eq!(meta.uploader.as_deref(), Some("utf-8"));
                assert_eq!(meta.webpage_url.as_deref(), Some("1"));
            }
            other => panic!("expected Started, got {other:?}"),
        }
        until_terminal(&mut rx).await;
    }
}
