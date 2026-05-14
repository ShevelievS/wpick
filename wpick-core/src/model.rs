use serde::{Deserialize, Serialize};
use std::fmt;

/// Type of a Wallpaper Engine wallpaper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WallpaperType {
    Video,
}

impl fmt::Display for WallpaperType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WallpaperType::Video => write!(f, "video"),
        }
    }
}

/// Metadata for a single Wallpaper Engine wallpaper, stored in / retrieved from the cache.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallpaperInfo {
    /// Steam Workshop item ID (numeric folder name under `content/431960/`)
    pub id: u64,
    /// Human-readable title from `project.json`
    pub title: String,
    pub wallpaper_type: WallpaperType,
    /// Absolute path to the playable file (e.g. the extracted mp4)
    pub file_path: String,
    /// Absolute path to the preview image, if one was found
    pub preview_path: Option<String>,
    /// Whether the wallpaper has an audio track
    pub has_audio: bool,
    /// File size of the primary asset in bytes
    pub file_size_bytes: u64,
    /// Width of the primary asset in pixels (0 if unknown)
    #[serde(default)]
    pub width:  u32,
    /// Height of the primary asset in pixels (0 if unknown)
    #[serde(default)]
    pub height: u32,
}

impl WallpaperInfo {
    /// Returns a single Unicode character icon for the wallpaper type.
    pub fn type_icon(&self) -> &'static str {
        "\u{25b6}"  // ▶
    }
}
