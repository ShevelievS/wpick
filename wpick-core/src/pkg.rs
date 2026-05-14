use std::path::Path;

use serde::Deserialize;

use crate::discovery::WallpaperDir;
use crate::error::Result;
use crate::model::{WallpaperInfo, WallpaperSource, WallpaperType};

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
    if project.wallpaper_type.to_lowercase() != "video" {
        return Ok(None);
    }

    let preview_path = project.preview.as_ref().map(|p| {
        preview_base.join(p).to_string_lossy().into_owned()
    });

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
        source:         WallpaperSource::Workshop,
    }))
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
    let project: ProjectJson = serde_json::from_str(&json_content)?;

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
    fn test_scene_type_returns_none() -> crate::error::Result<()> {
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        let project_json = r#"{"title":"Scene WP","type":"scene","file":"scene.pkg","preview":"p.gif"}"#;
        std::fs::write(tmp.path().join("project.json"), project_json)?;
        std::fs::write(tmp.path().join("p.gif"), b"GIF89a")?;

        let wd     = make_wallpaper_dir(99990, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;
        assert!(result.is_none(), "scene type must return None (not supported)");
        Ok(())
    }

    #[test]
    fn test_web_type_returns_none() -> crate::error::Result<()> {
        let tmp       = TempDir::new()?;
        let cache_dir = TempDir::new()?;

        std::fs::write(tmp.path().join("index.html"), b"<html></html>")?;
        let project_json = r#"{"title":"Web WP","type":"web","file":"index.html","preview":"p.jpg"}"#;
        std::fs::write(tmp.path().join("project.json"), project_json)?;
        std::fs::write(tmp.path().join("p.jpg"), b"\xff\xd8\xff")?;

        let wd     = make_wallpaper_dir(99986, tmp.path());
        let result = extract_and_parse(&wd, cache_dir.path())?;
        assert!(result.is_none(), "web type must return None (not supported)");
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


}
