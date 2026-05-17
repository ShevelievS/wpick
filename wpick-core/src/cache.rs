use std::path::Path;

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::model::{WallpaperInfo, WallpaperSource, WallpaperType};

// ─── Schema ───────────────────────────────────────────────────────────────────

const SCHEMA_SQL: &str = "
    PRAGMA journal_mode=WAL;
    PRAGMA synchronous=NORMAL;
    CREATE TABLE IF NOT EXISTS wallpapers (
        id               INTEGER PRIMARY KEY,
        title            TEXT NOT NULL,
        wallpaper_type   TEXT NOT NULL,
        file_path        TEXT NOT NULL,
        preview_path     TEXT,
        has_audio        INTEGER NOT NULL DEFAULT 0,
        file_size_bytes  INTEGER NOT NULL DEFAULT 0,
        pkg_mtime_secs   INTEGER NOT NULL,
        width            INTEGER NOT NULL DEFAULT 0,
        height           INTEGER NOT NULL DEFAULT 0,
        source           TEXT NOT NULL DEFAULT 'workshop',
        play_count       INTEGER NOT NULL DEFAULT 0,
        last_played_secs INTEGER NOT NULL DEFAULT 0
    ) STRICT;
    CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) STRICT;
";

// ─── Source serialization ─────────────────────────────────────────────────────

fn source_to_str(s: &WallpaperSource) -> String {
    match s {
        WallpaperSource::Workshop            => "workshop".to_owned(),
        WallpaperSource::Local { label }     => format!("local:{}", label),
    }
}

fn str_to_source(s: &str) -> WallpaperSource {
    if s == "workshop" {
        WallpaperSource::Workshop
    } else if let Some(label) = s.strip_prefix("local:") {
        WallpaperSource::Local { label: label.to_owned() }
    } else {
        WallpaperSource::Workshop
    }
}

// ─── Cache ────────────────────────────────────────────────────────────────────

pub struct Cache {
    conn: Connection,
}

impl Cache {
    /// Open (or create) the SQLite database, applying the WAL schema.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(SCHEMA_SQL)?;
        // Migrate existing DBs that predate each column.
        // "duplicate column name" errors are ignored — the column already exists.
        conn.execute(
            "ALTER TABLE wallpapers ADD COLUMN width INTEGER NOT NULL DEFAULT 0",
            [],
        ).ok();
        conn.execute(
            "ALTER TABLE wallpapers ADD COLUMN height INTEGER NOT NULL DEFAULT 0",
            [],
        ).ok();
        conn.execute(
            "ALTER TABLE wallpapers ADD COLUMN source TEXT NOT NULL DEFAULT 'workshop'",
            [],
        ).ok();
        conn.execute(
            "ALTER TABLE wallpapers ADD COLUMN play_count INTEGER NOT NULL DEFAULT 0",
            [],
        ).ok();
        conn.execute(
            "ALTER TABLE wallpapers ADD COLUMN last_played_secs INTEGER NOT NULL DEFAULT 0",
            [],
        ).ok();
        Ok(Self { conn })
    }

    /// Open an in-memory SQLite database for testing.
    /// Does NOT apply WAL/synchronous pragmas — they are meaningless or may error
    /// on in-memory databases depending on the SQLite build.
    pub fn open_in_memory() -> Result<Self> {
        const IN_MEMORY_SCHEMA: &str = "
            CREATE TABLE IF NOT EXISTS wallpapers (
                id               INTEGER PRIMARY KEY,
                title            TEXT NOT NULL,
                wallpaper_type   TEXT NOT NULL,
                file_path        TEXT NOT NULL,
                preview_path     TEXT,
                has_audio        INTEGER NOT NULL DEFAULT 0,
                file_size_bytes  INTEGER NOT NULL DEFAULT 0,
                pkg_mtime_secs   INTEGER NOT NULL DEFAULT 0,
                width            INTEGER NOT NULL DEFAULT 0,
                height           INTEGER NOT NULL DEFAULT 0,
                source           TEXT NOT NULL DEFAULT 'workshop',
                play_count       INTEGER NOT NULL DEFAULT 0,
                last_played_secs INTEGER NOT NULL DEFAULT 0
            ) STRICT;
            CREATE TABLE IF NOT EXISTS meta (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            ) STRICT;
        ";
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(IN_MEMORY_SCHEMA)?;
        Ok(Self { conn })
    }

    /// Retrieve all wallpapers sorted by source then title ascending.
    pub fn get_all(&self) -> Result<Vec<WallpaperInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, wallpaper_type, file_path, preview_path, \
             has_audio, file_size_bytes, width, height, source \
             FROM wallpapers ORDER BY source ASC, title ASC",
        )?;
        let rows = stmt.query_map([], row_to_info)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Retrieve a single wallpaper by ID.
    pub fn get_by_id(&self, id: u64) -> Result<Option<WallpaperInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, wallpaper_type, file_path, preview_path, \
             has_audio, file_size_bytes, width, height, source \
             FROM wallpapers WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id as i64], row_to_info)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Insert or update a wallpaper record, preserving play_count and last_played_secs.
    pub fn upsert(&self, info: &WallpaperInfo, pkg_mtime_secs: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO wallpapers \
             (id, title, wallpaper_type, file_path, preview_path, \
              has_audio, file_size_bytes, pkg_mtime_secs, width, height, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(id) DO UPDATE SET \
               title=excluded.title, wallpaper_type=excluded.wallpaper_type, \
               file_path=excluded.file_path, preview_path=excluded.preview_path, \
               has_audio=excluded.has_audio, file_size_bytes=excluded.file_size_bytes, \
               pkg_mtime_secs=excluded.pkg_mtime_secs, width=excluded.width, \
               height=excluded.height, source=excluded.source",
            params![
                info.id as i64,
                info.title,
                info.wallpaper_type.to_string(),
                info.file_path,
                info.preview_path,
                info.has_audio,
                info.file_size_bytes as i64,
                pkg_mtime_secs as i64,
                info.width as i64,
                info.height as i64,
                source_to_str(&info.source),
            ],
        )?;
        Ok(())
    }

    /// Record a play event: increment play_count and update last_played_secs.
    pub fn record_play(&self, id: u64) -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        self.conn.execute(
            "UPDATE wallpapers \
             SET play_count = play_count + 1, last_played_secs = ?1 \
             WHERE id = ?2",
            params![now, id as i64],
        )?;
        Ok(())
    }

    /// Return up to `limit` wallpapers ordered by play_count DESC, last_played DESC.
    /// Only includes wallpapers that have been played at least once.
    pub fn get_frequent(&self, limit: usize) -> Result<Vec<WallpaperInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, wallpaper_type, file_path, preview_path, \
             has_audio, file_size_bytes, width, height, source \
             FROM wallpapers \
             WHERE play_count > 0 \
             ORDER BY play_count DESC, last_played_secs DESC \
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], row_to_info)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Get the stored PKG mtime for a wallpaper, or None if not found.
    pub fn get_pkg_mtime(&self, id: u64) -> Result<Option<u64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT pkg_mtime_secs FROM wallpapers WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id as i64], |row| row.get::<_, i64>(0))?;
        match rows.next() {
            Some(r) => Ok(Some(r? as u64)),
            None => Ok(None),
        }
    }

    /// Delete a wallpaper by ID.
    pub fn remove(&self, id: u64) -> Result<()> {
        self.conn
            .execute("DELETE FROM wallpapers WHERE id = ?1", params![id as i64])?;
        Ok(())
    }

    /// Remove all wallpapers whose IDs are NOT in `active_ids`.
    /// If `active_ids` is empty, does nothing (safety guard against deleting everything).
    ///
    /// Uses DELETE WHERE id IN (to_delete) chunked at 999 to stay within
    /// SQLite's SQLITE_MAX_VARIABLE_NUMBER limit (safe even on large libraries).
    pub fn prune(&self, active_ids: &[u64]) -> Result<()> {
        if active_ids.is_empty() {
            return Ok(());
        }

        let mut stmt = self.conn.prepare("SELECT id FROM wallpapers")?;
        let db_ids: Vec<u64> = stmt
            .query_map([], |row| row.get::<_, i64>(0))?
            .filter_map(|r| r.ok())
            .map(|id| id as u64)
            .collect();

        let active_set: std::collections::HashSet<u64> = active_ids.iter().copied().collect();
        let to_delete: Vec<u64> = db_ids.into_iter()
            .filter(|id| !active_set.contains(id))
            .collect();

        if to_delete.is_empty() {
            return Ok(());
        }

        let tx = self.conn.unchecked_transaction()?;
        for chunk in to_delete.chunks(999) {
            let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM wallpapers WHERE id IN ({placeholders})");
            let params: Vec<rusqlite::types::Value> = chunk
                .iter()
                .map(|&id| rusqlite::types::Value::Integer(id as i64))
                .collect();
            tx.execute(&sql, rusqlite::params_from_iter(params))?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Insert or update a batch of wallpaper records in a single transaction.
    /// Preserves play_count and last_played_secs — a scan never resets statistics.
    pub fn upsert_batch(&self, items: &[(WallpaperInfo, u64)]) -> Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for (info, mtime) in items {
            tx.execute(
                "INSERT INTO wallpapers \
                 (id, title, wallpaper_type, file_path, preview_path, \
                  has_audio, file_size_bytes, pkg_mtime_secs, width, height, source) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                 ON CONFLICT(id) DO UPDATE SET \
                   title=excluded.title, wallpaper_type=excluded.wallpaper_type, \
                   file_path=excluded.file_path, preview_path=excluded.preview_path, \
                   has_audio=excluded.has_audio, file_size_bytes=excluded.file_size_bytes, \
                   pkg_mtime_secs=excluded.pkg_mtime_secs, width=excluded.width, \
                   height=excluded.height, source=excluded.source",
                params![
                    info.id as i64,
                    info.title,
                    info.wallpaper_type.to_string(),
                    info.file_path,
                    info.preview_path,
                    info.has_audio,
                    info.file_size_bytes as i64,
                    *mtime as i64,
                    info.width as i64,
                    info.height as i64,
                    source_to_str(&info.source),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Return total count of cached wallpapers.
    pub fn count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM wallpapers", [], |row| row.get(0))?;
        Ok(n as u64)
    }
}

// ─── Row helper ───────────────────────────────────────────────────────────────

fn row_to_info(row: &rusqlite::Row) -> rusqlite::Result<WallpaperInfo> {
    let type_str: String = row.get(2)?;
    let id: i64          = row.get(0)?;
    let wallpaper_type   = match type_str.as_str() {
        "video" => WallpaperType::Video,
        other   => {
            tracing::debug!(
                id,
                type_str = other,
                "Unsupported wallpaper_type in DB — treating as Video (pruned on next scan)"
            );
            WallpaperType::Video
        }
    };
    let source_str: String = row.get(9)?;
    Ok(WallpaperInfo {
        id:              id as u64,
        title:           row.get(1)?,
        wallpaper_type,
        file_path:       row.get(3)?,
        preview_path:    row.get(4)?,
        has_audio:       row.get(5)?,
        file_size_bytes: row.get::<_, i64>(6)? as u64,
        width:           row.get::<_, i64>(7)? as u32,
        height:          row.get::<_, i64>(8)? as u32,
        source:          str_to_source(&source_str),
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{WallpaperInfo, WallpaperSource, WallpaperType};
    use tempfile::TempDir;

    fn make_info(id: u64, title: &str) -> WallpaperInfo {
        WallpaperInfo {
            id,
            title:           title.to_owned(),
            wallpaper_type:  WallpaperType::Video,
            file_path:       format!("/fake/{id}/video.mp4"),
            preview_path:    None,
            has_audio:       false,
            file_size_bytes: 1024,
            width:           0,
            height:          0,
            source:          WallpaperSource::Workshop,
        }
    }

    #[test]
    fn test_open_creates_schema() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;
        assert_eq!(cache.count()?, 0);
        Ok(())
    }

    #[test]
    fn test_upsert_and_get_by_id() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        let info = make_info(12345, "Test Wallpaper");
        cache.upsert(&info, 9999)?;

        let result = cache.get_by_id(12345)?
            .ok_or(crate::error::WpickError::WallpaperNotFound { id: 12345 })?;
        assert_eq!(result.title, "Test Wallpaper");
        assert_eq!(result.wallpaper_type, WallpaperType::Video);
        assert_eq!(result.id, 12345);
        assert_eq!(result.source, WallpaperSource::Workshop);
        Ok(())
    }

    #[test]
    fn test_source_local_roundtrip() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        let mut info = make_info(99, "Local Video");
        info.source = WallpaperSource::Local { label: "my-videos".to_owned() };
        cache.upsert(&info, 0)?;

        let result = cache.get_by_id(99)?.expect("must exist");
        assert_eq!(result.source, WallpaperSource::Local { label: "my-videos".to_owned() });
        Ok(())
    }

    #[test]
    fn test_get_all_ordered_by_title() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        cache.upsert(&make_info(1, "Zzz"), 1)?;
        cache.upsert(&make_info(2, "Aaa"), 2)?;
        cache.upsert(&make_info(3, "Mmm"), 3)?;

        let all = cache.get_all()?;
        let titles: Vec<&str> = all.iter().map(|w| w.title.as_str()).collect();
        assert_eq!(titles, vec!["Aaa", "Mmm", "Zzz"]);
        Ok(())
    }

    #[test]
    fn test_prune_removes_inactive() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        cache.upsert(&make_info(1, "One"),   1)?;
        cache.upsert(&make_info(2, "Two"),   2)?;
        cache.upsert(&make_info(3, "Three"), 3)?;

        cache.prune(&[1, 3])?;

        assert!(cache.get_by_id(2)?.is_none());
        assert!(cache.get_by_id(1)?.is_some());
        assert!(cache.get_by_id(3)?.is_some());
        Ok(())
    }

    #[test]
    fn test_prune_empty_slice_does_nothing() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        cache.upsert(&make_info(1, "One"), 1)?;
        cache.prune(&[])?;
        assert_eq!(cache.count()?, 1);
        Ok(())
    }

    #[test]
    fn test_pkg_mtime_roundtrip() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        let info = make_info(777, "Mtime Test");
        cache.upsert(&info, 42)?;

        let mtime = cache.get_pkg_mtime(777)?;
        assert_eq!(mtime, Some(42));
        Ok(())
    }

    #[test]
    fn test_get_by_id_missing_returns_none() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;
        assert!(cache.get_by_id(99999)?.is_none(), "non-existent ID must return None");
        Ok(())
    }

    #[test]
    fn test_upsert_idempotency() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        let mut info = make_info(42, "Original");
        cache.upsert(&info, 100)?;

        info.title = "Updated".to_owned();
        cache.upsert(&info, 200)?;

        let result = cache.get_by_id(42)?.expect("must exist after upsert");
        assert_eq!(result.title, "Updated");
        assert_eq!(cache.count()?, 1);
        assert_eq!(cache.get_pkg_mtime(42)?, Some(200));
        Ok(())
    }

    #[test]
    fn test_remove() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        cache.upsert(&make_info(1, "One"), 0)?;
        cache.upsert(&make_info(2, "Two"), 0)?;
        assert_eq!(cache.count()?, 2);

        cache.remove(1)?;
        assert!(cache.get_by_id(1)?.is_none());
        assert_eq!(cache.count()?, 1);

        cache.remove(9999)?;
        assert_eq!(cache.count()?, 1);
        Ok(())
    }

    #[test]
    fn test_prune_over_999_ids() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        // Insert 1001 records.
        for i in 0u64..1001 {
            cache.upsert(&make_info(i, &format!("Wall {i}")), i)?;
        }
        assert_eq!(cache.count()?, 1001);

        // Keep only 0..999 active — record 999 and 1000 should be deleted.
        let active: Vec<u64> = (0u64..999).collect();
        cache.prune(&active)?;

        assert_eq!(cache.count()?, 999);
        assert!(cache.get_by_id(999)?.is_none(), "id 999 must be pruned");
        assert!(cache.get_by_id(1000)?.is_none(), "id 1000 must be pruned");
        assert!(cache.get_by_id(0)?.is_some(), "id 0 must survive");
        Ok(())
    }

    #[test]
    fn test_upsert_batch() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        let items: Vec<(WallpaperInfo, u64)> = (0u64..50)
            .map(|i| (make_info(i, &format!("Batch {i}")), i))
            .collect();

        cache.upsert_batch(&items)?;
        assert_eq!(cache.count()?, 50);

        // Idempotent: re-inserting same items should not duplicate.
        cache.upsert_batch(&items)?;
        assert_eq!(cache.count()?, 50);
        Ok(())
    }

    #[test]
    fn test_count_tracks_insertions_and_removals() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache = Cache::open(&tmp.path().join("test.db"))?;

        assert_eq!(cache.count()?, 0);
        cache.upsert(&make_info(1, "A"), 0)?;
        assert_eq!(cache.count()?, 1);
        cache.upsert(&make_info(2, "B"), 0)?;
        assert_eq!(cache.count()?, 2);
        cache.upsert(&make_info(1, "A-updated"), 1)?;
        assert_eq!(cache.count()?, 2);
        cache.remove(1)?;
        assert_eq!(cache.count()?, 1);
        Ok(())
    }
}
