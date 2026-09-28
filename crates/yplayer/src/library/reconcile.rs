use anyhow::Result;
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::db::Db;
use crate::types::{Track, TrackState};

const AUDIO_EXTS: &[&str] = &[
    "mp3", "m4a", "opus", "flac", "wav", "webm", "ogg", "oga", "aac",
];
const COVER_EXTS: &[&str] = &["webp", "jpg", "jpeg", "png"];

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReconcileReport {
    pub imported: Vec<String>,
    pub removed: Vec<String>,
    /// Existing tracks whose `thumb_path` was filled in (see `attach_covers`).
    pub updated: Vec<String>,
    pub skipped_dirs: Vec<PathBuf>,
    pub deleted_partials: Vec<PathBuf>,
}

/// Bring the DB in line with the cache dir: import entries no row references,
/// drop partial download dirs, and delete complete rows whose file is gone.
/// Never deletes a directory that has a `meta.json`.
pub fn reconcile(
    cache_dir: &Path,
    db: &Db,
    in_flight: &HashSet<String>,
) -> Result<ReconcileReport> {
    let tracks = db.list_tracks()?;
    let mut ids: HashSet<String> = HashSet::with_capacity(tracks.len());
    // Per-track rows make their folder known, flat rows their file.
    let mut known: HashSet<PathBuf> = HashSet::with_capacity(tracks.len() * 2);
    let mut partial_id8s: HashSet<&str> = HashSet::new();
    for t in &tracks {
        ids.insert(t.id.clone());
        if let Some(p) = &t.audio_path {
            let p = Path::new(p);
            if let Some(parent) = p.parent() {
                known.insert(parent.to_path_buf());
            }
            known.insert(p.to_path_buf());
        }
        if matches!(t.state, TrackState::Downloading | TrackState::Failed)
            && !in_flight.contains(&t.id)
        {
            partial_id8s.insert(id8(&t.id));
        }
    }

    let mut report = ReconcileReport::default();
    for entry in fs::read_dir(cache_dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if known.contains(&path) {
            continue;
        }
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            let meta_path = path.join("meta.json");
            if meta_path.exists() {
                match import_folder(&path, &meta_path) {
                    Some(track) => {
                        if ids.insert(track.id.clone()) {
                            db.upsert_track(&track)?;
                            report.imported.push(track.id);
                        }
                    }
                    None => report.skipped_dirs.push(path),
                }
            } else if dir_id8(&path).is_some_and(|s| partial_id8s.contains(s)) {
                fs::remove_dir_all(&path)?;
                report.deleted_partials.push(path);
            } else {
                report.skipped_dirs.push(path);
            }
        } else if file_type.is_file()
            && let Some(track) = import_flat(&path)
            && ids.insert(track.id.clone())
        {
            db.upsert_track(&track)?;
            report.imported.push(track.id);
        }
    }

    for t in &tracks {
        if t.state == TrackState::Complete
            && let Some(p) = &t.audio_path
            && !Path::new(p).exists()
        {
            db.delete_track(&t.id)?;
            report.removed.push(t.id.clone());
        }
    }

    report.updated = attach_covers(db, &report.removed)?;
    Ok(report)
}

/// Complete per-track-folder tracks without a `thumb_path`: use an existing `cover.*` in the
/// folder, else extract the cover embedded in an mp3's ID3 tag (the old pipeline embedded it
/// instead of writing a sidecar) to `cover.<png|jpg>`. Never overwrites a file. Returns the ids
/// whose `thumb_path` was set.
fn attach_covers(db: &Db, removed: &[String]) -> Result<Vec<String>> {
    let mut updated = Vec::new();
    for t in db.list_tracks()? {
        if t.state != TrackState::Complete || t.thumb_path.is_some() || removed.contains(&t.id) {
            continue;
        }
        let Some(audio) = t.audio_path.as_deref().map(Path::new) else {
            continue;
        };
        let Some(dir) = audio.parent().filter(|d| dir_id8(d) == Some(id8(&t.id))) else {
            continue;
        };
        let cover = match find_cover(dir) {
            Some(existing) => existing,
            None => match extract_embedded_cover(audio, dir) {
                Some(written) => written,
                None => continue,
            },
        };
        db.set_thumb_path(&t.id, &cover.to_string_lossy())?;
        updated.push(t.id);
    }
    Ok(updated)
}

fn find_cover(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .find_map(|e| {
            let path = e.path();
            let is_cover = path.file_stem().is_some_and(|s| s == "cover")
                && lower_ext(&path).is_some_and(|x| COVER_EXTS.contains(&x.as_str()));
            is_cover.then_some(path)
        })
}

/// The front cover (else the first picture) of an mp3's ID3 tag, written atomically to
/// `<dir>/cover.<png|jpg>`; None when there is no tag, picture, or known image type.
fn extract_embedded_cover(audio: &Path, dir: &Path) -> Option<PathBuf> {
    if lower_ext(audio).as_deref() != Some("mp3") {
        return None;
    }
    let tag = id3::Tag::read_from_path(audio).ok()?;
    let picture = tag
        .pictures()
        .find(|p| p.picture_type == id3::frame::PictureType::CoverFront)
        .or_else(|| tag.pictures().next())?;
    let ext = match picture.mime_type.to_ascii_lowercase().as_str() {
        "image/png" | "png" => "png",
        "image/jpeg" | "image/jpg" | "jpg" | "jpeg" => "jpg",
        _ => return None,
    };
    let dest = dir.join(format!("cover.{ext}"));
    let tmp = dir.join(format!("cover.{ext}.tmp"));
    fs::write(&tmp, &picture.data).ok()?;
    fs::rename(&tmp, &dest).ok()?;
    Some(dest)
}

/// Rows left in state downloading by a previous run: delete their partial
/// `[<id8>]` folder and mark them failed. Returns the affected ids.
pub fn recover_interrupted(cache_dir: &Path, db: &Db) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for t in db.tracks_in_state(TrackState::Downloading)? {
        if let Some(dir) = t.audio_path.as_deref().and_then(|p| Path::new(p).parent())
            && dir.parent() == Some(cache_dir)
            && dir_id8(dir) == Some(id8(&t.id))
            && dir.is_dir()
            && !dir.join("meta.json").exists()
        {
            fs::remove_dir_all(dir)?;
        }
        db.mark_failed(&t.id)?;
        ids.push(t.id);
    }
    Ok(ids)
}

/// The 8-char id prefix yt-dlp puts in folder names (`%(id).8s`).
fn id8(id: &str) -> &str {
    id.char_indices().nth(8).map_or(id, |(i, _)| &id[..i])
}

/// The bracketed suffix of a `<Title> [<id8>]` folder name.
fn dir_id8(dir: &Path) -> Option<&str> {
    let name = dir.file_name()?.to_str()?.strip_suffix(']')?;
    let start = name.rfind('[')?;
    Some(&name[start + 1..])
}

fn read_json(path: &Path) -> Option<Value> {
    fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

fn lower_ext(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
}

/// A per-track folder: `meta.json` + audio file + optional `cover.*`.
fn import_folder(dir: &Path, meta_path: &Path) -> Option<Track> {
    let meta = read_json(meta_path)?;
    let id = meta.get("id")?.as_str()?.to_string();
    let mut audio = None;
    let mut cover = None;
    for entry in fs::read_dir(dir).ok()?.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = lower_ext(&path) else {
            continue;
        };
        if audio.is_none() && AUDIO_EXTS.contains(&ext.as_str()) {
            audio = Some((path, ext));
        } else if cover.is_none()
            && COVER_EXTS.contains(&ext.as_str())
            && path.file_stem().is_some_and(|s| s == "cover")
        {
            cover = Some(path);
        }
    }
    let (audio, ext) = audio?;
    Some(track_from_meta(Some(&meta), &id, &audio, ext, cover))
}

/// A legacy flat `<id>.<ext>` audio file with an optional `<id>.json` sidecar.
fn import_flat(path: &Path) -> Option<Track> {
    let ext = lower_ext(path)?;
    if !AUDIO_EXTS.contains(&ext.as_str()) {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let meta = read_json(&path.with_extension("json"));
    Some(track_from_meta(meta.as_ref(), stem, path, ext, None))
}

fn track_from_meta(
    meta: Option<&Value>,
    fallback_id: &str,
    audio: &Path,
    ext: String,
    cover: Option<PathBuf>,
) -> Track {
    let field = |k: &str| meta.and_then(|m| m.get(k));
    let text = |k: &str| field(k).and_then(|v| v.as_str()).map(String::from);
    let id = text("id").unwrap_or_else(|| fallback_id.to_string());
    Track {
        title: text("title").unwrap_or_else(|| id.clone()),
        uploader: text("uploader"),
        duration: field("duration").and_then(|v| v.as_i64()),
        webpage_url: text("webpage_url"),
        audio_path: Some(audio.to_string_lossy().into_owned()),
        format: Some(ext),
        file_size: fs::metadata(audio).ok().map(|m| m.len() as i64),
        added_at: None,
        last_played: None,
        state: TrackState::Complete,
        thumb_path: cover.map(|p| p.to_string_lossy().into_owned()),
        id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use id3::TagLike;
    use std::time::Instant;

    fn setup() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join(".yplayer.db"), dir.path()).unwrap();
        (dir, db)
    }

    fn row(id: &str, audio_path: Option<&Path>, state: TrackState) -> Track {
        Track {
            id: id.to_string(),
            title: id.to_string(),
            uploader: None,
            duration: None,
            webpage_url: None,
            audio_path: audio_path.map(|p| p.to_string_lossy().into_owned()),
            format: None,
            file_size: None,
            added_at: None,
            last_played: None,
            state,
            thumb_path: None,
        }
    }

    fn write_meta(dir: &Path, id: &str, title: &str) {
        fs::write(
            dir.join("meta.json"),
            format!(
                r#"{{"id": "{id}", "title": "{title}", "uploader": "ZUTOMAYO", "duration": 215, "webpage_url": "https://www.youtube.com/watch?v={id}"}}"#
            ),
        )
        .unwrap();
    }

    fn path_str(p: &Path) -> Option<String> {
        Some(p.to_string_lossy().into_owned())
    }

    fn none() -> HashSet<String> {
        HashSet::new()
    }

    /// An mp3 that is only an ID3v2.4 tag (plus a few audio bytes) carrying `pictures`.
    fn mp3_with_pictures(path: &Path, pictures: &[(&str, id3::frame::PictureType, &[u8])]) {
        fs::write(path, b"\xff\xfb\x90\x00audio").unwrap();
        let mut tag = id3::Tag::new();
        for (mime, kind, data) in pictures {
            tag.add_frame(id3::frame::Picture {
                mime_type: mime.to_string(),
                picture_type: *kind,
                description: String::new(),
                data: data.to_vec(),
            });
        }
        tag.write_to_path(path, id3::Version::Id3v24).unwrap();
    }

    #[test]
    fn extracts_embedded_mp3_cover_once() {
        use id3::frame::PictureType;
        let (dir, db) = setup();
        let folder = dir.path().join("ハム [ouLndhBR]");
        fs::create_dir(&folder).unwrap();
        let audio = folder.join("audio.mp3");
        mp3_with_pictures(
            &audio,
            &[
                ("image/jpeg", PictureType::Other, b"other"),
                ("image/png", PictureType::CoverFront, b"\x89PNGfront"),
            ],
        );
        write_meta(&folder, "ouLndhBRL4w", "ハム");
        db.upsert_track(&row("ouLndhBRL4w", Some(&audio), TrackState::Complete))
            .unwrap();

        let report = reconcile(dir.path(), &db, &none()).unwrap();
        assert_eq!(report.updated, ["ouLndhBRL4w"]);
        let cover = folder.join("cover.png");
        assert_eq!(fs::read(&cover).unwrap(), b"\x89PNGfront");
        let t = db.get_track("ouLndhBRL4w").unwrap().unwrap();
        assert_eq!(t.thumb_path, path_str(&cover));
        assert!(!folder.join("cover.png.tmp").exists());

        // thumb_path is set now: a second pass does nothing.
        assert!(
            reconcile(dir.path(), &db, &none())
                .unwrap()
                .updated
                .is_empty()
        );
    }

    #[test]
    fn uses_existing_cover_and_never_overwrites_it() {
        use id3::frame::PictureType;
        let (dir, db) = setup();
        let folder = dir.path().join("Song [abcdefgh]");
        fs::create_dir(&folder).unwrap();
        let audio = folder.join("audio.mp3");
        mp3_with_pictures(
            &audio,
            &[("image/png", PictureType::CoverFront, b"embedded")],
        );
        fs::write(folder.join("cover.jpg"), b"sidecar").unwrap();
        db.upsert_track(&row("abcdefghijk", Some(&audio), TrackState::Complete))
            .unwrap();

        let report = reconcile(dir.path(), &db, &none()).unwrap();
        assert_eq!(report.updated, ["abcdefghijk"]);
        assert_eq!(fs::read(folder.join("cover.jpg")).unwrap(), b"sidecar");
        assert!(!folder.join("cover.png").exists());
        let t = db.get_track("abcdefghijk").unwrap().unwrap();
        assert_eq!(t.thumb_path, path_str(&folder.join("cover.jpg")));
    }

    #[test]
    fn skips_mp3_without_picture_and_flat_legacy_files() {
        let (dir, db) = setup();
        let folder = dir.path().join("Song [abcdefgh]");
        fs::create_dir(&folder).unwrap();
        let bare = folder.join("audio.mp3");
        mp3_with_pictures(&bare, &[]);
        db.upsert_track(&row("abcdefghijk", Some(&bare), TrackState::Complete))
            .unwrap();
        let flat = dir.path().join("zyxwvutsrqp.mp3");
        mp3_with_pictures(
            &flat,
            &[("image/png", id3::frame::PictureType::CoverFront, b"x")],
        );
        db.upsert_track(&row("zyxwvutsrqp", Some(&flat), TrackState::Complete))
            .unwrap();

        let report = reconcile(dir.path(), &db, &none()).unwrap();
        assert!(report.updated.is_empty());
        assert!(!folder.join("cover.png").exists());
        assert!(!dir.path().join("cover.png").exists());
    }

    #[test]
    fn imports_unknown_track_folder_with_meta_audio_and_cover() {
        let (dir, db) = setup();
        let folder = dir.path().join("秒針を噛む [GJI4Gv7N]");
        fs::create_dir(&folder).unwrap();
        write_meta(&folder, "GJI4Gv7NbmE", "秒針を噛む");
        fs::write(folder.join("audio.webm"), b"12345").unwrap();
        fs::write(folder.join("cover.webp"), b"img").unwrap();

        let report = reconcile(dir.path(), &db, &none()).unwrap();
        assert_eq!(report.imported, ["GJI4Gv7NbmE"]);
        let t = db.get_track("GJI4Gv7NbmE").unwrap().unwrap();
        assert_eq!(t.title, "秒針を噛む");
        assert_eq!(t.uploader.as_deref(), Some("ZUTOMAYO"));
        assert_eq!(t.duration, Some(215));
        assert_eq!(
            t.webpage_url.as_deref(),
            Some("https://www.youtube.com/watch?v=GJI4Gv7NbmE")
        );
        assert_eq!(t.audio_path, path_str(&folder.join("audio.webm")));
        assert_eq!(t.thumb_path, path_str(&folder.join("cover.webp")));
        assert_eq!(t.format.as_deref(), Some("webm"));
        assert_eq!(t.file_size, Some(5));
        assert_eq!(t.state, TrackState::Complete);

        // The folder is now referenced by a row, so a second pass is a no-op.
        assert_eq!(
            reconcile(dir.path(), &db, &none()).unwrap(),
            ReconcileReport::default()
        );
    }

    #[test]
    fn ignores_folder_already_referenced_by_a_row() {
        let (dir, db) = setup();
        let folder = dir.path().join("Known [aaaaaaaa]");
        fs::create_dir(&folder).unwrap();
        write_meta(&folder, "zzzzzzzzzzz", "From disk");
        let audio = folder.join("audio.mp3");
        fs::write(&audio, b"x").unwrap();
        let mut known = row("aaaaaaaaaaa", Some(&audio), TrackState::Complete);
        known.title = "From DB".to_string();
        db.upsert_track(&known).unwrap();

        let report = reconcile(dir.path(), &db, &none()).unwrap();
        assert_eq!(report, ReconcileReport::default());
        assert_eq!(db.get_track("zzzzzzzzzzz").unwrap(), None);
        assert_eq!(
            db.get_track("aaaaaaaaaaa").unwrap().unwrap().title,
            "From DB"
        );
    }

    #[test]
    fn imports_legacy_flat_files_with_and_without_sidecar() {
        let (dir, db) = setup();
        let bare = dir.path().join("flat0000001.mp3");
        let with_sidecar = dir.path().join("flat0000002.m4a");
        fs::write(&bare, b"abc").unwrap();
        fs::write(&with_sidecar, b"abcd").unwrap();
        fs::write(
            dir.path().join("flat0000002.json"),
            r#"{"id": "flat0000002", "title": "Sidecar title", "uploader": "U", "duration": 100}"#,
        )
        .unwrap();

        let mut report = reconcile(dir.path(), &db, &none()).unwrap();
        report.imported.sort();
        assert_eq!(report.imported, ["flat0000001", "flat0000002"]);

        let a = db.get_track("flat0000001").unwrap().unwrap();
        assert_eq!(a.title, "flat0000001");
        assert_eq!(a.uploader, None);
        assert_eq!(a.audio_path, path_str(&bare));
        assert_eq!(a.format.as_deref(), Some("mp3"));
        assert_eq!(a.file_size, Some(3));

        let b = db.get_track("flat0000002").unwrap().unwrap();
        assert_eq!(b.title, "Sidecar title");
        assert_eq!(b.uploader.as_deref(), Some("U"));
        assert_eq!(b.duration, Some(100));
        assert_eq!(b.audio_path, path_str(&with_sidecar));
        assert_eq!(b.format.as_deref(), Some("m4a"));

        assert_eq!(
            reconcile(dir.path(), &db, &none()).unwrap(),
            ReconcileReport::default()
        );
    }

    #[test]
    fn leaves_unknown_dir_without_meta_json_in_skipped_dirs() {
        let (dir, db) = setup();
        let plain = dir.path().join("My stuff");
        fs::create_dir(&plain).unwrap();
        fs::write(plain.join("notes.txt"), b"keep me").unwrap();
        let bracketed = dir.path().join("Other [zzzzzzzz]");
        fs::create_dir(&bracketed).unwrap();

        let mut report = reconcile(dir.path(), &db, &none()).unwrap();
        report.skipped_dirs.sort();
        let mut expected = vec![plain.clone(), bracketed.clone()];
        expected.sort();
        assert_eq!(report.skipped_dirs, expected);
        assert!(report.imported.is_empty());
        assert!(report.deleted_partials.is_empty());
        assert!(plain.join("notes.txt").exists());
        assert!(bracketed.exists());
    }

    #[test]
    fn deletes_partial_dir_of_failed_row_unless_in_flight() {
        let (dir, db) = setup();
        db.upsert_track(&row("abcdefghijk", None, TrackState::Failed))
            .unwrap();
        let partial = dir.path().join("Title [abcdefgh]");
        fs::create_dir(&partial).unwrap();
        fs::write(partial.join("audio.webm"), b"x").unwrap();

        let in_flight: HashSet<String> = ["abcdefghijk".to_string()].into();
        let report = reconcile(dir.path(), &db, &in_flight).unwrap();
        assert!(partial.exists());
        assert_eq!(report.skipped_dirs, std::slice::from_ref(&partial));
        assert!(report.deleted_partials.is_empty());

        let report = reconcile(dir.path(), &db, &none()).unwrap();
        assert!(!partial.exists());
        assert_eq!(report.deleted_partials, [partial]);
        assert!(report.skipped_dirs.is_empty());
        assert!(db.get_track("abcdefghijk").unwrap().is_some());
    }

    #[test]
    fn never_deletes_a_dir_containing_meta_json() {
        let (dir, db) = setup();
        db.upsert_track(&row("abcdefghijk", None, TrackState::Failed))
            .unwrap();
        let folder = dir.path().join("Title [abcdefgh]");
        fs::create_dir(&folder).unwrap();
        write_meta(&folder, "abcdefghijk", "Title");
        let audio = folder.join("audio.webm");
        fs::write(&audio, b"x").unwrap();

        let report = reconcile(dir.path(), &db, &none()).unwrap();
        assert!(folder.join("meta.json").exists());
        assert!(report.deleted_partials.is_empty());
        assert!(report.imported.is_empty());

        db.upsert_track(&row("abcdefghijk", Some(&audio), TrackState::Downloading))
            .unwrap();
        assert_eq!(
            recover_interrupted(dir.path(), &db).unwrap(),
            ["abcdefghijk"]
        );
        assert!(folder.join("meta.json").exists());
    }

    #[test]
    fn removes_complete_rows_whose_file_vanished_and_keeps_downloading_rows() {
        let (dir, db) = setup();
        let gone = dir.path().join("Gone [gone0000]").join("audio.mp3");
        db.upsert_track(&row("gone0000000", Some(&gone), TrackState::Complete))
            .unwrap();
        let downloading = dir.path().join("Dl [dl000000]").join("audio.webm");
        db.upsert_track(&row(
            "dl000000000",
            Some(&downloading),
            TrackState::Downloading,
        ))
        .unwrap();
        let kept_dir = dir.path().join("Kept [kept0000]");
        fs::create_dir(&kept_dir).unwrap();
        write_meta(&kept_dir, "kept0000000", "Kept");
        let kept = kept_dir.join("audio.mp3");
        fs::write(&kept, b"x").unwrap();
        db.upsert_track(&row("kept0000000", Some(&kept), TrackState::Complete))
            .unwrap();

        let report = reconcile(dir.path(), &db, &none()).unwrap();
        assert_eq!(report.removed, ["gone0000000"]);
        assert_eq!(db.get_track("gone0000000").unwrap(), None);
        assert!(db.get_track("dl000000000").unwrap().is_some());
        assert!(db.get_track("kept0000000").unwrap().is_some());
    }

    #[test]
    fn recover_interrupted_fails_downloading_rows_and_deletes_their_dir() {
        let (dir, db) = setup();
        let partial = dir.path().join("Title [abcdefgh]");
        fs::create_dir(&partial).unwrap();
        fs::write(partial.join("audio.webm"), b"x").unwrap();
        db.upsert_track(&row(
            "abcdefghijk",
            Some(&partial.join("audio.webm")),
            TrackState::Downloading,
        ))
        .unwrap();
        // A dir whose name does not carry the row's id8 is left alone.
        let other = dir.path().join("Unrelated [zzzzzzzz]");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("audio.webm"), b"x").unwrap();
        db.upsert_track(&row(
            "mismatch000",
            Some(&other.join("audio.webm")),
            TrackState::Downloading,
        ))
        .unwrap();
        db.insert_pending("pending0000", "https://youtu.be/pending0000")
            .unwrap();

        let mut ids = recover_interrupted(dir.path(), &db).unwrap();
        ids.sort();
        assert_eq!(ids, ["abcdefghijk", "mismatch000", "pending0000"]);
        assert!(!partial.exists());
        assert!(other.exists());
        assert!(
            db.tracks_in_state(TrackState::Downloading)
                .unwrap()
                .is_empty()
        );
        assert_eq!(db.tracks_in_state(TrackState::Failed).unwrap().len(), 3);
    }

    #[test]
    #[ignore = "timing benchmark; run with --release -- --ignored --nocapture"]
    fn reconcile_5000_known_folders_timing() {
        let (dir, db) = setup();
        for i in 0..5000 {
            let id = format!("{i:011}");
            let folder = dir.path().join(format!("Track {i} [{}]", &id[..8]));
            fs::create_dir(&folder).unwrap();
            write_meta(&folder, &id, &format!("Track {i}"));
            let audio = folder.join("audio.webm");
            fs::write(&audio, b"x").unwrap();
            db.upsert_track(&row(&id, Some(&audio), TrackState::Complete))
                .unwrap();
        }

        let start = Instant::now();
        let report = reconcile(dir.path(), &db, &HashSet::new()).unwrap();
        let elapsed = start.elapsed();
        println!(
            "reconcile over 5000 known folders: {:.2} ms",
            elapsed.as_secs_f64() * 1000.0
        );
        assert_eq!(report, ReconcileReport::default());
    }
}
