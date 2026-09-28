use rusqlite::{Connection, ErrorCode, OptionalExtension, Row, params};
use serde_json::Value;
use std::fmt;
use std::path::Path;

use crate::types::{Album, Track, TrackMeta, TrackState};

#[derive(Debug)]
pub enum DbError {
    NotFound,
    Conflict,
    BadRequest(String),
    Sqlite(rusqlite::Error),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::NotFound => f.write_str("not found"),
            DbError::Conflict => f.write_str("conflict"),
            DbError::BadRequest(msg) => write!(f, "bad request: {msg}"),
            DbError::Sqlite(e) => write!(f, "sqlite: {e}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Sqlite(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(e)
    }
}

/// A cached lyrics lookup; `body: None` is a confirmed miss.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricsRow {
    pub synced: bool,
    pub body: Option<String>,
    pub fetched_at: i64,
}

const TRACK_COLS: &str = "id, title, uploader, duration, webpage_url, audio_path, format, file_size, added_at, last_played, state, thumb_path";

pub struct Db {
    conn: Connection,
}

impl Db {
    pub fn open(db_path: &Path, cache_dir: &Path) -> Result<Self, DbError> {
        let conn = Connection::open(db_path)?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;

        let idx = Self { conn };
        idx.create_tables()?;
        idx.migrate(cache_dir)?;
        // The ON DELETE CASCADE was historically a no-op (foreign_keys defaulted
        // off), so old databases can carry album_tracks rows for deleted tracks.
        idx.cleanup_orphans()?;
        Ok(idx)
    }

    /// Remove album_tracks rows referencing tracks that no longer exist.
    fn cleanup_orphans(&self) -> Result<(), DbError> {
        self.conn.execute(
            "DELETE FROM album_tracks WHERE track_id NOT IN (SELECT id FROM tracks)",
            [],
        )?;
        Ok(())
    }

    /// The v0 schema; `migrate` brings it to the current version.
    fn create_tables(&self) -> Result<(), DbError> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS tracks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                uploader TEXT,
                duration INTEGER,
                webpage_url TEXT,
                audio_path TEXT NOT NULL,
                format TEXT,
                file_size INTEGER,
                added_at INTEGER NOT NULL,
                last_played INTEGER
            );

            CREATE TABLE IF NOT EXISTS albums (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                description TEXT DEFAULT '',
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS album_tracks (
                album_id INTEGER REFERENCES albums(id) ON DELETE CASCADE,
                track_id TEXT REFERENCES tracks(id) ON DELETE CASCADE,
                position INTEGER NOT NULL,
                PRIMARY KEY (album_id, track_id)
            );

            CREATE INDEX IF NOT EXISTS idx_tracks_title ON tracks(title COLLATE NOCASE);
            CREATE INDEX IF NOT EXISTS idx_tracks_uploader ON tracks(uploader COLLATE NOCASE);
            ",
        )?;
        Ok(())
    }

    fn migrate(&self, cache_dir: &Path) -> Result<(), DbError> {
        let version: i64 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version >= 1 {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        tx.execute_batch(
            "
            ALTER TABLE tracks ADD COLUMN state TEXT NOT NULL DEFAULT 'complete';
            ALTER TABLE tracks ADD COLUMN thumb_path TEXT;
            ALTER TABLE albums ADD COLUMN last_used_at INTEGER;
            CREATE INDEX IF NOT EXISTS idx_album_tracks_track ON album_tracks(track_id);
            CREATE TABLE IF NOT EXISTS lyrics (
                track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                synced INTEGER,
                body TEXT,
                fetched_at INTEGER NOT NULL
            );
            ",
        )?;
        // Legacy album files are imported exactly once, here; re-importing on
        // every launch (as the old scanner did) would undo album edits.
        self.import_album_files(cache_dir)?;
        tx.execute_batch("PRAGMA user_version = 1")?;
        tx.commit()?;
        Ok(())
    }

    /// Import `<cache_dir>/albums/*.album.json`, linking only tracks that exist.
    fn import_album_files(&self, cache_dir: &Path) -> Result<(), DbError> {
        let Ok(entries) = std::fs::read_dir(cache_dir.join("albums")) else {
            return Ok(());
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".album.json"))
            {
                continue;
            }
            let Some(data) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            else {
                continue;
            };

            let name = data
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("Unknown");
            let album_id = self.get_or_create_album(name)?;
            let mut stmt = self.conn.prepare_cached(
                "INSERT OR IGNORE INTO album_tracks (album_id, track_id, position)
                 SELECT ?1, id, ?3 FROM tracks WHERE id = ?2",
            )?;
            let tracks = data.get("tracks").and_then(|v| v.as_array());
            for (i, t) in tracks.into_iter().flatten().enumerate() {
                let Some(id) = t.get("id").and_then(|v| v.as_str()) else {
                    continue;
                };
                let position = t
                    .get("order")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(i as i64 + 1);
                stmt.execute(params![album_id, id, position])?;
            }
        }
        Ok(())
    }

    // --- Tracks ---

    pub fn get_track(&self, id: &str) -> Result<Option<Track>, DbError> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {TRACK_COLS} FROM tracks WHERE id = ?1"))?;
        Ok(stmt.query_row(params![id], track_from_row).optional()?)
    }

    pub fn list_tracks(&self) -> Result<Vec<Track>, DbError> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {TRACK_COLS} FROM tracks ORDER BY added_at DESC, title COLLATE NOCASE"
        ))?;
        let tracks = stmt
            .query_map([], track_from_row)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(tracks)
    }

    /// Track ids in library order (same order as `list_tracks`).
    pub fn library_order(&self) -> Result<Vec<String>, DbError> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT id FROM tracks ORDER BY added_at DESC, title COLLATE NOCASE")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    }

    /// New row: placeholder title, unknown path, state downloading. Existing
    /// row (e.g. a retry of a failed download): only the state changes.
    pub fn insert_pending(&self, id: &str, webpage_url: &str) -> Result<(), DbError> {
        self.conn
            .prepare_cached(
                "INSERT INTO tracks (id, title, webpage_url, audio_path, added_at, state)
                 VALUES (?1, ?1, ?2, '', ?3, 'downloading')
                 ON CONFLICT(id) DO UPDATE SET state = 'downloading'",
            )?
            .execute(params![id, webpage_url, now()])?;
        Ok(())
    }

    pub fn mark_started(
        &self,
        id: &str,
        meta: &TrackMeta,
        audio_path: &str,
    ) -> Result<(), DbError> {
        self.conn
            .prepare_cached(
                "UPDATE tracks SET title = ?2, uploader = ?3, duration = ?4, webpage_url = ?5, audio_path = ?6
                 WHERE id = ?1",
            )?
            .execute(params![
                id,
                meta.title,
                meta.uploader,
                meta.duration,
                meta.webpage_url,
                audio_path,
            ])?;
        Ok(())
    }

    pub fn mark_complete(
        &self,
        id: &str,
        meta: &TrackMeta,
        audio_path: &str,
        format: Option<&str>,
        file_size: Option<i64>,
        thumb_path: Option<&str>,
    ) -> Result<(), DbError> {
        self.conn
            .prepare_cached(
                "UPDATE tracks SET title = ?2, uploader = ?3, duration = ?4, webpage_url = ?5, audio_path = ?6,
                    format = ?7, file_size = ?8, thumb_path = ?9, state = 'complete'
                 WHERE id = ?1",
            )?
            .execute(params![
                id,
                meta.title,
                meta.uploader,
                meta.duration,
                meta.webpage_url,
                audio_path,
                format,
                file_size,
                thumb_path,
            ])?;
        Ok(())
    }

    pub fn mark_failed(&self, id: &str) -> Result<(), DbError> {
        self.conn
            .prepare_cached("UPDATE tracks SET state = 'failed' WHERE id = ?1")?
            .execute(params![id])?;
        Ok(())
    }

    pub fn upsert_track(&self, track: &Track) -> Result<(), DbError> {
        self.conn
            .prepare_cached(
                "INSERT INTO tracks (id, title, uploader, duration, webpage_url, audio_path, format, file_size, added_at, last_played, state, thumb_path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(id) DO UPDATE SET
                    title=excluded.title,
                    uploader=excluded.uploader,
                    duration=excluded.duration,
                    webpage_url=excluded.webpage_url,
                    audio_path=excluded.audio_path,
                    format=excluded.format,
                    file_size=excluded.file_size,
                    state=excluded.state,
                    thumb_path=excluded.thumb_path",
            )?
            .execute(params![
                track.id,
                track.title,
                track.uploader,
                track.duration,
                track.webpage_url,
                track.audio_path.as_deref().unwrap_or(""),
                track.format,
                track.file_size,
                track.added_at.unwrap_or_else(now),
                track.last_played,
                state_str(track.state),
                track.thumb_path,
            ])?;
        Ok(())
    }

    pub fn rename_track(&self, id: &str, title: &str) -> Result<(), DbError> {
        let n = self
            .conn
            .prepare_cached("UPDATE tracks SET title = ?1 WHERE id = ?2")?
            .execute(params![title, id])?;
        found(n)
    }

    /// Delete a track; album links and cached lyrics cascade.
    pub fn delete_track(&self, id: &str) -> Result<(), DbError> {
        let n = self
            .conn
            .prepare_cached("DELETE FROM tracks WHERE id = ?1")?
            .execute(params![id])?;
        found(n)
    }

    pub fn touch_last_played(&self, id: &str) -> Result<(), DbError> {
        self.conn
            .prepare_cached("UPDATE tracks SET last_played = ?1 WHERE id = ?2")?
            .execute(params![now(), id])?;
        Ok(())
    }

    pub fn tracks_in_state(&self, state: TrackState) -> Result<Vec<Track>, DbError> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {TRACK_COLS} FROM tracks WHERE state = ?1
             ORDER BY added_at DESC, title COLLATE NOCASE"
        ))?;
        let tracks = stmt
            .query_map(params![state_str(state)], track_from_row)?
            .filter_map(|r| r.ok())
            .collect();
        Ok(tracks)
    }

    // --- Albums ---

    fn album_track_ids(&self, album_id: i64) -> Result<Vec<String>, DbError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT track_id FROM album_tracks WHERE album_id = ?1 ORDER BY position",
        )?;
        let ids = stmt
            .query_map(params![album_id], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    }

    pub fn list_albums(&self) -> Result<Vec<Album>, DbError> {
        let rows: Vec<(i64, String, i64, Option<i64>)> = {
            let mut stmt = self.conn.prepare_cached(
                "SELECT id, name, created_at, last_used_at FROM albums ORDER BY name COLLATE NOCASE",
            )?;
            stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .filter_map(|r| r.ok())
            .collect()
        };

        let mut albums = Vec::with_capacity(rows.len());
        for (id, name, created_at, last_used_at) in rows {
            albums.push(Album {
                id,
                name,
                track_ids: self.album_track_ids(id)?,
                created_at,
                last_used_at,
            });
        }
        Ok(albums)
    }

    pub fn get_album(&self, id: i64) -> Result<Option<Album>, DbError> {
        let row: Option<(String, i64, Option<i64>)> = self
            .conn
            .prepare_cached("SELECT name, created_at, last_used_at FROM albums WHERE id = ?1")?
            .query_row(params![id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .optional()?;
        let Some((name, created_at, last_used_at)) = row else {
            return Ok(None);
        };
        Ok(Some(Album {
            id,
            name,
            track_ids: self.album_track_ids(id)?,
            created_at,
            last_used_at,
        }))
    }

    pub fn album_id_by_name(&self, name: &str) -> Result<Option<i64>, DbError> {
        Ok(self
            .conn
            .prepare_cached("SELECT id FROM albums WHERE name = ?1")?
            .query_row(params![name], |row| row.get::<_, i64>(0))
            .optional()?)
    }

    /// Create an album; a duplicate name is `DbError::Conflict`.
    pub fn create_album(&self, name: &str) -> Result<i64, DbError> {
        self.conn
            .prepare_cached("INSERT INTO albums (name, created_at) VALUES (?1, ?2)")?
            .execute(params![name, now()])
            .map_err(conflict_on(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Return the id of the album named `name`, creating it if absent.
    pub fn get_or_create_album(&self, name: &str) -> Result<i64, DbError> {
        match self.album_id_by_name(name)? {
            Some(id) => Ok(id),
            None => self.create_album(name),
        }
    }

    pub fn rename_album(&self, id: i64, name: &str) -> Result<(), DbError> {
        let n = self
            .conn
            .prepare_cached("UPDATE albums SET name = ?1 WHERE id = ?2")?
            .execute(params![name, id])
            .map_err(conflict_on(rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE))?;
        found(n)
    }

    /// Delete an album; its tracks stay in the library.
    pub fn delete_album(&self, id: i64) -> Result<(), DbError> {
        let n = self
            .conn
            .prepare_cached("DELETE FROM albums WHERE id = ?1")?
            .execute(params![id])?;
        found(n)
    }

    /// Append `track_id` to the album; false if it is already there.
    pub fn album_add(&self, album_id: i64, track_id: &str) -> Result<bool, DbError> {
        let n = self
            .conn
            .prepare_cached(
                "INSERT OR IGNORE INTO album_tracks (album_id, track_id, position)
                 SELECT ?1, ?2, COALESCE(MAX(position) + 1, 0) FROM album_tracks WHERE album_id = ?1",
            )?
            .execute(params![album_id, track_id])
            .map_err(|e| match e {
                rusqlite::Error::SqliteFailure(err, _)
                    if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY =>
                {
                    DbError::NotFound
                }
                e => DbError::Sqlite(e),
            })?;
        Ok(n == 1)
    }

    pub fn album_remove(&self, album_id: i64, track_id: &str) -> Result<bool, DbError> {
        let n = self
            .conn
            .prepare_cached("DELETE FROM album_tracks WHERE album_id = ?1 AND track_id = ?2")?
            .execute(params![album_id, track_id])?;
        Ok(n == 1)
    }

    /// Rewrite the album order; `track_ids` must be a permutation of its tracks.
    pub fn album_reorder(&self, album_id: i64, track_ids: &[String]) -> Result<(), DbError> {
        let mut current = self.album_track_ids(album_id)?;
        let mut wanted = track_ids.to_vec();
        current.sort();
        wanted.sort();
        if current != wanted {
            return Err(DbError::BadRequest(
                "track_ids must be a permutation of the album's tracks".to_string(),
            ));
        }
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "UPDATE album_tracks SET position = ?1 WHERE album_id = ?2 AND track_id = ?3",
            )?;
            for (position, track_id) in track_ids.iter().enumerate() {
                stmt.execute(params![position as i64, album_id, track_id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn albums_containing(&self, track_id: &str) -> Result<Vec<i64>, DbError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT album_id FROM album_tracks WHERE track_id = ?1 ORDER BY album_id",
        )?;
        let ids = stmt
            .query_map(params![track_id], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    }

    pub fn touch_album(&self, id: i64) -> Result<(), DbError> {
        self.conn
            .prepare_cached("UPDATE albums SET last_used_at = ?1 WHERE id = ?2")?
            .execute(params![now(), id])?;
        Ok(())
    }

    /// The most recently used album (ties → lowest id); never-used albums are ignored.
    pub fn last_used_album(&self) -> Result<Option<i64>, DbError> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT id FROM albums WHERE last_used_at IS NOT NULL
                 ORDER BY last_used_at DESC, id LIMIT 1",
            )?
            .query_row([], |row| row.get::<_, i64>(0))
            .optional()?)
    }

    // --- Lyrics ---

    pub fn get_lyrics(&self, track_id: &str) -> Result<Option<LyricsRow>, DbError> {
        Ok(self
            .conn
            .prepare_cached("SELECT synced, body, fetched_at FROM lyrics WHERE track_id = ?1")?
            .query_row(params![track_id], |row| {
                Ok(LyricsRow {
                    synced: row.get::<_, Option<i64>>(0)?.unwrap_or(0) != 0,
                    body: row.get(1)?,
                    fetched_at: row.get(2)?,
                })
            })
            .optional()?)
    }

    /// Upsert the cached lyrics; `body: None` records a confirmed miss.
    pub fn put_lyrics(
        &self,
        track_id: &str,
        synced: bool,
        body: Option<&str>,
    ) -> Result<(), DbError> {
        self.conn
            .prepare_cached(
                "INSERT INTO lyrics (track_id, synced, body, fetched_at) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(track_id) DO UPDATE SET
                    synced=excluded.synced,
                    body=excluded.body,
                    fetched_at=excluded.fetched_at",
            )?
            .execute(params![track_id, synced, body, now()])?;
        Ok(())
    }
}

fn track_from_row(row: &Row) -> rusqlite::Result<Track> {
    let audio_path: String = row.get(5)?;
    let state: String = row.get(10)?;
    Ok(Track {
        id: row.get(0)?,
        title: row.get(1)?,
        uploader: row.get(2)?,
        duration: row.get(3)?,
        webpage_url: row.get(4)?,
        audio_path: (!audio_path.is_empty()).then_some(audio_path),
        format: row.get(6)?,
        file_size: row.get(7)?,
        added_at: row.get(8)?,
        last_played: row.get(9)?,
        state: parse_state(&state),
        thumb_path: row.get(11)?,
    })
}

fn state_str(state: TrackState) -> &'static str {
    match state {
        TrackState::Complete => "complete",
        TrackState::Downloading => "downloading",
        TrackState::Failed => "failed",
    }
}

fn parse_state(s: &str) -> TrackState {
    match s {
        "downloading" => TrackState::Downloading,
        "failed" => TrackState::Failed,
        _ => TrackState::Complete,
    }
}

/// Map the given constraint violation to `DbError::Conflict`.
fn conflict_on(extended_code: i32) -> impl Fn(rusqlite::Error) -> DbError {
    move |e| match e {
        rusqlite::Error::SqliteFailure(err, _)
            if err.code == ErrorCode::ConstraintViolation && err.extended_code == extended_code =>
        {
            DbError::Conflict
        }
        e => DbError::Sqlite(e),
    }
}

fn found(changed: usize) -> Result<(), DbError> {
    if changed == 0 {
        Err(DbError::NotFound)
    } else {
        Ok(())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TrackMeta;
    use std::path::PathBuf;

    fn track(id: &str, title: &str) -> Track {
        Track {
            id: id.to_string(),
            title: title.to_string(),
            uploader: Some("Zutomayo".to_string()),
            duration: Some(215),
            webpage_url: Some(format!("https://youtu.be/{id}")),
            audio_path: Some(format!("/tmp/{id}.mp3")),
            format: Some("mp3".to_string()),
            file_size: Some(1234),
            added_at: None,
            last_played: None,
            state: TrackState::Complete,
            thumb_path: None,
        }
    }

    fn meta(id: &str, title: &str) -> TrackMeta {
        TrackMeta {
            id: id.to_string(),
            title: title.to_string(),
            uploader: Some("Zutomayo".to_string()),
            duration: Some(215),
            webpage_url: Some(format!("https://youtu.be/{id}")),
        }
    }

    fn open_temp() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("test.db"), dir.path()).unwrap();
        (db, dir)
    }

    fn user_version(db: &Db) -> i64 {
        db.conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    }

    fn columns(db: &Db, table: &str) -> Vec<String> {
        let mut stmt = db
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// The schema every pre-service (TUI) database was created with.
    const V0_DDL: &str = "
        CREATE TABLE tracks (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            uploader TEXT,
            duration INTEGER,
            webpage_url TEXT,
            audio_path TEXT NOT NULL,
            format TEXT,
            file_size INTEGER,
            added_at INTEGER NOT NULL,
            last_played INTEGER
        );
        CREATE TABLE albums (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            description TEXT DEFAULT '',
            created_at INTEGER NOT NULL
        );
        CREATE TABLE album_tracks (
            album_id INTEGER REFERENCES albums(id) ON DELETE CASCADE,
            track_id TEXT REFERENCES tracks(id) ON DELETE CASCADE,
            position INTEGER NOT NULL,
            PRIMARY KEY (album_id, track_id)
        );
        CREATE INDEX idx_tracks_title ON tracks(title COLLATE NOCASE);
        CREATE INDEX idx_tracks_uploader ON tracks(uploader COLLATE NOCASE);
        INSERT INTO tracks (id, title, uploader, duration, webpage_url, audio_path, format, file_size, added_at, last_played)
        VALUES ('aaaaaaaaaaa', 'One', 'Zutomayo', 215, 'https://youtu.be/aaaaaaaaaaa', '/c/One [aaaaaaaa]/audio.mp3', 'mp3', 1234, 100, 150),
               ('bbbbbbbbbbb', 'Two', NULL, NULL, NULL, '/c/bbbbbbbbbbb.mp3', 'mp3', NULL, 200, NULL);
    ";

    fn v0_fixture(dir: &Path) -> PathBuf {
        let path = dir.join("v0.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(V0_DDL).unwrap();
        path
    }

    #[test]
    fn fresh_db_migrates_to_v1_schema() {
        let (db, _dir) = open_temp();
        assert_eq!(user_version(&db), 1);
        let tracks = columns(&db, "tracks");
        assert!(tracks.contains(&"state".to_string()));
        assert!(tracks.contains(&"thumb_path".to_string()));
        assert!(columns(&db, "albums").contains(&"last_used_at".to_string()));
        assert_eq!(
            columns(&db, "lyrics"),
            ["track_id", "synced", "body", "fetched_at"]
        );
        let idx: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_album_tracks_track'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(idx, 1);
    }

    #[test]
    fn v0_db_migrates_preserving_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = v0_fixture(dir.path());
        let db = Db::open(&path, dir.path()).unwrap();
        assert_eq!(user_version(&db), 1);
        assert_eq!(
            db.get_track("aaaaaaaaaaa").unwrap().unwrap(),
            Track {
                id: "aaaaaaaaaaa".to_string(),
                title: "One".to_string(),
                uploader: Some("Zutomayo".to_string()),
                duration: Some(215),
                webpage_url: Some("https://youtu.be/aaaaaaaaaaa".to_string()),
                audio_path: Some("/c/One [aaaaaaaa]/audio.mp3".to_string()),
                format: Some("mp3".to_string()),
                file_size: Some(1234),
                added_at: Some(100),
                last_played: Some(150),
                state: TrackState::Complete,
                thumb_path: None,
            }
        );
        let two = db.get_track("bbbbbbbbbbb").unwrap().unwrap();
        assert_eq!(two.state, TrackState::Complete);
        assert_eq!(two.uploader, None);
        assert_eq!(db.list_tracks().unwrap().len(), 2);
    }

    #[test]
    fn migration_imports_album_json_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = v0_fixture(dir.path());
        std::fs::create_dir(dir.path().join("albums")).unwrap();
        std::fs::write(
            dir.path().join("albums").join("mix.album.json"),
            r#"{"name": "Mix", "description": "", "tracks": [
                {"id": "bbbbbbbbbbb", "order": 1},
                {"id": "aaaaaaaaaaa", "order": 2},
                {"id": "missing0000", "order": 3}]}"#,
        )
        .unwrap();

        let db = Db::open(&path, dir.path()).unwrap();
        let albums = db.list_albums().unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name, "Mix");
        assert_eq!(albums[0].track_ids, ["bbbbbbbbbbb", "aaaaaaaaaaa"]);
        assert!(db.album_remove(albums[0].id, "aaaaaaaaaaa").unwrap());
        drop(db);

        // The old scanner re-imported on every launch and undid album edits.
        let db = Db::open(&path, dir.path()).unwrap();
        assert_eq!(db.list_albums().unwrap()[0].track_ids, ["bbbbbbbbbbb"]);
    }

    #[test]
    fn pending_started_complete_transitions() {
        let (db, _dir) = open_temp();
        db.insert_pending("aaaaaaaaaaa", "https://youtu.be/aaaaaaaaaaa")
            .unwrap();
        let t = db.get_track("aaaaaaaaaaa").unwrap().unwrap();
        assert_eq!(t.title, "aaaaaaaaaaa");
        assert_eq!(t.state, TrackState::Downloading);
        assert_eq!(t.audio_path, None);
        assert_eq!(
            t.webpage_url.as_deref(),
            Some("https://youtu.be/aaaaaaaaaaa")
        );
        assert!(t.added_at.is_some());

        let m = meta("aaaaaaaaaaa", "秒針を噛む");
        let audio = "/c/秒針を噛む [aaaaaaaa]/audio.webm";
        db.mark_started("aaaaaaaaaaa", &m, audio).unwrap();
        let t = db.get_track("aaaaaaaaaaa").unwrap().unwrap();
        assert_eq!(t.title, "秒針を噛む");
        assert_eq!(t.uploader.as_deref(), Some("Zutomayo"));
        assert_eq!(t.duration, Some(215));
        assert_eq!(t.audio_path.as_deref(), Some(audio));
        assert_eq!(t.state, TrackState::Downloading);
        assert_eq!(t.format, None);

        let cover = "/c/秒針を噛む [aaaaaaaa]/cover.webp";
        db.mark_complete(
            "aaaaaaaaaaa",
            &m,
            audio,
            Some("webm"),
            Some(4321),
            Some(cover),
        )
        .unwrap();
        let t = db.get_track("aaaaaaaaaaa").unwrap().unwrap();
        assert_eq!(t.state, TrackState::Complete);
        assert_eq!(t.format.as_deref(), Some("webm"));
        assert_eq!(t.file_size, Some(4321));
        assert_eq!(t.thumb_path.as_deref(), Some(cover));
        assert!(
            db.tracks_in_state(TrackState::Downloading)
                .unwrap()
                .is_empty()
        );

        db.mark_failed("aaaaaaaaaaa").unwrap();
        let failed = db.tracks_in_state(TrackState::Failed).unwrap();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].id, "aaaaaaaaaaa");
    }

    #[test]
    fn insert_pending_on_failed_row_keeps_title_and_added_at() {
        let (db, _dir) = open_temp();
        let mut t = track("aaaaaaaaaaa", "One");
        t.added_at = Some(100);
        t.state = TrackState::Failed;
        db.upsert_track(&t).unwrap();

        db.insert_pending("aaaaaaaaaaa", "https://example.invalid/other")
            .unwrap();
        let got = db.get_track("aaaaaaaaaaa").unwrap().unwrap();
        assert_eq!(got.title, "One");
        assert_eq!(got.added_at, Some(100));
        assert_eq!(got.state, TrackState::Downloading);
        assert_eq!(got.webpage_url, t.webpage_url);
    }

    #[test]
    fn list_tracks_orders_by_added_at_desc_then_title_nocase() {
        let (db, _dir) = open_temp();
        for (id, title, added) in [
            ("t1", "banana", 100),
            ("t2", "Apple", 100),
            ("t3", "cherry", 200),
            ("t4", "apricot", 100),
        ] {
            let mut t = track(id, title);
            t.added_at = Some(added);
            db.upsert_track(&t).unwrap();
        }
        let ids: Vec<String> = db
            .list_tracks()
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, ["t3", "t2", "t4", "t1"]);
        assert_eq!(db.library_order().unwrap(), ids);
    }

    #[test]
    fn create_album_duplicate_name_is_conflict() {
        let (db, _dir) = open_temp();
        let id = db.create_album("Mix").unwrap();
        assert!(matches!(db.create_album("Mix"), Err(DbError::Conflict)));
        assert_eq!(db.get_or_create_album("Mix").unwrap(), id);
        assert_eq!(db.album_id_by_name("Mix").unwrap(), Some(id));
        assert_eq!(db.album_id_by_name("Other").unwrap(), None);
        let album = db.get_album(id).unwrap().unwrap();
        assert_eq!(album.name, "Mix");
        assert!(album.track_ids.is_empty());
        assert_eq!(album.last_used_at, None);
    }

    #[test]
    fn rename_album_to_existing_name_is_conflict() {
        let (db, _dir) = open_temp();
        db.create_album("A").unwrap();
        let b = db.create_album("B").unwrap();
        assert!(matches!(db.rename_album(b, "A"), Err(DbError::Conflict)));
        db.rename_album(b, "C").unwrap();
        assert_eq!(db.get_album(b).unwrap().unwrap().name, "C");
    }

    #[test]
    fn album_add_appends_and_is_idempotent() {
        let (db, _dir) = open_temp();
        for id in ["t1", "t2", "t3"] {
            db.upsert_track(&track(id, id)).unwrap();
        }
        let a = db.create_album("Mix").unwrap();
        assert!(db.album_add(a, "t2").unwrap());
        assert!(db.album_add(a, "t1").unwrap());
        assert!(!db.album_add(a, "t2").unwrap());
        assert!(db.album_add(a, "t3").unwrap());
        assert_eq!(
            db.get_album(a).unwrap().unwrap().track_ids,
            ["t2", "t1", "t3"]
        );
    }

    #[test]
    fn album_remove_drops_only_that_link() {
        let (db, _dir) = open_temp();
        db.upsert_track(&track("t1", "One")).unwrap();
        db.upsert_track(&track("t2", "Two")).unwrap();
        let a = db.create_album("Mix").unwrap();
        db.album_add(a, "t1").unwrap();
        db.album_add(a, "t2").unwrap();
        assert!(db.album_remove(a, "t1").unwrap());
        assert!(!db.album_remove(a, "t1").unwrap());
        assert_eq!(db.get_album(a).unwrap().unwrap().track_ids, ["t2"]);
        assert!(db.get_track("t1").unwrap().is_some());
    }

    #[test]
    fn album_reorder_rewrites_order_and_rejects_non_permutations() {
        let (db, _dir) = open_temp();
        for id in ["t1", "t2", "t3", "t4"] {
            db.upsert_track(&track(id, id)).unwrap();
        }
        let a = db.create_album("Mix").unwrap();
        for id in ["t1", "t2", "t3"] {
            db.album_add(a, id).unwrap();
        }
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        db.album_reorder(a, &ids(&["t3", "t1", "t2"])).unwrap();
        assert_eq!(
            db.get_album(a).unwrap().unwrap().track_ids,
            ["t3", "t1", "t2"]
        );

        for bad in [
            ids(&["t1", "t2"]),
            ids(&["t1", "t2", "t2"]),
            ids(&["t1", "t2", "t4"]),
            ids(&["t1", "t2", "t3", "t4"]),
        ] {
            assert!(matches!(
                db.album_reorder(a, &bad),
                Err(DbError::BadRequest(_))
            ));
        }
        assert_eq!(
            db.get_album(a).unwrap().unwrap().track_ids,
            ["t3", "t1", "t2"]
        );

        db.album_add(a, "t4").unwrap();
        assert_eq!(
            db.get_album(a).unwrap().unwrap().track_ids,
            ["t3", "t1", "t2", "t4"]
        );
    }

    #[test]
    fn albums_containing_lists_every_album_with_the_track() {
        let (db, _dir) = open_temp();
        db.upsert_track(&track("t1", "One")).unwrap();
        db.upsert_track(&track("t2", "Two")).unwrap();
        let a = db.create_album("A").unwrap();
        let b = db.create_album("B").unwrap();
        let c = db.create_album("C").unwrap();
        db.album_add(a, "t1").unwrap();
        db.album_add(b, "t2").unwrap();
        db.album_add(c, "t1").unwrap();
        assert_eq!(db.albums_containing("t1").unwrap(), [a, c]);
        assert_eq!(db.albums_containing("t2").unwrap(), [b]);
        assert!(db.albums_containing("t3").unwrap().is_empty());
    }

    #[test]
    fn last_used_album_ignores_unused_and_breaks_ties_by_lowest_id() {
        let (db, _dir) = open_temp();
        db.create_album("A").unwrap();
        let b = db.create_album("B").unwrap();
        let c = db.create_album("C").unwrap();
        assert_eq!(db.last_used_album().unwrap(), None);

        db.touch_album(c).unwrap();
        assert_eq!(db.last_used_album().unwrap(), Some(c));
        assert!(db.get_album(c).unwrap().unwrap().last_used_at.is_some());

        db.conn
            .execute(
                "UPDATE albums SET last_used_at = 500 WHERE id IN (?1, ?2)",
                params![b, c],
            )
            .unwrap();
        assert_eq!(db.last_used_album().unwrap(), Some(b));

        db.conn
            .execute(
                "UPDATE albums SET last_used_at = 600 WHERE id = ?1",
                params![c],
            )
            .unwrap();
        assert_eq!(db.last_used_album().unwrap(), Some(c));
    }

    #[test]
    fn deleting_a_track_cascades_album_links_and_lyrics() {
        let (db, _dir) = open_temp();
        db.upsert_track(&track("t1", "One")).unwrap();
        db.upsert_track(&track("t2", "Two")).unwrap();
        let a = db.create_album("Best").unwrap();
        db.album_add(a, "t1").unwrap();
        db.album_add(a, "t2").unwrap();
        db.put_lyrics("t1", true, Some("[00:01.00]hi")).unwrap();

        db.delete_track("t1").unwrap();
        assert_eq!(db.get_track("t1").unwrap(), None);
        assert_eq!(db.get_album(a).unwrap().unwrap().track_ids, ["t2"]);
        assert!(db.albums_containing("t1").unwrap().is_empty());
        assert_eq!(db.get_lyrics("t1").unwrap(), None);
        let lyrics_rows: i64 = db
            .conn
            .query_row("SELECT COUNT(*) FROM lyrics", [], |r| r.get(0))
            .unwrap();
        assert_eq!(lyrics_rows, 0);
    }

    #[test]
    fn lyrics_round_trip_including_miss_rows() {
        let (db, _dir) = open_temp();
        db.upsert_track(&track("t1", "One")).unwrap();
        db.upsert_track(&track("t2", "Two")).unwrap();
        assert_eq!(db.get_lyrics("t1").unwrap(), None);

        db.put_lyrics("t1", true, Some("[00:01.00]hi")).unwrap();
        let row = db.get_lyrics("t1").unwrap().unwrap();
        assert!(row.synced);
        assert_eq!(row.body.as_deref(), Some("[00:01.00]hi"));
        assert!(row.fetched_at > 0);

        db.put_lyrics("t1", false, Some("plain")).unwrap();
        let row = db.get_lyrics("t1").unwrap().unwrap();
        assert!(!row.synced);
        assert_eq!(row.body.as_deref(), Some("plain"));

        // A confirmed miss is a row with no body, distinct from "never fetched".
        db.put_lyrics("t2", false, None).unwrap();
        let row = db.get_lyrics("t2").unwrap().unwrap();
        assert!(!row.synced);
        assert_eq!(row.body, None);
    }

    #[test]
    fn cjk_title_round_trips_byte_exact() {
        let (db, _dir) = open_temp();
        let t = track("abc12345", "ずっと真夜中でいいのに。— 秒針を噛む");
        db.upsert_track(&t).unwrap();
        let got = db.get_track("abc12345").unwrap().unwrap();
        assert_eq!(got.title, "ずっと真夜中でいいのに。— 秒針を噛む");
    }
}
