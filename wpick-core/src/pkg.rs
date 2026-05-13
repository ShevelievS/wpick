use std::path::Path;

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
/// Not used by the main code path (scene.pkg uses a different proprietary binary
/// format), but retained for potential future use.
#[allow(dead_code)]
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
                width:          0,
                height:         0,
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
            let (width, height) = image::image_dimensions(preview_pb).unwrap_or((0, 0));
            Ok(Some(WallpaperInfo {
                id,
                title:          project.title,
                wallpaper_type: WallpaperType::Scene,
                file_path:      preview_file.clone(),
                preview_path:   Some(preview_file),
                has_audio:      false,
                file_size_bytes,
                width,
                height,
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
                width:          0,
                height:         0,
            }))
        }
    }
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Extract PKG (if stale) and parse project.json → WallpaperInfo.
///
/// project.json is always in the original wallpaper directory — never inside scene.pkg.
/// scene.pkg contains only engine-specific scene assets (meshes, textures, etc.).
///
/// Returns `Ok(None)` when the wallpaper type is unknown or required assets are missing.
/// Returns `Err` only on genuine I/O problems.
pub fn extract_and_parse(
    wallpaper_dir: &WallpaperDir,
    _wallpapers_cache: &Path,
) -> Result<Option<WallpaperInfo>> {
    // project.json is always in the original wallpaper dir, not inside any PKG.
    let project_json_path = wallpaper_dir.path.join("project.json");
    if !project_json_path.exists() {
        return Ok(None);
    }

    let json_content = std::fs::read_to_string(&project_json_path)?;
    let project: ProjectJson = serde_json::from_str(&json_content)
        .map_err(|e| crate::error::WpickError::Io(std::io::Error::other(e)))?;

    // All assets (video, preview, HTML) live in the original wallpaper directory.
    let base = &wallpaper_dir.path;
    build_wallpaper_info(wallpaper_dir.id, project, base, base)
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
    fn test_ok_none_when_no_project_json() -> crate::error::Result<()> {
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        let wd = make_wallpaper_dir(99999, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;

        assert!(result.is_none());
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

        // Minimal preview.gif in the wallpaper dir (no scene.pkg — direct).
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
    fn test_scene_with_pkg_still_works() -> crate::error::Result<()> {
        // Real-world layout: scene.pkg + preview.gif + project.json side by side in wallpaper dir.
        // scene.pkg is ignored; project.json and preview.gif are read from the dir directly.
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        std::fs::write(tmp.path().join("preview.gif"), b"GIF89a")?;
        std::fs::write(tmp.path().join("project.json"),
            br#"{"title":"PKG Scene","type":"scene","preview":"preview.gif"}"#)?;
        std::fs::write(tmp.path().join("scene.pkg"), b"some-binary-data")?;

        let wd     = make_wallpaper_dir(99985, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;
        let info   = result.expect("scene with scene.pkg must still parse correctly");

        assert_eq!(info.wallpaper_type, WallpaperType::Scene);
        assert!(info.file_path.ends_with("preview.gif"),
            "file_path must point to preview, got: {}", info.file_path);
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

}
