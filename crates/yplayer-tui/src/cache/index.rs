use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::path::Path;

use crate::types::{Album, SortMode, Track};

pub struct CacheIndex {
    conn: Connection,
}

impl CacheIndex {
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)
            .with_context(|| format!("Failed to open database: {}", db_path.display()))?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;",
        )?;

        let idx = Self { conn };
        idx.create_tables()?;
        // The ON DELETE CASCADE was historically a no-op (foreign_keys defaulted
        // off), so old databases can carry album_tracks rows for deleted tracks.
        idx.cleanup_orphans()?;
        Ok(idx)
    }

    /// Remove album_tracks rows referencing tracks that no longer exist.
    fn cleanup_orphans(&self) -> Result<()> {
        self.conn.execute(
            "DELETE FROM album_tracks WHERE track_id NOT IN (SELECT id FROM tracks)",
            [],
        )?;
        Ok(())
    }

    fn create_tables(&self) -> Result<()> {
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

    pub fn upsert_track(&self, track: &Track) -> Result<()> {
        self.conn.execute(
            "INSERT INTO tracks (id, title, uploader, duration, webpage_url, audio_path, format, file_size, added_at, last_played)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                title=excluded.title,
                uploader=excluded.uploader,
                duration=excluded.duration,
                webpage_url=excluded.webpage_url,
                audio_path=excluded.audio_path,
                format=excluded.format,
                file_size=excluded.file_size",
            params![
                track.id,
                track.title,
                track.uploader,
                track.duration,
                track.webpage_url,
                track.audio_path,
                track.format,
                track.file_size,
                track.added_at.unwrap_or_else(|| now()),
                track.last_played,
            ],
        )?;
        Ok(())
    }

    pub fn list_tracks(&self) -> Result<Vec<Track>> {
        self.list_tracks_sorted(SortMode::Title)
    }

    pub fn list_tracks_sorted(&self, sort: SortMode) -> Result<Vec<Track>> {
        let order = match sort {
            SortMode::Title => "title COLLATE NOCASE",
            SortMode::RecentlyPlayed => "COALESCE(last_played, 0) DESC, title COLLATE NOCASE",
            SortMode::RecentlyAdded => "COALESCE(added_at, 0) DESC, title COLLATE NOCASE",
            SortMode::Uploader => "COALESCE(uploader, '') COLLATE NOCASE, title COLLATE NOCASE",
        };

        let sql = format!(
            "SELECT id, title, uploader, duration, webpage_url, audio_path, format, file_size, added_at, last_played
             FROM tracks ORDER BY {}",
            order
        );

        let mut stmt = self.conn.prepare(&sql)?;

        let tracks = stmt
            .query_map([], |row| {
                Ok(Track {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    uploader: row.get(2)?,
                    duration: row.get(3)?,
                    webpage_url: row.get(4)?,
                    audio_path: row.get(5)?,
                    format: row.get(6)?,
                    file_size: row.get(7)?,
                    added_at: row.get(8)?,
                    last_played: row.get(9)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }

    /// Return all track IDs currently in the DB (used for incremental scan).
    pub fn list_track_ids(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT id FROM tracks")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(ids)
    }

    /// Update just the title of a track (rename).
    pub fn rename_track(&self, id: &str, new_title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET title = ?1 WHERE id = ?2",
            params![new_title, id],
        )?;
        Ok(())
    }

    pub fn get_track(&self, id: &str) -> Result<Option<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, uploader, duration, webpage_url, audio_path, format, file_size, added_at, last_played
             FROM tracks WHERE id = ?1",
        )?;

        let mut tracks: Vec<Track> = stmt
            .query_map(params![id], |row| {
                Ok(Track {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    uploader: row.get(2)?,
                    duration: row.get(3)?,
                    webpage_url: row.get(4)?,
                    audio_path: row.get(5)?,
                    format: row.get(6)?,
                    file_size: row.get(7)?,
                    added_at: row.get(8)?,
                    last_played: row.get(9)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks.pop())
    }

    pub fn delete_track(&self, id: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM tracks WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn update_last_played(&self, id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET last_played = ?1 WHERE id = ?2",
            params![now(), id],
        )?;
        Ok(())
    }

    pub fn search_tracks(&self, query: &str) -> Result<Vec<Track>> {
        let pattern = format!("%{}%", query);
        let mut stmt = self.conn.prepare(
            "SELECT id, title, uploader, duration, webpage_url, audio_path, format, file_size, added_at, last_played
             FROM tracks
             WHERE title LIKE ?1 COLLATE NOCASE OR uploader LIKE ?1 COLLATE NOCASE
             ORDER BY title COLLATE NOCASE",
        )?;

        let tracks = stmt
            .query_map(params![pattern], |row| {
                Ok(Track {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    uploader: row.get(2)?,
                    duration: row.get(3)?,
                    webpage_url: row.get(4)?,
                    audio_path: row.get(5)?,
                    format: row.get(6)?,
                    file_size: row.get(7)?,
                    added_at: row.get(8)?,
                    last_played: row.get(9)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }

    // --- Albums ---

    pub fn list_albums(&self) -> Result<Vec<Album>> {
        let mut stmt = self.conn.prepare(
            "SELECT a.id, a.name, a.description, a.created_at,
                    (SELECT COUNT(*) FROM album_tracks WHERE album_id = a.id) as track_count
             FROM albums a ORDER BY a.name COLLATE NOCASE",
        )?;

        let albums = stmt
            .query_map([], |row| {
                Ok(Album {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    created_at: row.get(3)?,
                    track_count: row.get::<_, i64>(4)? as usize,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(albums)
    }

    pub fn create_album(&self, name: &str, description: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO albums (name, description, created_at) VALUES (?1, ?2, ?3)",
            params![name, description, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Return the id of the album named `name`, creating it if absent.
    /// (albums.name is UNIQUE, so a plain INSERT fails on re-sync — this doesn't.)
    pub fn get_or_create_album(&self, name: &str, description: &str) -> Result<i64> {
        use rusqlite::OptionalExtension;
        if let Some(id) = self
            .conn
            .query_row(
                "SELECT id FROM albums WHERE name = ?1",
                params![name],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            return Ok(id);
        }
        self.create_album(name, description)
    }

    pub fn get_album_tracks(&self, album_id: i64) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.title, t.uploader, t.duration, t.webpage_url, t.audio_path, t.format, t.file_size, t.added_at, t.last_played
             FROM tracks t
             INNER JOIN album_tracks at ON t.id = at.track_id
             WHERE at.album_id = ?1
             ORDER BY at.position",
        )?;

        let tracks = stmt
            .query_map(params![album_id], |row| {
                Ok(Track {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    uploader: row.get(2)?,
                    duration: row.get(3)?,
                    webpage_url: row.get(4)?,
                    audio_path: row.get(5)?,
                    format: row.get(6)?,
                    file_size: row.get(7)?,
                    added_at: row.get(8)?,
                    last_played: row.get(9)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(tracks)
    }

    pub fn add_track_to_album(&self, album_id: i64, track_id: &str, position: i32) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO album_tracks (album_id, track_id, position) VALUES (?1, ?2, ?3)",
            params![album_id, track_id, position],
        )?;
        Ok(())
    }

    pub fn remove_track_from_album(&self, album_id: i64, track_id: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM album_tracks WHERE album_id = ?1 AND track_id = ?2",
            params![album_id, track_id],
        )?;
        Ok(())
    }

    pub fn track_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tracks", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Bulk insert tracks within a single transaction (for scanner).
    pub fn bulk_insert_tracks(&self, tracks: &[Track]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO tracks (id, title, uploader, duration, webpage_url, audio_path, format, file_size, added_at, last_played)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            )?;
            for track in tracks {
                stmt.execute(params![
                    track.id,
                    track.title,
                    track.uploader,
                    track.duration,
                    track.webpage_url,
                    track.audio_path,
                    track.format,
                    track.file_size,
                    track.added_at.unwrap_or_else(|| now()),
                    track.last_played,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Bulk insert album data (for scanner migration). Idempotent across launches:
    /// gets-or-creates the album and ignores album_tracks rows that already exist
    /// or reference missing tracks (skipped by OR IGNORE under foreign_keys=ON).
    pub fn bulk_insert_album(&self, name: &str, description: &str, track_ids: &[(String, i32)]) -> Result<()> {
        let album_id = self.get_or_create_album(name, description)?;
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO album_tracks (album_id, track_id, position) VALUES (?1, ?2, ?3)",
            )?;
            for (track_id, position) in track_ids {
                stmt.execute(params![album_id, track_id, position])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }

    fn open_temp() -> (CacheIndex, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = CacheIndex::open(&dir.path().join("test.db")).unwrap();
        (db, dir)
    }

    #[test]
    fn cjk_title_round_trips_byte_exact() {
        let (db, _dir) = open_temp();
        let t = track("abc12345", "ずっと真夜中でいいのに。— 秒針を噛む");
        db.upsert_track(&t).unwrap();
        let got = db.get_track("abc12345").unwrap().unwrap();
        assert_eq!(got.title, "ずっと真夜中でいいのに。— 秒針を噛む");
    }

    #[test]
    fn deleting_a_track_leaves_no_orphan_album_rows() {
        let (db, _dir) = open_temp();
        db.upsert_track(&track("t1", "One")).unwrap();
        db.upsert_track(&track("t2", "Two")).unwrap();
        let album = db.get_or_create_album("Best", "").unwrap();
        db.add_track_to_album(album, "t1", 0).unwrap();
        db.add_track_to_album(album, "t2", 1).unwrap();

        // With foreign_keys=ON, deleting a track cascades to album_tracks, so the
        // album's reported count stays in sync with its actual joined tracks.
        db.delete_track("t1").unwrap();
        assert_eq!(db.get_album_tracks(album).unwrap().len(), 1);
        let albums = db.list_albums().unwrap();
        assert_eq!(albums[0].track_count, 1);
    }

    #[test]
    fn bulk_insert_album_is_idempotent_across_reruns() {
        let (db, _dir) = open_temp();
        db.upsert_track(&track("t1", "One")).unwrap();
        let rows = [("t1".to_string(), 0)];
        // Simulates the scanner running on every launch — the second call used to
        // fail on the albums.name UNIQUE constraint and silently skip re-sync.
        db.bulk_insert_album("Mix", "", &rows).unwrap();
        db.bulk_insert_album("Mix", "", &rows).unwrap();
        let albums = db.list_albums().unwrap();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].track_count, 1);
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
