use std::fmt;
use std::fs;
use std::path::Path;

/// How a per-track folder is removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveMode {
    /// Automatic cleanup (failed download, reconcile, startup recovery):
    /// permanent, and only when the folder holds nothing but pipeline files.
    Cleanup,
    /// An explicit user delete: to the Trash or permanent.
    Delete { to_trash: bool },
}

#[derive(Debug)]
pub enum RemoveError {
    /// A safety check failed; nothing was touched.
    Refused(String),
    /// The delete itself failed.
    Failed(String),
}

impl fmt::Display for RemoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemoveError::Refused(why) => write!(f, "refused: {why}"),
            RemoveError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for RemoveError {}

/// Remove the per-track folder `dir` of `track_id`. Refused unless it is not
/// a symlink, its canonical parent is the canonical `cache_dir` and its name
/// ends with `[<first 8 chars of track_id>]`; `Cleanup` also refuses a folder
/// holding anything but files the download pipeline creates.
pub fn remove_track_dir(
    cache_dir: &Path,
    dir: &Path,
    track_id: &str,
    mode: RemoveMode,
) -> Result<(), RemoveError> {
    let meta = fs::symlink_metadata(dir).map_err(failed)?;
    if meta.file_type().is_symlink() {
        return Err(RemoveError::Refused("it is a symlink".into()));
    }
    if !meta.is_dir() {
        return Err(RemoveError::Refused("it is not a folder".into()));
    }
    let dir = fs::canonicalize(dir).map_err(failed)?;
    let cache = fs::canonicalize(cache_dir).map_err(failed)?;
    if dir.parent() != Some(cache.as_path()) {
        return Err(RemoveError::Refused(format!(
            "it is not a folder directly inside {}",
            cache.display()
        )));
    }
    let suffix = format!("[{}]", track_id.chars().take(8).collect::<String>());
    if !dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(&suffix))
    {
        return Err(RemoveError::Refused(format!(
            "its name does not end with {suffix}"
        )));
    }
    match mode {
        RemoveMode::Cleanup => {
            for entry in fs::read_dir(&dir).map_err(failed)? {
                let entry = entry.map_err(failed)?;
                let name = entry.file_name();
                let is_dir = entry.file_type().map_err(failed)?.is_dir();
                if is_dir || !name.to_str().is_some_and(is_pipeline_file) {
                    return Err(RemoveError::Refused(format!(
                        "it holds {}, which the download pipeline does not create",
                        name.to_string_lossy()
                    )));
                }
            }
            fs::remove_dir_all(&dir).map_err(failed)
        }
        RemoveMode::Delete { to_trash } => remove_path(&dir, to_trash),
    }
}

/// Whether `path` is a file (not a symlink) inside `cache_dir`.
pub fn file_in_cache(cache_dir: &Path, path: &Path) -> bool {
    let is_file = fs::symlink_metadata(path).is_ok_and(|m| m.is_file());
    let parent = path.parent().and_then(|p| fs::canonicalize(p).ok());
    let cache = fs::canonicalize(cache_dir).ok();
    is_file && parent.zip(cache).is_some_and(|(p, c)| p.starts_with(c))
}

/// Move `path` to the Trash, or delete it permanently.
pub fn remove_path(path: &Path, to_trash: bool) -> Result<(), RemoveError> {
    if to_trash {
        trash_context()
            .delete(path)
            .map_err(|e| RemoveError::Failed(e.to_string()))
    } else if path.is_dir() {
        fs::remove_dir_all(path).map_err(failed)
    } else {
        fs::remove_file(path).map_err(failed)
    }
}

/// `audio.*`, `cover.{jpg,png,webp}`, `*.tmp`, `*.part`, `*.ytdl`, `meta.json`.
fn is_pipeline_file(name: &str) -> bool {
    name.starts_with("audio.")
        || matches!(name, "cover.jpg" | "cover.png" | "cover.webp" | "meta.json")
        || [".tmp", ".part", ".ytdl"].iter().any(|s| name.ends_with(s))
}

fn failed(e: std::io::Error) -> RemoveError {
    RemoveError::Failed(e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    const ID: &str = "abcdefghijk";
    const CLEANUP: RemoveMode = RemoveMode::Cleanup;
    const DELETE: RemoveMode = RemoveMode::Delete { to_trash: false };

    /// A temp dir holding `cache` (named like a track folder, so only the
    /// parent check can refuse it) and an unrelated `outside` folder.
    fn setup() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("yt [abcdefgh]");
        fs::create_dir(&cache).unwrap();
        (root, cache)
    }

    fn folder(parent: &Path, name: &str, files: &[&str]) -> PathBuf {
        let dir = parent.join(name);
        fs::create_dir(&dir).unwrap();
        for f in files {
            fs::write(dir.join(f), b"x").unwrap();
        }
        dir
    }

    fn refused(r: Result<(), RemoveError>) -> bool {
        matches!(r, Err(RemoveError::Refused(_)))
    }

    #[test]
    fn refuses_the_cache_root() {
        let (_root, cache) = setup();
        fs::write(cache.join("audio.webm"), b"x").unwrap();
        for mode in [CLEANUP, DELETE] {
            assert!(refused(remove_track_dir(&cache, &cache, ID, mode)));
        }
        assert!(cache.join("audio.webm").exists());
    }

    #[test]
    fn refuses_a_folder_outside_the_cache() {
        let (root, cache) = setup();
        let outside = folder(root.path(), "Song [abcdefgh]", &["audio.webm"]);
        for mode in [CLEANUP, DELETE] {
            assert!(refused(remove_track_dir(&cache, &outside, ID, mode)));
        }
        let dotted = cache.join("..").join("Song [abcdefgh]");
        assert!(refused(remove_track_dir(&cache, &dotted, ID, DELETE)));
        assert!(outside.join("audio.webm").exists());
    }

    #[test]
    fn refuses_a_symlink() {
        let (root, cache) = setup();
        let target = folder(root.path(), "target", &["audio.webm"]);
        let link = cache.join("Song [abcdefgh]");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        for mode in [CLEANUP, DELETE] {
            assert!(refused(remove_track_dir(&cache, &link, ID, mode)));
        }
        assert!(link.exists());
        assert!(target.join("audio.webm").exists());
    }

    #[test]
    fn refuses_a_wrong_suffix() {
        let (_root, cache) = setup();
        for name in ["Song [zzzzzzzz]", "Song", "Song [abcdefgh] x"] {
            let dir = folder(&cache, name, &["audio.webm"]);
            for mode in [CLEANUP, DELETE] {
                assert!(refused(remove_track_dir(&cache, &dir, ID, mode)), "{name}");
            }
            assert!(dir.join("audio.webm").exists());
        }
    }

    #[test]
    fn cleanup_refuses_a_folder_with_a_user_file() {
        let (_root, cache) = setup();
        let dir = folder(&cache, "Song [abcdefgh]", &["audio.webm", "notes.txt"]);
        assert!(refused(remove_track_dir(&cache, &dir, ID, CLEANUP)));
        assert!(dir.join("notes.txt").exists());
        let sub = folder(&cache, "Other [abcdefgh]", &["audio.webm"]);
        fs::create_dir(sub.join("nested")).unwrap();
        assert!(refused(remove_track_dir(&cache, &sub, ID, CLEANUP)));
        assert!(sub.join("nested").exists());
    }

    #[test]
    fn cleanup_removes_a_normal_partial() {
        let (_root, cache) = setup();
        let dir = folder(
            &cache,
            "秒針を噛む [abcdefgh]",
            &[
                "audio.webm",
                "audio.f251.webm.part",
                "audio.webm.ytdl",
                "audio.webp",
                "cover.jpg",
                "cover.png",
                "cover.webp",
                "cover.jpg.tmp",
                "meta.json",
                "meta.json.tmp",
            ],
        );
        remove_track_dir(&cache, &dir, ID, CLEANUP).unwrap();
        assert!(!dir.exists());
        assert!(cache.exists());
    }

    #[test]
    fn explicit_delete_removes_the_folder_with_any_content() {
        let (_root, cache) = setup();
        let dir = folder(&cache, "Song [abcdefgh]", &["audio.webm", "notes.txt"]);
        remove_track_dir(&cache, &dir, ID, DELETE).unwrap();
        assert!(!dir.exists());
        assert!(cache.exists());
    }
}
