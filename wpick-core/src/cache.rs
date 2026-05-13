use std::path::Path;

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::model::{WallpaperInfo, WallpaperType};

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
        height           INTEGER NOT NULL DEFAULT 0
    ) STRICT;
    CREATE TABLE IF NOT EXISTS meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) STRICT;
";

// ─── Cache ────────────────────────────────────────────────────────────────────

pub struct Cache {
    conn: Connection,
}

impl Cache {
    /// Open (or create) the SQLite database, applying the WAL schema.
    pub fn open(db_path: &Path) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(SCHEMA_SQL)?;
        // Migrate existing DBs that predate the width/height columns.
        // "duplicate column name" errors are ignored — the column already exists.
        conn.execute(
            "ALTER TABLE wallpapers ADD COLUMN width INTEGER NOT NULL DEFAULT 0",
            [],
        ).ok();
        conn.execute(
            "ALTER TABLE wallpapers ADD COLUMN height INTEGER NOT NULL DEFAULT 0",
            [],
        ).ok();
        Ok(Self { conn })
    }

    /// Retrieve all wallpapers sorted by title ascending.
    pub fn get_all(&self) -> Result<Vec<WallpaperInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, wallpaper_type, file_path, preview_path, \
             has_audio, file_size_bytes, width, height FROM wallpapers ORDER BY title ASC",
        )?;
        let rows = stmt.query_map([], row_to_info)?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    /// Retrieve a single wallpaper by ID.
    pub fn get_by_id(&self, id: u64) -> Result<Option<WallpaperInfo>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, wallpaper_type, file_path, preview_path, \
             has_audio, file_size_bytes, width, height FROM wallpapers WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![id as i64], row_to_info)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    /// Insert or replace a wallpaper record.
    pub fn upsert(&self, info: &WallpaperInfo, pkg_mtime_secs: u64) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO wallpapers \
             (id, title, wallpaper_type, file_path, preview_path, \
              has_audio, file_size_bytes, pkg_mtime_secs, width, height) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            ],
        )?;
        Ok(())
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
    pub fn prune(&self, active_ids: &[u64]) -> Result<()> {
        if active_ids.is_empty() {
            return Ok(());
        }

        // Build a parameterised placeholder string: "?,?,?..."
        let placeholders = active_ids
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(",");

        let sql = format!(
            "DELETE FROM wallpapers WHERE id NOT IN ({placeholders})"
        );

        let params: Vec<rusqlite::types::Value> = active_ids
            .iter()
            .map(|&id| rusqlite::types::Value::Integer(id as i64))
            .collect();

        self.conn.execute(&sql, rusqlite::params_from_iter(params))?;
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
    let id: i64 = row.get(0)?;
    let wallpaper_type = match type_str.as_str() {
        "video" => WallpaperType::Video,
        "web"   => WallpaperType::Web,
        "scene" => WallpaperType::Scene,
        other   => {
            tracing::warn!(id, type_str = other, "Unknown wallpaper_type in DB — treating as Scene");
            WallpaperType::Scene
        }
    };
    Ok(WallpaperInfo {
        id:              row.get::<_, i64>(0)? as u64,
        title:           row.get(1)?,
        wallpaper_type,
        file_path:       row.get(3)?,
        preview_path:    row.get(4)?,
        has_audio:       row.get(5)?,
        file_size_bytes: row.get::<_, i64>(6)? as u64,
        width:           row.get::<_, i64>(7)? as u32,
        height:          row.get::<_, i64>(8)? as u32,
    })
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{WallpaperInfo, WallpaperType};
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

        cache.upsert(&make_info(1, "One"), 1)?;
        cache.upsert(&make_info(2, "Two"), 2)?;
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
        cache.upsert(&info, 200)?; // same id, new title + new mtime

        let result = cache.get_by_id(42)?.expect("must exist after upsert");
        assert_eq!(result.title, "Updated", "title must be updated");
        assert_eq!(cache.count()?, 1, "upsert must not create a duplicate row");
        assert_eq!(cache.get_pkg_mtime(42)?, Some(200), "mtime must be updated");
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
        assert!(cache.get_by_id(1)?.is_none(), "removed entry must not be found");
        assert_eq!(cache.count()?, 1, "count must decrease after remove");

        // Removing a non-existent ID must not error.
        cache.remove(9999)?;
        assert_eq!(cache.count()?, 1, "count must be unchanged after no-op remove");
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
        cache.upsert(&make_info(1, "A-updated"), 1)?; // re-upsert same id
        assert_eq!(cache.count()?, 2, "re-upsert must not increase count");
        cache.remove(1)?;
        assert_eq!(cache.count()?, 1);
        Ok(())
    }
}
