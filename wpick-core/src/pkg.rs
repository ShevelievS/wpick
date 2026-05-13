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
    #[serde(default = "default_sound_enabled")]
    pub sound_enabled: bool,
    #[serde(default = "default_volume")]
    pub volume:         f32,
}

fn default_sound_enabled() -> bool { true }
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
            reason: format!("Unknown magic: {}", magic.iter().map(|b| format!("{b:02x}")).collect::<String>()),
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

    if file_count > 65_536 {
        return Err(WpickError::PkgExtract {
            id:     0,
            reason: format!("Implausible file_count={file_count} — refusing to process"),
        });
    }

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

        // Sanitize: strip leading slashes and any ".." components to prevent
        // path traversal (e.g. "../../.bashrc" in a crafted PKG). (H-6)
        let safe_name = name
            .trim_start_matches('/')
            .split('/')
            .filter(|c| *c != ".." && !c.is_empty())
            .collect::<Vec<_>>()
            .join("/");

        if safe_name.is_empty() {
            tracing::debug!("PKG: skipping empty/unsafe name {:?}", name);
            continue;
        }

        let out_path = out_dir.join(&safe_name);
        // Final guard: out_path must be inside out_dir
        if !out_path.starts_with(out_dir) {
            return Err(WpickError::PkgExtract {
                id:     0,
                reason: format!("Path traversal attempt: {name:?}"),
            });
        }
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&out_path, file_data)?;
    }

    Ok(())
}

// ─── Shared builder ──────────────────────────────────────────────────────────

/// Build a `WallpaperInfo` from a parsed `ProjectJson`.
///
/// - `file_base` — directory that contains the primary asset (video / preview).
/// - `preview_base` — directory that contains the preview image (may equal `file_base`).
///
/// Returns `Ok(None)` when the wallpaper type is unknown or the required asset
/// is missing from disk; returns `Err` only on genuine I/O problems.
fn build_wallpaper_info(
    id:           u64,
    project:      ProjectJson,
    file_base:    &Path,
    preview_base: &Path,
) -> Result<Option<WallpaperInfo>> {
    let wtype = match project.wallpaper_type.to_lowercase().as_str() {
        "video" => WallpaperType::Video,
        "scene" => WallpaperType::Scene,
        "web"   => WallpaperType::Web,
        _       => return Ok(None),
    };

    let preview_path = project.preview.as_ref().map(|p| {
        preview_base.join(p).to_string_lossy().into_owned()
    });

    match wtype {
        WallpaperType::Video => {
            let file_name = match project.file.as_ref() {
                Some(f) => f.clone(),
                None    => return Ok(None),
            };
            let video_path = file_base.join(&file_name);
            if !video_path.exists() { return Ok(None); }
            let file_size_bytes = std::fs::metadata(&video_path).map(|m| m.len()).unwrap_or(0);
            Ok(Some(WallpaperInfo {
                id,
                title:          project.title,
                wallpaper_type: WallpaperType::Video,
                file_path:      video_path.to_string_lossy().into_owned(),
                preview_path,
                has_audio:      project.sound_enabled,
                file_size_bytes,
            }))
        }

        WallpaperType::Scene => {
            // Scene wallpapers are displayed via their preview image (gif or static).
            // No preview → nothing to show → skip silently.
            let preview_file = match &preview_path {
                Some(p) => p.clone(),
                None    => return Ok(None),
            };
            let preview_pb = Path::new(&preview_file);
            if !preview_pb.exists() { return Ok(None); }
            let file_size_bytes = std::fs::metadata(preview_pb).map(|m| m.len()).unwrap_or(0);
            Ok(Some(WallpaperInfo {
                id,
                title:          project.title,
                wallpaper_type: WallpaperType::Scene,
                file_path:      preview_file.clone(),
                preview_path:   Some(preview_file),
                has_audio:      false,
                file_size_bytes,
            }))
        }

        WallpaperType::Web => {
            // Web wallpapers are rendered by wpick-webview via webkit2gtk.
            // file_path = the HTML entry point; preview_path = thumbnail image.
            let html_name = match project.file.as_ref() {
                Some(f) => f.clone(),
                None    => return Ok(None),
            };
            let html_path = file_base.join(&html_name);
            if !html_path.exists() { return Ok(None); }
            let file_size_bytes = std::fs::metadata(&html_path).map(|m| m.len()).unwrap_or(0);
            Ok(Some(WallpaperInfo {
                id,
                title:          project.title,
                wallpaper_type: WallpaperType::Web,
                file_path:      html_path.to_string_lossy().into_owned(),
                preview_path,
                has_audio:      false,
                file_size_bytes,
            }))
        }
    }
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

    let base = &wallpaper_dir.path;
    build_wallpaper_info(wallpaper_dir.id, project, base, base)
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

    build_wallpaper_info(wallpaper_dir.id, project, &out_dir, &out_dir)
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
    fn test_path_traversal_rejected() -> crate::error::Result<()> {
        // Multiple traversal patterns — none must escape the cache output dir.
        let traversal_names: &[&[u8]] = &[
            b"../../evil.sh",
            b"../escape.txt",
            b"/etc/passwd",
            b"sub/../../sneaky.sh",
        ];

        for &name in traversal_names {
            let tmp       = TempDir::new()?;
            let cache_dir = TempDir::new()?;

            let mut pkg_bytes = b"PKGV0001".to_vec();
            pkg_bytes.extend_from_slice(&1u32.to_le_bytes());
            pkg_bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
            pkg_bytes.extend_from_slice(name);
            let content = b"pwned";
            pkg_bytes.extend_from_slice(&(content.len() as u32).to_le_bytes());
            pkg_bytes.extend_from_slice(content);

            std::fs::write(tmp.path().join("scene.pkg"), &pkg_bytes)?;
            let wd = make_wallpaper_dir(99998, tmp.path());
            let _ = extract_and_parse(&wd, cache_dir.path());

            // Verify that nothing was written above the cache output dir.
            let name_str = std::str::from_utf8(name).unwrap_or("?");
            let dangerous = cache_dir.path().parent().unwrap().join("evil.sh");
            let dangerous2 = cache_dir.path().parent().unwrap().join("sneaky.sh");
            let dangerous3 = std::path::Path::new("/etc/passwd");
            assert!(!dangerous.exists(),  "traversal '{name_str}' escaped cache dir (evil.sh)");
            assert!(!dangerous2.exists(), "traversal '{name_str}' escaped cache dir (sneaky.sh)");
            // /etc/passwd is a pre-existing file, so we only assert we didn't MODIFY it.
            if dangerous3.exists() {
                let meta = std::fs::metadata(dangerous3)?;
                assert!(meta.len() > 0, "/etc/passwd should not be zeroed by traversal");
            }
        }
        Ok(())
    }

    #[test]
    fn test_file_count_guard() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut pkg_bytes = b"PKGV0001".to_vec();
        pkg_bytes.extend_from_slice(&0xFFFF_FFFFu32.to_le_bytes()); // absurd count
        std::fs::write(tmp.path().join("scene.pkg"), &pkg_bytes).unwrap();
        let out = tempfile::TempDir::new().unwrap();
        let wd = make_wallpaper_dir(99997, tmp.path());
        let r = extract_and_parse(&wd, out.path());
        assert!(r.is_err(), "absurd file_count must be rejected");
    }

    #[test]
    fn test_pkgv0005_magic_accepted() -> crate::error::Result<()> {
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        // PKGV0005 with 0 files — extraction must succeed (result is None, no project.json).
        let mut pkg_bytes = b"PKGV0005".to_vec();
        pkg_bytes.extend_from_slice(&0u32.to_le_bytes());
        std::fs::write(tmp.path().join("scene.pkg"), &pkg_bytes)?;

        let wd     = make_wallpaper_dir(11111, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path());
        assert!(result.is_ok(), "PKGV0005 must be accepted, got: {:?}", result.err());
        Ok(())
    }

    #[test]
    fn test_direct_wallpaper_parse() -> crate::error::Result<()> {
        // Wallpapers stored directly (no scene.pkg): project.json + mp4 in the same dir.
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        let project_json = r#"{
            "title": "My Wallpaper",
            "type": "video",
            "file": "video.mp4",
            "soundEnabled": true,
            "preview": "preview.jpg"
        }"#;
        std::fs::write(tmp.path().join("project.json"), project_json)?;
        std::fs::write(tmp.path().join("video.mp4"),    b"fake-video-data")?;

        let wd     = make_wallpaper_dir(12345, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;

        let info = result.expect("direct wallpaper must parse to Some(WallpaperInfo)");
        assert_eq!(info.id, 12345);
        assert_eq!(info.title, "My Wallpaper");
        assert_eq!(info.wallpaper_type, WallpaperType::Video);
        assert!(info.has_audio,          "soundEnabled=true must set has_audio");
        assert!(info.preview_path.is_some(), "preview path must be populated");
        assert_eq!(info.file_size_bytes, 15, "file_size_bytes should match fake data length");
        Ok(())
    }

    #[test]
    fn test_scene_no_preview_returns_none() -> crate::error::Result<()> {
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        // Scene with no preview field → nothing to display → None.
        let project_json = r#"{"title":"Scene WP","type":"scene","file":"scene.pkg"}"#;
        std::fs::write(tmp.path().join("project.json"), project_json)?;

        let wd     = make_wallpaper_dir(99990, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;
        assert!(result.is_none(), "scene with no preview must return None");
        Ok(())
    }

    #[test]
    fn test_unknown_type_returns_none() -> crate::error::Result<()> {
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        let project_json = r#"{"title":"Future WP","type":"hologram","preview":"p.jpg"}"#;
        std::fs::write(tmp.path().join("project.json"), project_json)?;
        std::fs::write(tmp.path().join("p.jpg"), b"fake")?;

        let wd     = make_wallpaper_dir(99989, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;
        assert!(result.is_none(), "unknown type must return None");
        Ok(())
    }

    #[test]
    fn test_scene_with_preview_returns_some() -> crate::error::Result<()> {
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        // Minimal preview.gif in the wallpaper dir.
        std::fs::write(tmp.path().join("preview.gif"), b"GIF89a")?;
        let project_json = r#"{"title":"My Scene","type":"scene","preview":"preview.gif"}"#;
        std::fs::write(tmp.path().join("project.json"), project_json)?;

        let wd     = make_wallpaper_dir(99988, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;
        let info   = result.expect("scene with preview must return Some");

        assert_eq!(info.wallpaper_type, WallpaperType::Scene);
        assert_eq!(info.title, "My Scene");
        assert!(info.file_path.ends_with("preview.gif"));
        assert!(info.preview_path.as_ref().map(|p| p.ends_with("preview.gif")).unwrap_or(false));
        assert!(!info.has_audio);
        Ok(())
    }

    #[test]
    fn test_web_with_html_returns_some() -> crate::error::Result<()> {
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        std::fs::write(tmp.path().join("index.html"), b"<html></html>")?;
        std::fs::write(tmp.path().join("preview.jpg"), b"\xff\xd8\xff")?;
        let project_json = r#"{"title":"My Web","type":"web","file":"index.html","preview":"preview.jpg"}"#;
        std::fs::write(tmp.path().join("project.json"), project_json)?;

        let wd     = make_wallpaper_dir(99987, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;
        let info   = result.expect("web with html file must return Some");

        assert_eq!(info.wallpaper_type, WallpaperType::Web);
        assert!(info.file_path.ends_with("index.html"), "file_path must point to HTML, got: {}", info.file_path);
        assert!(info.preview_path.as_ref().map(|p| p.ends_with("preview.jpg")).unwrap_or(false));
        Ok(())
    }

    #[test]
    fn test_web_no_html_file_returns_none() -> crate::error::Result<()> {
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        // No file field → None.
        let project_json = r#"{"title":"Web","type":"web","preview":"preview.jpg"}"#;
        std::fs::write(tmp.path().join("project.json"), project_json)?;
        std::fs::write(tmp.path().join("preview.jpg"), b"\xff\xd8\xff")?;

        let wd     = make_wallpaper_dir(99986, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;
        assert!(result.is_none(), "web with no file field must return None");
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
