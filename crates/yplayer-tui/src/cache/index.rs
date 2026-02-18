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

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;

        let idx = Self { conn };
        idx.create_tables()?;
        Ok(idx)
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

    /// Bulk insert album data (for scanner migration).
    pub fn bulk_insert_album(&self, name: &str, description: &str, track_ids: &[(String, i32)]) -> Result<()> {
        let album_id = self.create_album(name, description)?;
        let mut stmt = self.conn.prepare(
            "INSERT OR IGNORE INTO album_tracks (album_id, track_id, position) VALUES (?1, ?2, ?3)",
        )?;
        for (track_id, position) in track_ids {
            stmt.execute(params![album_id, track_id, position])?;
        }
        Ok(())
    }
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
