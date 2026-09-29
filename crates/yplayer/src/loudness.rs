//! Loudness leveling: each track's integrated loudness and sample peak are
//! measured once with ffmpeg's EBU R128 filter and turned into the gain mpv
//! plays it with (`volume-gain`).

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;

/// Loudness every track is brought to, in LUFS.
pub const TARGET_LUFS: f64 = -14.0;
/// The gain never lifts a track's sample peak above this, in dBFS.
const PEAK_CEILING: f64 = -1.0;
const MIN_GAIN: f64 = -20.0;
const MAX_GAIN: f64 = 10.0;
/// Integrated loudness below this is silence: the measurement failed.
const SILENCE_BELOW: f64 = -60.0;
/// A measurement still running after this long is killed.
const TIMEOUT: Duration = Duration::from_secs(600);
/// Runs ffmpeg at background priority.
const TASKPOLICY: &str = "/usr/sbin/taskpolicy";

/// One track's measured levels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measurement {
    /// Integrated loudness, in LUFS.
    pub integrated: f64,
    /// Sample peak, in dBFS.
    pub sample_peak: f64,
}

/// Parse the final summary ffmpeg's `ebur128=peak=sample` filter prints:
/// `I: <x> LUFS` and, under `Sample peak:`, `Peak: <y> dBFS`. Silence
/// (I < −60) and anything unparsable are `None`.
pub fn parse_summary(stderr: &str) -> Option<Measurement> {
    let summary = &stderr[stderr.rfind("Summary:")?..];
    let mut integrated = None;
    let mut sample_peak = None;
    let mut section = "";
    for line in summary.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("I:") {
            integrated = number(rest, "LUFS");
        } else if let Some(rest) = line.strip_prefix("Peak:") {
            if section == "Sample peak:" {
                sample_peak = number(rest, "dBFS");
            }
        } else if line.ends_with(':') {
            section = line;
        }
    }
    let m = Measurement {
        integrated: integrated?,
        sample_peak: sample_peak?,
    };
    (m.integrated.is_finite() && m.sample_peak.is_finite() && m.integrated >= SILENCE_BELOW)
        .then_some(m)
}

/// `<number> <unit>` → the number.
fn number(text: &str, unit: &str) -> Option<f64> {
    text.trim().strip_suffix(unit)?.trim().parse().ok()
}

/// Gain in dB that brings a track to `TARGET_LUFS` without lifting its
/// sample peak above −1 dBFS, clamped to [−20, +10].
pub fn gain_db(m: Measurement) -> f64 {
    (TARGET_LUFS - m.integrated)
        .min(PEAK_CEILING - m.sample_peak)
        .clamp(MIN_GAIN, MAX_GAIN)
}

/// Measure `path`: `taskpolicy -b ffmpeg … -af ebur128=peak=sample …`,
/// killed after 10 minutes.
pub async fn measure(ffmpeg: &Path, path: &Path) -> Result<Measurement, String> {
    let output = tokio::process::Command::new(TASKPOLICY)
        .arg("-b")
        .arg(ffmpeg)
        .args(["-hide_banner", "-nostats", "-nostdin", "-i"])
        .arg(path)
        .args([
            "-map",
            "0:a:0",
            "-af",
            "ebur128=peak=sample:framelog=quiet",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(TIMEOUT, output)
        .await
        .map_err(|_| "ffmpeg timed out".to_string())?
        .map_err(|e| format!("could not run ffmpeg: {e}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        let last = stderr.lines().last().unwrap_or("").trim();
        return Err(format!("ffmpeg failed ({}): {last}", output.status));
    }
    parse_summary(&stderr).ok_or_else(|| "no usable loudness summary".to_string())
}

pub type MeasureFuture = Pin<Box<dyn Future<Output = Result<Measurement, String>> + Send>>;

/// Measures one audio file: ffmpeg in the service, a fake in tests.
pub trait Measure: Send + Sync {
    fn measure(&self, path: PathBuf) -> MeasureFuture;
}

/// The ffmpeg binary measurements run with.
pub struct Ffmpeg(pub PathBuf);

impl Ffmpeg {
    /// `ffmpeg` on `PATH` (the LaunchAgent's includes /opt/homebrew/bin).
    pub fn find() -> Option<Ffmpeg> {
        which::which("ffmpeg").ok().map(Ffmpeg)
    }
}

impl Measure for Ffmpeg {
    fn measure(&self, path: PathBuf) -> MeasureFuture {
        let ffmpeg = self.0.clone();
        Box::pin(async move { measure(&ffmpeg, &path).await })
    }
}

/// Gains of the measured tracks and whether leveling is on.
#[derive(Debug, Default)]
pub struct Levels {
    enabled: bool,
    gains: HashMap<String, f64>,
    /// `gains`' values in order, for the median.
    sorted: Vec<f64>,
}

impl Levels {
    pub fn new(enabled: bool, measured: impl IntoIterator<Item = (String, Measurement)>) -> Self {
        let mut levels = Levels {
            enabled,
            ..Levels::default()
        };
        for (id, m) in measured {
            levels.set(&id, Some(m));
        }
        levels
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Record `id`'s measurement; `None` (failed, or reset) forgets it.
    pub fn set(&mut self, id: &str, m: Option<Measurement>) {
        self.forget(id);
        if let Some(m) = m {
            let gain = gain_db(m);
            let at = self.sorted.partition_point(|g| g.total_cmp(&gain).is_lt());
            self.sorted.insert(at, gain);
            self.gains.insert(id.to_string(), gain);
        }
    }

    pub fn forget(&mut self, id: &str) {
        if let Some(gain) = self.gains.remove(id) {
            let at = self.sorted.partition_point(|g| g.total_cmp(&gain).is_lt());
            self.sorted.remove(at);
        }
    }

    /// Median gain of the measured tracks; 0 when there are none.
    pub fn median(&self) -> f64 {
        let n = self.sorted.len();
        match n {
            0 => 0.0,
            _ if n % 2 == 1 => self.sorted[n / 2],
            _ => (self.sorted[n / 2 - 1] + self.sorted[n / 2]) / 2.0,
        }
    }

    /// The gain to play `id` with: 0 while leveling is off; the median for
    /// a track that is unmeasured or failed.
    pub fn gain(&self, id: &str) -> f64 {
        if !self.enabled {
            return 0.0;
        }
        self.gains.get(id).copied().unwrap_or_else(|| self.median())
    }
}

/// Which track to measure next, one at a time: tracks that just finished
/// downloading, then the playing or preloaded track, then the backfill.
#[derive(Debug, Default)]
pub struct Schedule {
    fresh: VecDeque<String>,
    backfill: VecDeque<String>,
    running: Option<String>,
}

impl Schedule {
    /// `id` finished downloading.
    pub fn downloaded(&mut self, id: &str) {
        if !self.fresh.iter().any(|f| f == id) {
            self.fresh.push_back(id.to_string());
        }
    }

    /// Replace the backfill with `ids`, in measuring order.
    pub fn set_backfill(&mut self, ids: Vec<String>) {
        self.backfill = ids.into();
    }

    pub fn running(&self) -> Option<&str> {
        self.running.as_deref()
    }

    /// Pick the next track and mark it running; `None` while one runs or
    /// when nothing is due. `current` is the playing and preloaded tracks;
    /// `candidate` gives what to measure for an id, or `None` to skip it
    /// (measured, downloading, gone).
    pub fn start<P>(
        &mut self,
        current: &[&str],
        candidate: impl Fn(&str) -> Option<P>,
    ) -> Option<(String, P)> {
        if self.running.is_some() {
            return None;
        }
        let picked = pop_first(&mut self.fresh, &candidate)
            .or_else(|| {
                current
                    .iter()
                    .find_map(|id| candidate(id).map(|p| (id.to_string(), p)))
            })
            .or_else(|| pop_first(&mut self.backfill, &candidate))?;
        self.running = Some(picked.0.clone());
        Some(picked)
    }

    /// The running measurement ended.
    pub fn finish(&mut self) {
        self.running = None;
    }
}

fn pop_first<P>(
    queue: &mut VecDeque<String>,
    candidate: &impl Fn(&str) -> Option<P>,
) -> Option<(String, P)> {
    while let Some(id) = queue.pop_front() {
        if let Some(p) = candidate(&id) {
            return Some((id, p));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: &str = "[out#0/null @ 0x1] video:0KiB audio:375KiB
[Parsed_ebur128_0 @ 0x7680c44900] Summary:

  Integrated loudness:
    I:         -21.8 LUFS
    Threshold: -31.8 LUFS

  Loudness range:
    LRA:         0.0 LU
    Threshold: -41.8 LUFS
    LRA low:   -21.8 LUFS
    LRA high:  -21.8 LUFS

  Sample peak:
    Peak:      -17.8 dBFS
[out#0/null @ 0x7680c44180] video:0KiB audio:375KiB subtitle:0KiB other streams:0KiB
size=N/A time=00:00:04.00 bitrate=N/A speed= 652x elapsed=0:00:00.00
";

    fn m(integrated: f64, sample_peak: f64) -> Measurement {
        Measurement {
            integrated,
            sample_peak,
        }
    }

    #[test]
    fn parses_integrated_loudness_and_sample_peak() {
        assert_eq!(parse_summary(NORMAL), Some(m(-21.8, -17.8)));
        let loud = NORMAL
            .replace(
                "-21.8 LUFS\n    Threshold: -31.8",
                "-6.7 LUFS\n    Threshold: -16.7",
            )
            .replace("-17.8 dBFS", "1.7 dBFS");
        assert_eq!(parse_summary(&loud), Some(m(-6.7, 1.7)));
    }

    #[test]
    fn peak_comes_from_the_sample_peak_block_of_the_last_summary() {
        let text = "Summary:\n  Integrated loudness:\n    I: -30.0 LUFS\n  Sample peak:\n    Peak: -20.0 dBFS\n\
            [Parsed_ebur128_0 @ 0x2] Summary:\n\n  Integrated loudness:\n    I:         -9.5 LUFS\n\
            \n  True peak:\n    Peak:        0.9 dBFS\n\n  Sample peak:\n    Peak:       -0.4 dBFS\n";
        assert_eq!(parse_summary(text), Some(m(-9.5, -0.4)));
    }

    #[test]
    fn silence_is_a_failure() {
        let silence = "[Parsed_ebur128_0 @ 0x1] Summary:\n\n  Integrated loudness:\n    I:         -70.0 LUFS\n\
            \n  Sample peak:\n    Peak:       -inf dBFS\n";
        assert_eq!(parse_summary(silence), None);
        let quiet = NORMAL.replace("-21.8 LUFS\n    Threshold", "-60.5 LUFS\n    Threshold");
        assert_eq!(parse_summary(&quiet), None);
    }

    #[test]
    fn garbage_is_a_failure() {
        for text in [
            "",
            "garbage",
            "/m/秒針を噛む.opus: Invalid data found when processing input",
            "Summary:\n  Integrated loudness:\n    I: loud LUFS\n  Sample peak:\n    Peak: -1.0 dBFS\n",
            "Summary:\n  Integrated loudness:\n    I: -10.0 LUFS\n",
            "Summary:\n  Integrated loudness:\n    I: -10.0 LUFS\n  True peak:\n    Peak: -1.0 dBFS\n",
        ] {
            assert_eq!(parse_summary(text), None, "{text}");
        }
    }

    #[test]
    fn gain_targets_minus_14_with_peak_headroom_and_clamps() {
        // Quiet track with room: reaches the target.
        assert_eq!(gain_db(m(-20.0, -10.0)), 6.0);
        // Loud track: turned down to the target.
        assert_eq!(gain_db(m(-8.0, 0.5)), -6.0);
        // The peak would pass −1 dBFS: headroom wins over the target.
        assert_eq!(gain_db(m(-20.0, -3.0)), 2.0);
        // Clamped to [−20, +10].
        assert_eq!(gain_db(m(-40.0, -30.0)), 10.0);
        assert_eq!(gain_db(m(10.0, 3.0)), -20.0);
    }

    #[test]
    fn unmeasured_tracks_use_the_median_and_off_is_zero() {
        let mut levels = Levels::new(true, []);
        assert_eq!(levels.gain("a"), 0.0);

        levels.set("a", Some(m(-20.0, -10.0))); // +6
        assert_eq!(levels.gain("a"), 6.0);
        assert_eq!(levels.gain("x"), 6.0);
        levels.set("b", Some(m(-8.0, 0.5))); // −6
        levels.set("c", Some(m(-16.0, -10.0))); // +2
        assert_eq!(levels.median(), 2.0);
        levels.set("d", Some(m(-18.0, -10.0))); // +4
        assert_eq!(levels.median(), 3.0);

        // A new measurement replaces the old gain; a failure forgets it.
        levels.set("d", Some(m(-8.0, 0.5))); // −6
        assert_eq!(levels.median(), -2.0);
        levels.set("d", None);
        assert_eq!(levels.median(), 2.0);
        assert_eq!(levels.gain("d"), 2.0);
        levels.forget("c");
        assert_eq!(levels.median(), 0.0);

        levels.set_enabled(false);
        assert_eq!(levels.gain("a"), 0.0);
        assert_eq!(levels.gain("x"), 0.0);
        let levels = Levels::new(true, [("a".to_string(), m(-20.0, -10.0))]);
        assert_eq!(levels.gain("zz"), 6.0);
    }

    #[test]
    fn schedule_prefers_fresh_then_current_then_backfill_one_at_a_time() {
        let mut s = Schedule::default();
        let all = |id: &str| Some(format!("/m/{id}"));
        s.set_backfill(vec!["b1".into(), "b2".into(), "cur".into()]);
        s.downloaded("f1");
        s.downloaded("f2");
        s.downloaded("f1");

        let picked = s.start(&["cur", "pre"], all);
        assert_eq!(picked, Some(("f1".to_string(), "/m/f1".to_string())));
        // Single flight: nothing starts while one runs.
        assert_eq!(s.start(&["cur"], all), None);
        assert_eq!(s.running(), Some("f1"));
        s.finish();

        assert_eq!(s.start(&["cur"], all).unwrap().0, "f2");
        s.finish();
        assert_eq!(s.start(&["cur", "pre"], all).unwrap().0, "cur");
        s.finish();

        // Measured or still-downloading ids are skipped.
        let skip = |id: &str| (!matches!(id, "cur" | "pre" | "b1")).then(|| id.to_string());
        assert_eq!(s.start(&["cur", "pre"], skip).unwrap().0, "b2");
        s.finish();
        assert_eq!(s.start(&["cur", "pre"], skip), None);
        assert_eq!(s.running(), None);
    }

    #[tokio::test]
    #[ignore = "needs ffmpeg"]
    async fn real_ffmpeg_loudness_of_tone_fixture() {
        let ffmpeg = Ffmpeg::find().expect("ffmpeg on PATH");
        let fixture = PathBuf::from(format!(
            "{}/tests/fixtures/tone.opus",
            env!("CARGO_MANIFEST_DIR")
        ));
        let got = ffmpeg.measure(fixture).await.unwrap();
        eprintln!(
            "tone.opus: I = {} LUFS, peak = {} dBFS",
            got.integrated, got.sample_peak
        );
        assert!((got.integrated - -21.8).abs() < 0.2, "{got:?}");
        assert!(got.sample_peak < -1.0, "{got:?}");

        let missing = ffmpeg.measure(PathBuf::from("/nonexistent/曲.opus")).await;
        assert!(missing.is_err(), "{missing:?}");
    }
}
