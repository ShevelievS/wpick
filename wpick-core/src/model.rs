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

/// Where the wallpaper came from — Steam Workshop or a user-defined local folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum WallpaperSource {
    /// Steam Workshop item (ID = numeric workshop folder name).
    #[default]
    Workshop,
    /// File discovered in a user-defined extra directory.
    /// `label` is the basename of the configured directory.
    Local { label: String },
}

impl fmt::Display for WallpaperSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WallpaperSource::Workshop       => write!(f, "Workshop"),
            WallpaperSource::Local { label } => write!(f, "{}", label),
        }
    }
}

/// Metadata for a single wallpaper, stored in / retrieved from the cache.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallpaperInfo {
    /// Workshop item ID or a stable xxhash of the file path for local files.
    pub id: u64,
    /// Human-readable title (project.json title or filename stem).
    pub title: String,
    pub wallpaper_type: WallpaperType,
    /// Absolute path to the playable file.
    pub file_path: String,
    /// Absolute path to the preview image, if one was found.
    pub preview_path: Option<String>,
    /// Whether the wallpaper has an audio track.
    pub has_audio: bool,
    /// File size of the primary asset in bytes.
    pub file_size_bytes: u64,
    /// Width of the primary asset in pixels (0 if unknown).
    #[serde(default)]
    pub width:  u32,
    /// Height of the primary asset in pixels (0 if unknown).
    #[serde(default)]
    pub height: u32,
    /// Where this wallpaper came from.
    #[serde(default)]
    pub source: WallpaperSource,
}

impl WallpaperInfo {
    /// Returns a single Unicode character icon for the wallpaper type.
    pub fn type_icon(&self) -> &'static str {
        "\u{25b6}"  // ▶
    }
}
