use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Deserialize;

use crate::discovery::WallpaperDir;
use crate::error::{Result, WpickError};
use crate::model::{WallpaperInfo, WallpaperType};

// ─── project.json schema ─────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectJson {
    pub title:          String,
    #[serde(rename = "type")]
    pub wallpaper_type: String,
    pub file:           Option<String>,
    pub preview:        Option<String>,
    #[serde(default)]
    pub sound_enabled:  bool,
    #[serde(default = "default_volume")]
    pub volume:         f32,
}

fn default_volume() -> f32 {
    1.0
}

// ─── PKG extraction fallback ─────────────────────────────────────────────────

/// Minimal PKG extractor for PKGV0001 / PKGV0005 format.
///
/// Format:
///   Magic (8 bytes): "PKGV0001" or "PKGV0005"
///   file_count (u32 le)
///   For each file:
///     name_len (u32 le)
///     name     (UTF-8, name_len bytes)
///     data_len (u32 le)
///     data     (data_len bytes)
fn extract_pkg(pkg_path: &Path, out_dir: &Path) -> Result<()> {
    let data = std::fs::read(pkg_path)?;

    if data.len() < 8 {
        return Err(WpickError::PkgExtract {
            id:     0,
            reason: "File too small to be a valid PKG".into(),
        });
    }

    let magic = &data[0..8];
    if magic != b"PKGV0001" && magic != b"PKGV0005" {
        return Err(WpickError::PkgExtract {
            id:     0,
            reason: format!("Unknown magic: {:?}", std::str::from_utf8(magic).unwrap_or("?")),
        });
    }

    if data.len() < 12 {
        return Err(WpickError::PkgExtract {
            id:     0,
            reason: "Truncated PKG header".into(),
        });
    }

    let mut pos = 8usize;
    let file_count = u32::from_le_bytes(
        data[pos..pos + 4]
            .try_into()
            .map_err(|_| WpickError::PkgExtract { id: 0, reason: "Bad file count bytes".into() })?,
    ) as usize;
    pos += 4;

    for _ in 0..file_count {
        if pos + 4 > data.len() {
            return Err(WpickError::PkgExtract { id: 0, reason: "Truncated name length".into() });
        }
        let name_len = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|_| WpickError::PkgExtract { id: 0, reason: "Bad name_len bytes".into() })?,
        ) as usize;
        pos += 4;

        if pos + name_len > data.len() {
            return Err(WpickError::PkgExtract { id: 0, reason: "Truncated filename".into() });
        }
        let name = std::str::from_utf8(&data[pos..pos + name_len]).map_err(|e| {
            WpickError::PkgExtract { id: 0, reason: format!("Non-UTF8 filename: {e}") }
        })?;
        pos += name_len;

        if pos + 4 > data.len() {
            return Err(WpickError::PkgExtract { id: 0, reason: "Truncated data length".into() });
        }
        let data_len = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|_| WpickError::PkgExtract { id: 0, reason: "Bad data_len bytes".into() })?,
        ) as usize;
        pos += 4;

        if pos + data_len > data.len() {
            return Err(WpickError::PkgExtract { id: 0, reason: "Truncated file data".into() });
        }
        let file_data = &data[pos..pos + data_len];
        pos += data_len;

        let out_path = out_dir.join(name);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, file_data)?;
    }

    Ok(())
}

// ─── Direct (non-PKG) wallpaper parser ───────────────────────────────────────

fn parse_direct_wallpaper(wallpaper_dir: &WallpaperDir) -> Result<Option<WallpaperInfo>> {
    let project_json_path = wallpaper_dir.path.join("project.json");
    if !project_json_path.exists() {
        return Ok(None);
    }

    let json_content = std::fs::read_to_string(&project_json_path)?;
    let project: ProjectJson = serde_json::from_str(&json_content)
        .map_err(|e| crate::error::WpickError::Io(std::io::Error::other(e)))?;

    if project.wallpaper_type.to_lowercase() != "video" {
        return Ok(None);
    }

    let file_name = match &project.file {
        Some(f) => f.clone(),
        None    => return Ok(None),
    };
    let video_path = wallpaper_dir.path.join(&file_name);
    if !video_path.exists() {
        return Ok(None);
    }

    let preview_path = project
        .preview
        .as_ref()
        .map(|p| wallpaper_dir.path.join(p).to_string_lossy().into_owned());

    let file_size_bytes = std::fs::metadata(&video_path).map(|m| m.len()).unwrap_or(0);

    Ok(Some(WallpaperInfo {
        id:              wallpaper_dir.id,
        title:           project.title,
        wallpaper_type:  WallpaperType::Video,
        file_path:       video_path.to_string_lossy().into_owned(),
        preview_path,
        has_audio:       project.sound_enabled,
        file_size_bytes,
    }))
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Extract PKG (if stale) and parse project.json → WallpaperInfo.
///
/// Two cases:
/// 1. No `scene.pkg` → Video wallpapers store the mp4 directly in the dir.
///    Parse project.json from the wallpaper dir itself.
/// 2. `scene.pkg` exists → Extract (if stale), parse from cache dir.
///
/// Returns `Ok(None)` when the wallpaper is not a supported Video type.
/// Returns `Err` only on genuine I/O or corrupt PKG.
pub fn extract_and_parse(
    wallpaper_dir: &WallpaperDir,
    wallpapers_cache: &Path,
) -> Result<Option<WallpaperInfo>> {
    let pkg_path = wallpaper_dir.path.join("scene.pkg");

    if !pkg_path.exists() {
        // Direct-file video wallpaper: project.json + mp4 in the same directory.
        return parse_direct_wallpaper(wallpaper_dir);
    }

    // mtime check
    let pkg_mtime_secs = std::fs::metadata(&pkg_path)?
        .modified()?
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let out_dir: PathBuf = wallpapers_cache.join(wallpaper_dir.id.to_string());

    let stamp_path = out_dir.join(".pkg_mtime");
    let cached_mtime = std::fs::read_to_string(&stamp_path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());

    let needs_extract = cached_mtime != Some(pkg_mtime_secs);

    if needs_extract {
        if out_dir.exists() {
            std::fs::remove_dir_all(&out_dir)?;
        }
        std::fs::create_dir_all(&out_dir)?;

        extract_pkg(&pkg_path, &out_dir).map_err(|e| {
            // Re-wrap with the correct id
            WpickError::PkgExtract {
                id:     wallpaper_dir.id,
                reason: e.to_string(),
            }
        })?;

        std::fs::write(&stamp_path, pkg_mtime_secs.to_string())?;
    }

    // Parse project.json
    let project_json_path = out_dir.join("project.json");
    if !project_json_path.exists() {
        return Ok(None);
    }

    let json_content = std::fs::read_to_string(&project_json_path)?;
    let project: ProjectJson = serde_json::from_str(&json_content)?;

    // Only video wallpapers are supported in MVP
    if project.wallpaper_type.to_lowercase() != "video" {
        return Ok(None);
    }

    // Verify the video file actually exists
    let file_name = match &project.file {
        Some(f) => f.clone(),
        None => return Ok(None),
    };
    let video_path = out_dir.join(&file_name);
    if !video_path.exists() {
        return Ok(None);
    }

    // Preview path (optional)
    let preview_path = project.preview.as_ref().map(|p| {
        out_dir.join(p).to_string_lossy().into_owned()
    });

    // File size
    let file_size_bytes = std::fs::metadata(&video_path)
        .map(|m| m.len())
        .unwrap_or(0);

    Ok(Some(WallpaperInfo {
        id:              wallpaper_dir.id,
        title:           project.title,
        wallpaper_type:  WallpaperType::Video,
        file_path:       video_path.to_string_lossy().into_owned(),
        preview_path,
        has_audio:       project.sound_enabled,
        file_size_bytes,
    }))
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_wallpaper_dir(id: u64, base: &Path) -> WallpaperDir {
        WallpaperDir { id, path: base.to_path_buf() }
    }

    #[test]
    fn test_ok_none_when_no_pkg() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        let wd = make_wallpaper_dir(99999, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;

        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn test_mtime_stamp_written_after_extract() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        // Create a minimal valid PKG (PKGV0001, 0 files)
        let mut pkg_bytes = b"PKGV0001".to_vec();
        pkg_bytes.extend_from_slice(&0u32.to_le_bytes()); // 0 files
        let pkg_path = tmp.path().join("scene.pkg");
        std::fs::write(&pkg_path, &pkg_bytes)?;

        let wd = make_wallpaper_dir(12345, tmp.path());
        // No project.json → should return Ok(None) but stamp should exist after extract
        let result = extract_and_parse(&wd, cache_dir.path())?;
        assert!(result.is_none());

        let stamp = cache_dir.path().join("12345").join(".pkg_mtime");
        assert!(stamp.exists(), ".pkg_mtime stamp should be written after extraction");

        Ok(())
    }

    #[test]
    fn test_mtime_skip_when_stamp_matches() -> crate::error::Result<()> {
        let tmp = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        // Create a minimal valid PKG
        let mut pkg_bytes = b"PKGV0001".to_vec();
        pkg_bytes.extend_from_slice(&0u32.to_le_bytes());
        let pkg_path = tmp.path().join("scene.pkg");
        std::fs::write(&pkg_path, &pkg_bytes)?;

        // Pre-create out_dir with a stamp that matches current mtime
        let mtime = std::fs::metadata(&pkg_path)?
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let out_dir = cache_dir.path().join("12345");
        std::fs::create_dir_all(&out_dir)?;
        let stamp_path = out_dir.join(".pkg_mtime");
        std::fs::write(&stamp_path, mtime.to_string())?;

        // Also write a sentinel file to confirm extraction is NOT re-run
        let sentinel = out_dir.join("sentinel.txt");
        std::fs::write(&sentinel, "original")?;

        let wd = make_wallpaper_dir(12345, tmp.path());
        let _ = extract_and_parse(&wd, cache_dir.path())?;

        // Sentinel must still exist (out_dir was not wiped)
        assert!(sentinel.exists(), "sentinel should survive when mtime matches (no re-extraction)");

        Ok(())
    }
}
