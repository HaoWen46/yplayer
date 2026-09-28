use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::Context;

use crate::download::worker::{JobId, WorkerMsg};
use crate::library::safe_fs::{self, RemoveError, RemoveMode};
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

/// Remove a track's existing files: its per-track folder (parent of
/// `audio_path` named `… [<id8>]`) through `safe_fs`, else a legacy flat
/// audio file plus its `<audio stem>.json`, each only if it is a file inside
/// `cache_dir`. Refused paths are logged and left alone.
pub fn remove_track_files(cache_dir: &Path, track: &Track, to_trash: bool) -> anyhow::Result<()> {
    let Some(audio) = track.audio_path.as_deref().map(Path::new) else {
        return Ok(());
    };
    let id8: String = track.id.chars().take(8).collect();
    let suffix = format!("[{id8}]");
    match audio.parent() {
        Some(dir)
            if dir
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix)) =>
        {
            if std::fs::symlink_metadata(dir).is_err() {
                return Ok(());
            }
            let mode = RemoveMode::Delete { to_trash };
            match safe_fs::remove_track_dir(cache_dir, dir, &track.id, mode) {
                Err(RemoveError::Refused(why)) => {
                    eprintln!("not removing {}: {why}", dir.display());
                }
                r => r.with_context(|| format!("removing {}", dir.display()))?,
            }
        }
        _ => {
            for path in [audio.to_path_buf(), audio.with_extension("json")] {
                if std::fs::symlink_metadata(&path).is_err() {
                    continue;
                }
                if !safe_fs::file_in_cache(cache_dir, &path) {
                    eprintln!(
                        "not removing {}: not a file inside the cache",
                        path.display()
                    );
                    continue;
                }
                safe_fs::remove_path(&path, to_trash)
                    .with_context(|| format!("removing {}", path.display()))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TrackState;
    use std::fs;

    fn flat(id: &str, audio: &Path) -> Track {
        Track {
            id: id.to_string(),
            title: id.to_string(),
            uploader: None,
            duration: None,
            webpage_url: None,
            audio_path: Some(audio.to_string_lossy().into_owned()),
            format: Some("mp3".into()),
            file_size: None,
            added_at: None,
            last_played: None,
            state: TrackState::Complete,
            thumb_path: None,
        }
    }

    #[test]
    fn deleting_a_legacy_track_never_touches_files_outside_the_cache() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("cache");
        let outside = root.path().join("outside");
        fs::create_dir(&cache).unwrap();
        fs::create_dir(&outside).unwrap();
        let keep = outside.join("keep.json");
        fs::write(&keep, b"keep").unwrap();

        // The sidecar is `<audio stem>.json`, whatever the id says.
        let audio = cache.join("flat0000001.mp3");
        fs::write(&audio, b"x").unwrap();
        fs::write(cache.join("flat0000001.json"), b"{}").unwrap();
        remove_track_files(&cache, &flat("../outside/keep", &audio), false).unwrap();
        assert!(!audio.exists());
        assert!(!cache.join("flat0000001.json").exists());
        assert!(keep.exists());

        // A flat file outside the cache is left alone, sidecar included.
        let far = outside.join("song.mp3");
        fs::write(&far, b"x").unwrap();
        fs::write(outside.join("song.json"), b"{}").unwrap();
        remove_track_files(&cache, &flat("song0000001", &far), false).unwrap();
        assert!(far.exists());
        assert!(outside.join("song.json").exists());
        assert!(keep.exists());
    }
}
