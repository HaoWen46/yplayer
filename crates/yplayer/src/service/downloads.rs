use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::download::worker::{JobId, WorkerMsg};
use crate::protocol::ErrorCode;
use crate::types::Track;
use crate::ytid::UrlError;

/// `download` progress events go out at most this often per track (≤ 2/s).
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

/// One message of a download job, forwarded into the core loop.
pub struct JobMsg {
    pub track_id: String,
    pub job: JobId,
    pub msg: WorkerMsg,
}

/// An in-flight download of a track that is still in the library.
pub struct Download {
    pub job: JobId,
    /// Album of the add that asked to play it: the drop-play context.
    pub album_id: Option<i64>,
    /// Audio file and folder, known once `Started` arrived.
    pub path: Option<String>,
    pub dir: Option<String>,
    last_progress: Option<Instant>,
}

impl Download {
    /// Whether a progress event may be emitted now; records it if so.
    pub fn progress_due(&mut self) -> bool {
        let now = Instant::now();
        if self
            .last_progress
            .is_some_and(|t| now.duration_since(t) < PROGRESS_INTERVAL)
        {
            return false;
        }
        self.last_progress = Some(now);
        true
    }
}

/// A job whose track was deleted: its folder is removed after its terminal
/// message.
struct Doomed {
    track_id: String,
    dir: Option<String>,
    to_trash: bool,
}

/// The core's download table.
#[derive(Default)]
pub struct Downloads {
    active: HashMap<String, Download>,
    doomed: HashMap<JobId, Doomed>,
}

impl Downloads {
    pub fn insert(&mut self, track_id: &str, job: JobId, album_id: Option<i64>) {
        self.active.insert(
            track_id.to_string(),
            Download {
                job,
                album_id,
                path: None,
                dir: None,
                last_progress: None,
            },
        );
    }

    pub fn get(&self, track_id: &str) -> Option<&Download> {
        self.active.get(track_id)
    }

    pub fn get_mut(&mut self, track_id: &str) -> Option<&mut Download> {
        self.active.get_mut(track_id)
    }

    pub fn is_active(&self, track_id: &str, job: JobId) -> bool {
        self.active.get(track_id).is_some_and(|d| d.job == job)
    }

    /// The job ended: stop tracking it.
    pub fn finish(&mut self, track_id: &str) -> Option<Download> {
        self.active.remove(track_id)
    }

    /// The track is being deleted: returns the job to cancel, if one is in
    /// flight.
    pub fn doom(&mut self, track_id: &str, to_trash: bool) -> Option<JobId> {
        let d = self.active.remove(track_id)?;
        self.doomed.insert(
            d.job,
            Doomed {
                track_id: track_id.to_string(),
                dir: d.dir,
                to_trash,
            },
        );
        Some(d.job)
    }

    pub fn is_doomed(&self, job: JobId) -> bool {
        self.doomed.contains_key(&job)
    }

    pub fn doomed_started(&mut self, job: JobId, dir: String) {
        if let Some(d) = self.doomed.get_mut(&job) {
            d.dir = Some(dir);
        }
    }

    /// The doomed job ended: its folder (if known) and the delete's
    /// `to_trash`.
    pub fn take_doomed(&mut self, job: JobId) -> Option<(Option<String>, bool)> {
        self.doomed.remove(&job).map(|d| (d.dir, d.to_trash))
    }

    /// Track ids with a job in flight; reconcile must not touch their folders.
    pub fn in_flight(&self) -> HashSet<String> {
        self.active
            .keys()
            .cloned()
            .chain(self.doomed.values().map(|d| d.track_id.clone()))
            .collect()
    }

    /// The growing audio file of a download whose `Started` arrived.
    pub fn streaming_path(&self, track_id: &str) -> Option<&str> {
        self.active.get(track_id)?.path.as_deref()
    }
}

/// The URL handed to the worker for a video id.
pub fn canonical_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

pub fn url_error(e: UrlError) -> (ErrorCode, &'static str) {
    match e {
        UrlError::PlaylistOnly => (ErrorCode::UnsupportedUrl, "Playlists are not supported yet"),
        UrlError::NotYouTube => (ErrorCode::InvalidUrl, "Not a YouTube URL"),
        UrlError::NoVideoId => (ErrorCode::InvalidUrl, "No YouTube video id in the URL"),
    }
}

/// Existing files of a track: its per-track folder (parent of `audio_path`
/// named `… [<id8>]`), else a legacy flat audio file plus `<id>.json`.
pub fn track_files(track: &Track) -> Vec<PathBuf> {
    let Some(audio) = track.audio_path.as_deref().map(Path::new) else {
        return Vec::new();
    };
    let id8: String = track.id.chars().take(8).collect();
    let suffix = format!("[{id8}]");
    let files = match audio.parent() {
        Some(dir)
            if dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix)) =>
        {
            vec![dir.to_path_buf()]
        }
        _ => vec![
            audio.to_path_buf(),
            audio.with_file_name(format!("{}.json", track.id)),
        ],
    };
    files.into_iter().filter(|p| p.exists()).collect()
}

/// Move `path` to the Trash, or delete it permanently.
pub fn remove_path(path: &Path, to_trash: bool) -> anyhow::Result<()> {
    if to_trash {
        trash_context().delete(path)?;
    } else if path.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// On macOS: NSFileManager, never the Finder/AppleScript method.
fn trash_context() -> trash::TrashContext {
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx
    }
    #[cfg(not(target_os = "macos"))]
    trash::TrashContext::default()
}
