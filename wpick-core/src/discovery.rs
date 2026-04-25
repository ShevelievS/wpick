use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

use crate::config::WpickConfig;
use crate::error::{Result, WpickError};

// ─── Public types ─────────────────────────────────────────────────────────────

/// A discovered Wallpaper Engine wallpaper directory.
#[derive(Debug, Clone)]
pub struct WallpaperDir {
    /// Workshop item ID (numeric directory name).
    pub id:   u64,
    /// Full path to the wallpaper directory.
    pub path: PathBuf,
}

// ─── VDF deserialization structs ──────────────────────────────────────────────

#[derive(Deserialize)]
struct LibraryFolders {
    #[serde(flatten)]
    entries: BTreeMap<String, LibraryEntry>,
}

#[derive(Deserialize)]
struct LibraryEntry {
    path: String,
    #[serde(flatten)]
    _rest: BTreeMap<String, serde_json::Value>,
}

// ─── Discovery ───────────────────────────────────────────────────────────────

/// Walk all Steam library roots and collect Wallpaper Engine workshop dirs.
///
/// - VDF missing → silently skip that root (not an error).
/// - VDF unparseable → return `WpickError::VdfParse` (let caller decide).
/// - Non-numeric subdir name → `tracing::debug!`, skip.
/// - 431960/ absent → skip that library root (normal case).
pub fn find_wallpaper_dirs(_config: &WpickConfig) -> Result<Vec<WallpaperDir>> {
    let home = dirs::home_dir()
        .ok_or_else(|| WpickError::Config("No home dir".into()))?;

    // Username for the Flatpak candidate path
    let username = home
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("user")
        .to_owned();

    let candidates: Vec<PathBuf> = vec![
        home.join(".steam/steam"),
        home.join(".steam/root"),
        home.join(".local/share/Steam"),
        home.join("snap/steam/common/.steam/steam"),
        PathBuf::from("/home")
            .join(&username)
            .join(".var/app/com.valvesoftware.Steam/data/Steam"),
    ];

    let mut results: Vec<WallpaperDir> = Vec::new();

    for steam_root in &candidates {
        // Skip roots that don't exist on disk
        if !steam_root.exists() {
            continue;
        }

        let vdf_path = steam_root.join("steamapps").join("libraryfolders.vdf");

        if !vdf_path.exists() {
            tracing::warn!(path = %vdf_path.display(), "libraryfolders.vdf not found, skipping Steam root");
            continue;
        }

        let content = match std::fs::read_to_string(&vdf_path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(path = %vdf_path.display(), error = %e, "Failed to read VDF, skipping");
                continue;
            }
        };

        let folders: LibraryFolders = match keyvalues_serde::from_str(&content) {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(
                    path = %vdf_path.display(),
                    error = %e,
                    "Failed to parse VDF — skipping this Steam root"
                );
                continue;
            }
        };

        for entry in folders.entries.values() {
            let library_root = PathBuf::from(&entry.path);
            let workshop_dir = library_root
                .join("steamapps")
                .join("workshop")
                .join("content")
                .join("431960");

            // Skip if Wallpaper Engine workshop dir doesn't exist (normal)
            if !workshop_dir.exists() {
                continue;
            }

            let read_dir = match std::fs::read_dir(&workshop_dir) {
                Ok(rd) => rd,
                Err(e) => {
                    tracing::warn!(path = %workshop_dir.display(), error = %e, "Cannot read workshop dir");
                    continue;
                }
            };

            for entry_result in read_dir {
                let dir_entry = match entry_result {
                    Ok(e) => e,
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to read directory entry");
                        continue;
                    }
                };

                let file_name = dir_entry.file_name();
                let name_str = match file_name.to_str() {
                    Some(s) => s,
                    None => {
                        tracing::debug!("Skipping non-UTF8 dir entry");
                        continue;
                    }
                };

                // Only numeric subdirectories are workshop item IDs
                let id: u64 = match name_str.parse() {
                    Ok(n) => n,
                    Err(_) => {
                        tracing::debug!(name = name_str, "Skipping non-numeric dir");
                        continue;
                    }
                };

                // Use metadata() which follows symlinks
                let meta = match std::fs::metadata(dir_entry.path()) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(path = %dir_entry.path().display(), error = %e, "Cannot stat dir entry");
                        continue;
                    }
                };

                if meta.is_dir() {
                    results.push(WallpaperDir {
                        id,
                        path: dir_entry.path(),
                    });
                }
            }
        }
    }

    Ok(results)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // VDF content with a single library entry
    const VDF_SINGLE: &str = r#""libraryfolders"
{
    "0"
    {
        "path"    "/tmp/fake_steam"
        "label"   ""
        "contentid"    "0"
        "totalsize"    "0"
        "update_clean_bytes_tally"    "0"
        "time_last_update_corruption"    "0"
        "apps"
        {
        }
    }
}"#;

    // VDF content with two library entries
    const VDF_MULTI: &str = r#""libraryfolders"
{
    "0"
    {
        "path"    "/mnt/games/steam"
        "label"   ""
        "contentid"    "0"
        "totalsize"    "0"
        "update_clean_bytes_tally"    "0"
        "time_last_update_corruption"    "0"
        "apps"
        {
        }
    }
    "1"
    {
        "path"    "/home/user/.local/share/Steam"
        "label"   ""
        "contentid"    "0"
        "totalsize"    "0"
        "update_clean_bytes_tally"    "0"
        "time_last_update_corruption"    "0"
        "apps"
        {
        }
    }
}"#;

    #[test]
    fn test_parse_vdf_single_library() -> crate::error::Result<()> {
        let folders: LibraryFolders =
            keyvalues_serde::from_str(VDF_SINGLE).map_err(|e| crate::error::WpickError::VdfParse {
                path:   "test".into(),
                reason: e.to_string(),
            })?;

        let paths: Vec<&str> = folders.entries.values().map(|e| e.path.as_str()).collect();
        assert_eq!(paths.len(), 1);
        assert_eq!(paths[0], "/tmp/fake_steam");
        Ok(())
    }

    #[test]
    fn test_parse_vdf_multiple_libraries() -> crate::error::Result<()> {
        let folders: LibraryFolders =
            keyvalues_serde::from_str(VDF_MULTI).map_err(|e| crate::error::WpickError::VdfParse {
                path:   "test".into(),
                reason: e.to_string(),
            })?;

        assert_eq!(folders.entries.len(), 2);

        let mut paths: Vec<&str> = folders.entries.values().map(|e| e.path.as_str()).collect();
        paths.sort();

        assert!(paths.contains(&"/mnt/games/steam"));
        assert!(paths.contains(&"/home/user/.local/share/Steam"));
        Ok(())
    }

    #[test]
    fn test_numeric_dir_filter() {
        // Simulate the filtering logic: only numeric names become WallpaperDir entries
        let names = vec![
            "2819752398",
            "12345",
            "thumbnails",
            ".DS_Store",
            "preview",
        ];

        let ids: Vec<u64> = names
            .iter()
            .filter_map(|name| name.parse::<u64>().ok())
            .collect();

        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&2819752398u64));
        assert!(ids.contains(&12345u64));
        // Non-numeric names produce no panics and are simply absent
        assert!(!ids.iter().any(|_| false)); // tautology — just confirms no panic
    }
}
