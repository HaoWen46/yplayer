use anyhow::Result;
use serde_json::Value;
use std::path::Path;
use walkdir::WalkDir;

use super::index::CacheIndex;
use crate::types::Track;

const KNOWN_AUDIO_EXTS: &[&str] = &[
    "mp3", "m4a", "opus", "flac", "wav", "webm", "ogg", "oga", "aac",
];

/// Scan the cache directory and populate the SQLite index.
/// Handles both per-track folder layout and legacy flat layout.
/// Always runs but only inserts tracks not already in the DB (incremental).
pub fn scan_and_index(cache_dir: &Path, db: &CacheIndex) -> Result<usize> {
    // Load existing IDs so we skip already-indexed tracks
    let existing_ids = db.list_track_ids().unwrap_or_default();

    let mut tracks: Vec<Track> = Vec::new();
    let mut seen_ids: std::collections::HashSet<String> = existing_ids;

    // Pass 1: Per-track directories (contain meta.json)
    for entry in WalkDir::new(cache_dir).min_depth(1).max_depth(1) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_dir() {
            continue;
        }
        let dir = entry.path();
        let meta_path = dir.join("meta.json");
        if !meta_path.exists() {
            continue;
        }

        // Read meta.json
        let meta: Value = match std::fs::read_to_string(&meta_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
        {
            Some(v) => v,
            None => continue,
        };

        let id = match meta.get("id").and_then(|v| v.as_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };

        // Incremental: skip tracks already in the DB (or a second folder sharing
        // the same id). Without this, every launch re-parses the whole library
        // and the returned "new tracks" count is wrong.
        if seen_ids.contains(&id) {
            continue;
        }

        // Find audio file in the directory
        let audio_path = find_audio_in_dir(dir);
        let audio_path = match audio_path {
            Some(p) => p,
            None => continue,
        };

        let ext = Path::new(&audio_path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_lowercase());

        let file_size = std::fs::metadata(&audio_path).ok().map(|m| m.len() as i64);

        tracks.push(Track {
            id: id.clone(),
            title: meta
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string(),
            uploader: meta.get("uploader").and_then(|v| v.as_str()).map(String::from),
            duration: meta.get("duration").and_then(|v| v.as_i64()),
            webpage_url: meta.get("webpage_url").and_then(|v| v.as_str()).map(String::from),
            audio_path: Some(audio_path),
            format: ext,
            file_size,
            added_at: None,
            last_played: None,
        });
        seen_ids.insert(id);
    }

    // Pass 2: Legacy flat files (<id>.ext + <id>.json sidecars)
    if let Ok(entries) = std::fs::read_dir(cache_dir) {
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let ext = match path.extension().and_then(|e| e.to_str()) {
                Some(e) => e.to_lowercase(),
                None => continue,
            };
            if !KNOWN_AUDIO_EXTS.contains(&ext.as_str()) {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if seen_ids.contains(&stem) {
                continue;
            }

            // Try to read sidecar
            let sidecar_path = path.with_extension("json");
            let meta: Option<Value> = std::fs::read_to_string(&sidecar_path)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok());

            let id = meta
                .as_ref()
                .and_then(|m| m.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or(&stem)
                .to_string();

            if seen_ids.contains(&id) {
                continue;
            }

            let file_size = std::fs::metadata(&path).ok().map(|m| m.len() as i64);

            tracks.push(Track {
                id: id.clone(),
                title: meta
                    .as_ref()
                    .and_then(|m| m.get("title"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&stem)
                    .to_string(),
                uploader: meta
                    .as_ref()
                    .and_then(|m| m.get("uploader"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
                duration: meta.as_ref().and_then(|m| m.get("duration")).and_then(|v| v.as_i64()),
                webpage_url: meta
                    .as_ref()
                    .and_then(|m| m.get("webpage_url"))
                    .and_then(|v| v.as_str())
                    .map(String::from),
                audio_path: Some(path.to_string_lossy().to_string()),
                format: Some(ext),
                file_size,
                added_at: None,
                last_played: None,
            });
            seen_ids.insert(id);
        }
    }

    let count = tracks.len();
    db.bulk_insert_tracks(&tracks)?;

    // Pass 3: Albums
    let albums_dir = cache_dir.join("albums");
    if albums_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&albums_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if !path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with(".album.json"))
                    .unwrap_or(false)
                {
                    continue;
                }

                let data: Value = match std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                {
                    Some(v) => v,
                    None => continue,
                };

                let name = data
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown")
                    .to_string();
                let description = data
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let track_ids: Vec<(String, i32)> = data
                    .get("tracks")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .enumerate()
                            .filter_map(|(i, t)| {
                                let id = t.get("id")?.as_str()?.to_string();
                                let pos = t
                                    .get("order")
                                    .and_then(|v| v.as_i64())
                                    .unwrap_or(i as i64 + 1) as i32;
                                Some((id, pos))
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                db.bulk_insert_album(&name, &description, &track_ids).ok();
            }
        }
    }

    Ok(count)
}

fn find_audio_in_dir(dir: &Path) -> Option<String> {
    let entries = std::fs::read_dir(dir).ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if KNOWN_AUDIO_EXTS.contains(&ext.to_lowercase().as_str()) {
                    return Some(path.to_string_lossy().to_string());
                }
            }
        }
    }
    None
}
