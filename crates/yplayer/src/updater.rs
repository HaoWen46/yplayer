use crate::http::HttpGet;
use anyhow::{Context, bail};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LATEST_URL: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest";
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 3600);
const INSTALLED_VERSIONS: &str = "import importlib.metadata as m\nfor p in ('yt-dlp', 'yt-dlp-ejs'):\n    try:\n        print(p + '==' + m.version(p))\n    except m.PackageNotFoundError:\n        print(p + ' not installed')";

/// `YYYY-MM-DDTHH:MM:SSZ` for `t` (UTC).
fn rfc3339_utc(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Dotted integer version (`2026.06.09`, `2026.06.09.1`); `None` for anything else.
pub fn parse_version(s: &str) -> Option<Vec<u64>> {
    s.trim()
        .split('.')
        .map(|p| {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        })
        .collect()
}

/// True when `latest` parses and is strictly greater than `current`.
pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Version from a GitHub `releases/latest` redirect target (`.../tag/<version>`).
pub fn version_from_location(loc: &str) -> Option<String> {
    let (_, rest) = loc.trim().rsplit_once("/tag/")?;
    let v = rest.trim_end_matches('/').rsplit('/').next()?;
    (!v.is_empty()).then(|| v.to_string())
}

pub struct CmdOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Blocking subprocess runner; `Err` only when the process cannot run or times out.
pub trait CmdRunner: Send + Sync {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> anyhow::Result<CmdOutput>;
}

pub struct SystemRunner;

impl CmdRunner for SystemRunner {
    fn run(&self, program: &str, args: &[&str], timeout: Duration) -> anyhow::Result<CmdOutput> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to run {program}"))?;
        // Drain both pipes on threads so a chatty child cannot block on a full pipe.
        let drain = |mut pipe: Box<dyn Read + Send>| {
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let _ = pipe.read_to_end(&mut buf);
                String::from_utf8_lossy(&buf).into_owned()
            })
        };
        let stdout = drain(Box::new(child.stdout.take().expect("piped stdout")));
        let stderr = drain(Box::new(child.stderr.take().expect("piped stderr")));
        let deadline = Instant::now() + timeout;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("{program} timed out after {}s", timeout.as_secs());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Ok(CmdOutput {
            status: status.code().unwrap_or(-1),
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum UpdateOutcome {
    UpToDate(String),
    Upgraded { from: String, to: String },
    Skipped(String),
    Failed(String),
}

/// Daily yt-dlp update check (GitHub redirect) and upgrade (uv).
pub struct Updater {
    pub python: String,
    pub uv: Option<String>,
    pub stamp: PathBuf,
    pub http: Arc<dyn HttpGet>,
    pub runner: Arc<dyn CmdRunner>,
}

impl Updater {
    /// True when the stamp is missing or its mtime is older than 24 h.
    pub fn due(&self, now: SystemTime) -> bool {
        let Ok(mtime) = std::fs::metadata(&self.stamp).and_then(|m| m.modified()) else {
            return true;
        };
        now.duration_since(mtime)
            .is_ok_and(|age| age > CHECK_INTERVAL)
    }

    /// Blocking: compare the installed yt-dlp with the latest release and
    /// upgrade via uv when newer. Touches the stamp on `UpToDate`/`Upgraded`.
    pub fn check_and_upgrade(&self) -> UpdateOutcome {
        let current = match self.runner.run(
            &self.python,
            &["-c", "import yt_dlp.version as v; print(v.__version__)"],
            Duration::from_secs(30),
        ) {
            Ok(out) if out.status == 0 => out.stdout.trim().to_string(),
            Ok(out) => {
                return UpdateOutcome::Failed(format!("yt-dlp version: {}", out.stderr.trim()));
            }
            Err(e) => return UpdateOutcome::Failed(format!("yt-dlp version: {e:#}")),
        };
        let latest = match self
            .http
            .head_no_redirect(LATEST_URL, Duration::from_secs(10))
        {
            Ok(resp) => match resp.location.as_deref().and_then(version_from_location) {
                Some(v) => v,
                None => {
                    return UpdateOutcome::Failed(format!(
                        "no release tag in redirect (HTTP {})",
                        resp.status
                    ));
                }
            },
            Err(e) => return UpdateOutcome::Failed(format!("latest release: {e:#}")),
        };
        if !is_newer(&latest, &current) {
            self.touch();
            return UpdateOutcome::UpToDate(current);
        }
        let Some(uv) = &self.uv else {
            return UpdateOutcome::Skipped("uv not found".to_string());
        };
        let cutoff = rfc3339_utc(SystemTime::now() - CHECK_INTERVAL);
        let requirement = format!("yt-dlp[default]=={latest}");
        match self.runner.run(
            uv,
            &[
                "pip",
                "install",
                "--python",
                &self.python,
                "--only-binary",
                ":all:",
                "--exclude-newer",
                &cutoff,
                "--upgrade-package",
                "yt-dlp",
                "--upgrade-package",
                "yt-dlp-ejs",
                &requirement,
            ],
            Duration::from_secs(180),
        ) {
            Ok(out) if out.status == 0 => {
                self.log_installed();
                self.touch();
                UpdateOutcome::Upgraded {
                    from: current,
                    to: latest,
                }
            }
            Ok(out) => UpdateOutcome::Failed(format!("uv pip install: {}", out.stderr.trim())),
            Err(e) => UpdateOutcome::Failed(format!("uv pip install: {e:#}")),
        }
    }

    fn log_installed(&self) {
        match self.runner.run(
            &self.python,
            &["-c", INSTALLED_VERSIONS],
            Duration::from_secs(30),
        ) {
            Ok(out) if out.status == 0 => {
                eprintln!("yt-dlp update installed: {}", out.stdout.trim());
            }
            Ok(out) => eprintln!("yt-dlp update installed; versions: {}", out.stderr.trim()),
            Err(e) => eprintln!("yt-dlp update installed; versions: {e:#}"),
        }
    }

    fn touch(&self) {
        if let Some(dir) = self.stamp.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(f) = std::fs::File::create(&self.stamp) {
            let _ = f.set_modified(SystemTime::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::HttpResponse;
    use std::sync::Mutex;

    const PY: &str = "/venv/bin/python";
    const UV: &str = "/opt/bin/uv";

    struct FakeHttp {
        latest: &'static str,
    }

    impl HttpGet for FakeHttp {
        fn get(&self, _url: &str, _timeout: Duration) -> anyhow::Result<HttpResponse> {
            unreachable!("updater only sends HEAD")
        }

        fn head_no_redirect(&self, url: &str, _timeout: Duration) -> anyhow::Result<HttpResponse> {
            assert_eq!(url, "https://github.com/yt-dlp/yt-dlp/releases/latest");
            Ok(HttpResponse {
                status: 302,
                body: Vec::new(),
                location: Some(format!(
                    "https://github.com/yt-dlp/yt-dlp/releases/tag/{}",
                    self.latest
                )),
            })
        }
    }

    struct FakeRunner {
        current: &'static str,
        uv_status: i32,
        calls: Mutex<Vec<(String, Vec<String>)>>,
    }

    impl CmdRunner for FakeRunner {
        fn run(
            &self,
            program: &str,
            args: &[&str],
            _timeout: Duration,
        ) -> anyhow::Result<CmdOutput> {
            self.calls.lock().unwrap().push((
                program.to_string(),
                args.iter().map(|a| a.to_string()).collect(),
            ));
            if program == PY {
                return Ok(CmdOutput {
                    status: 0,
                    stdout: format!("{}\n", self.current),
                    stderr: String::new(),
                });
            }
            Ok(CmdOutput {
                status: self.uv_status,
                stdout: String::new(),
                stderr: if self.uv_status == 0 {
                    String::new()
                } else {
                    "error: resolution failed".to_string()
                },
            })
        }
    }

    fn updater(
        dir: &tempfile::TempDir,
        current: &'static str,
        latest: &'static str,
        uv: Option<&str>,
        uv_status: i32,
    ) -> (Updater, Arc<FakeRunner>) {
        let runner = Arc::new(FakeRunner {
            current,
            uv_status,
            calls: Mutex::new(Vec::new()),
        });
        let up = Updater {
            python: PY.to_string(),
            uv: uv.map(str::to_string),
            stamp: dir.path().join("ytdlp-update.stamp"),
            http: Arc::new(FakeHttp { latest }),
            runner: runner.clone(),
        };
        (up, runner)
    }

    #[test]
    fn parse_version_and_is_newer() {
        assert_eq!(parse_version("2026.06.09"), Some(vec![2026, 6, 9]));
        assert_eq!(parse_version("2026.06.09.1"), Some(vec![2026, 6, 9, 1]));
        assert_eq!(parse_version("garbage"), None);
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("2026..09"), None);
        assert!(is_newer("2026.06.09.1", "2026.06.09"));
        assert!(!is_newer("2026.06.09", "2026.06.09.1"));
        assert!(is_newer("2026.10.01", "2026.06.09"));
        assert!(!is_newer("2026.06.09", "2026.10.01"));
        assert!(!is_newer("2026.06.09", "2026.06.09"));
        assert!(!is_newer("garbage", "2026.06.09"));
        assert!(!is_newer("2026.06.09", "garbage"));
    }

    #[test]
    fn version_from_location_takes_tag() {
        assert_eq!(
            version_from_location("https://github.com/yt-dlp/yt-dlp/releases/tag/2026.06.09"),
            Some("2026.06.09".to_string())
        );
        assert_eq!(
            version_from_location("https://github.com/yt-dlp/yt-dlp/releases"),
            None
        );
    }

    #[test]
    fn due_follows_stamp_age() {
        let dir = tempfile::tempdir().unwrap();
        let (up, _) = updater(&dir, "2026.06.09", "2026.06.09", Some(UV), 0);
        let now = SystemTime::now();
        assert!(up.due(now), "missing stamp");

        let f = std::fs::File::create(&up.stamp).unwrap();
        f.set_modified(now).unwrap();
        assert!(!up.due(now), "fresh stamp");

        f.set_modified(now - Duration::from_secs(25 * 3600))
            .unwrap();
        assert!(up.due(now), "25 h old stamp");
    }

    #[test]
    fn rfc3339_utc_formats_dates() {
        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(rfc3339_utc(at(0)), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_utc(at(951_868_799)), "2000-02-29T23:59:59Z");
        assert_eq!(rfc3339_utc(at(1_790_000_000)), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn newer_version_installs_exact_tag_binary_only_a_day_old() {
        let dir = tempfile::tempdir().unwrap();
        let (up, runner) = updater(&dir, "2026.06.09", "2026.10.01", Some(UV), 0);
        let day = Duration::from_secs(24 * 3600);
        let before = rfc3339_utc(SystemTime::now() - day);
        assert_eq!(
            up.check_and_upgrade(),
            UpdateOutcome::Upgraded {
                from: "2026.06.09".to_string(),
                to: "2026.10.01".to_string()
            }
        );
        let after = rfc3339_utc(SystemTime::now() - day);
        let calls = runner.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0, PY);
        let argv = |cutoff: &str| {
            (
                UV.to_string(),
                [
                    "pip",
                    "install",
                    "--python",
                    PY,
                    "--only-binary",
                    ":all:",
                    "--exclude-newer",
                    cutoff,
                    "--upgrade-package",
                    "yt-dlp",
                    "--upgrade-package",
                    "yt-dlp-ejs",
                    "yt-dlp[default]==2026.10.01",
                ]
                .iter()
                .map(|a| a.to_string())
                .collect::<Vec<_>>(),
            )
        };
        assert!(
            calls[1] == argv(&before) || calls[1] == argv(&after),
            "{:?}",
            calls[1]
        );
        assert_eq!(calls[2].0, PY);
        assert!(!up.due(SystemTime::now()));
    }

    #[test]
    fn same_version_is_up_to_date_and_touches_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let (up, runner) = updater(&dir, "2026.06.09", "2026.06.09", Some(UV), 0);
        let now = SystemTime::now();
        let f = std::fs::File::create(&up.stamp).unwrap();
        f.set_modified(now - Duration::from_secs(25 * 3600))
            .unwrap();
        assert!(up.due(now));

        assert_eq!(
            up.check_and_upgrade(),
            UpdateOutcome::UpToDate("2026.06.09".to_string())
        );
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
        assert!(!up.due(SystemTime::now()));
    }

    #[test]
    fn missing_uv_skips() {
        let dir = tempfile::tempdir().unwrap();
        let (up, runner) = updater(&dir, "2026.06.09", "2026.10.01", None, 0);
        assert_eq!(
            up.check_and_upgrade(),
            UpdateOutcome::Skipped("uv not found".to_string())
        );
        assert_eq!(runner.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn runner_failure_fails_without_touching_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let (up, _) = updater(&dir, "2026.06.09", "2026.10.01", Some(UV), 1);
        assert!(matches!(up.check_and_upgrade(), UpdateOutcome::Failed(_)));
        assert!(!up.stamp.exists());
    }
}
