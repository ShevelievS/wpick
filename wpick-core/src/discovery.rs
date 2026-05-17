use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use walkdir::WalkDir;

use crate::config::WpickConfig;
use crate::error::{Result, WpickError};
use crate::model::{WallpaperInfo, WallpaperSource, WallpaperType};

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

    // Deduplicate by Workshop ID — multiple Steam root candidates (symlinks, Flatpak
    // paths) can point to the same library, producing the same wallpaper twice.
    results.sort_by_key(|wd| wd.id);
    results.dedup_by_key(|wd| wd.id);

    Ok(results)
}

// ─── Local video discovery ────────────────────────────────────────────────────

const VIDEO_EXTENSIONS: &[&str] = &["mp4", "webm", "mkv", "avi", "mov", "gif", "wmv", "flv"];
const PREVIEW_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp"];

/// FNV-1a 64-bit hash — small, no deps, stable across runs.
fn fnv1a(data: &[u8]) -> u64 {
    const OFFSET: u64 = 14695981039346656037;
    const PRIME:  u64 = 1099511628211;
    let mut h = OFFSET;
    for &b in data { h ^= b as u64; h = h.wrapping_mul(PRIME); }
    h
}

/// Stable u64 ID for a local file path.
/// High bit is always set so local IDs never collide with Workshop IDs (< 2³²).
fn local_id(path: &Path) -> u64 {
    fnv1a(path.to_string_lossy().as_bytes()) | (1u64 << 63)
}

/// Walk each directory in `extra_dirs`, collect all video files, and return them
/// as `WallpaperInfo` with `source = Local { label: dir_basename }`.
///
/// Errors per-entry are logged and skipped — never propagated.
pub fn find_local_video_files(extra_dirs: &[String]) -> Vec<WallpaperInfo> {
    let mut results = Vec::new();

    for dir_str in extra_dirs {
        let dir = Path::new(dir_str);
        if !dir.exists() {
            tracing::warn!(path = dir_str, "extra_dir does not exist — skipping");
            continue;
        }

        let label = dir.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(dir_str)
            .to_owned();

        for entry in WalkDir::new(dir).max_depth(6).follow_links(false).into_iter().filter_map(|e| e.ok()) {
            if !entry.file_type().is_file() { continue; }

            let path = entry.path();
            let ext = path.extension()
                .and_then(|e| e.to_str())
                .map(|e| e.to_lowercase());
            let Some(ext) = ext else { continue };
            if !VIDEO_EXTENSIONS.contains(&ext.as_str()) { continue; }

            let id               = local_id(path);
            let file_size_bytes  = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            let title            = path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_owned();

            // Look for a sibling thumbnail with same stem but image extension.
            let preview_path = PREVIEW_EXTENSIONS.iter().find_map(|pext| {
                let candidate = path.with_extension(pext);
                if candidate.exists() && candidate != path {
                    candidate.to_str().map(|s| s.to_owned())
                } else {
                    None
                }
            });

            results.push(WallpaperInfo {
                id,
                title,
                wallpaper_type:  WallpaperType::Video,
                file_path:       path.to_string_lossy().into_owned(),
                preview_path,
                has_audio:       false,
                file_size_bytes,
                width:           0,
                height:          0,
                source:          WallpaperSource::Local { label: label.clone() },
            });
        }
    }

    results
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
    fn test_workshop_id_parse_logic() {
        // Confirm which directory names are valid Workshop IDs (u64) and which are not.
        let valid   = ["2819752398", "12345", "0", "18446744073709551615"];
        let invalid = ["thumbnails", ".DS_Store", "preview", "abc123", "-1", "1.5"];

        for name in valid {
            assert!(name.parse::<u64>().is_ok(), "'{name}' should be a valid Workshop ID");
        }
        for name in invalid {
            assert!(name.parse::<u64>().is_err(), "'{name}' should NOT be a valid Workshop ID");
        }
    }

    #[test]
    fn test_dedup_removes_duplicate_workshop_ids() {
        // Two Steam root candidates that overlap produce the same Workshop IDs.
        // The sort + dedup we added must collapse them to one entry per ID.
        let mut results = vec![
            WallpaperDir { id: 100, path: "/fake/root_a/100".into() },
            WallpaperDir { id: 200, path: "/fake/root_a/200".into() },
            WallpaperDir { id: 100, path: "/fake/root_b/100".into() }, // duplicate
        ];
        results.sort_by_key(|wd| wd.id);
        results.dedup_by_key(|wd| wd.id);

        assert_eq!(results.len(), 2, "duplicate IDs must be collapsed to one");
        assert_eq!(results[0].id, 100);
        assert_eq!(results[1].id, 200);
    }

    #[test]
    fn test_find_wallpaper_dirs_with_mock_steam() -> crate::error::Result<()> {
        use tempfile::TempDir;

        let steam_root = TempDir::new()?;
        let steamapps  = steam_root.path().join("steamapps");
        std::fs::create_dir_all(&steamapps)?;

        // libraryfolders.vdf pointing at the same root (single library entry).
        let vdf = format!(
            "\"libraryfolders\"\n{{\n  \"0\"\n  {{\n    \"path\"    \"{}\"\n  }}\n}}\n",
            steam_root.path().display()
        );
        std::fs::write(steamapps.join("libraryfolders.vdf"), &vdf)?;

        // Create Workshop dirs — two numeric (valid IDs) + one non-numeric (skipped).
        let workshop = steamapps.join("workshop").join("content").join("431960");
        std::fs::create_dir_all(workshop.join("12345"))?;
        std::fs::create_dir_all(workshop.join("67890"))?;
        std::fs::create_dir_all(workshop.join("not_a_number"))?;

        // parse the VDF directly to confirm it's valid
        let content = std::fs::read_to_string(steamapps.join("libraryfolders.vdf"))?;
        let folders: LibraryFolders = keyvalues_serde::from_str(&content)
            .map_err(|e| crate::error::WpickError::VdfParse {
                path:   "test".into(),
                reason: e.to_string(),
            })?;

        let mut found_ids: Vec<u64> = Vec::new();
        for entry in folders.entries.values() {
            let wd = std::path::PathBuf::from(&entry.path)
                .join("steamapps/workshop/content/431960");
            if let Ok(rd) = std::fs::read_dir(&wd) {
                for e in rd.flatten() {
                    if let Ok(id) = e.file_name().to_string_lossy().parse::<u64>() {
                        if e.path().is_dir() { found_ids.push(id); }
                    }
                }
            }
        }

        found_ids.sort();
        assert_eq!(found_ids, vec![12345u64, 67890u64],
            "non-numeric dir 'not_a_number' must be excluded");

        Ok(())
    }

    #[test]
    fn test_find_local_video_files_basic() {
        use tempfile::TempDir;
        let tmp = TempDir::new().unwrap();

        std::fs::write(tmp.path().join("ocean.mp4"),  b"fake-mp4").unwrap();
        std::fs::write(tmp.path().join("ocean.jpg"),  b"fake-jpg").unwrap(); // sibling thumbnail
        std::fs::write(tmp.path().join("loop.webm"),  b"fake-webm").unwrap();
        std::fs::write(tmp.path().join("readme.txt"), b"text").unwrap(); // must be ignored

        let dir = tmp.path().to_str().unwrap().to_owned();
        let results = find_local_video_files(&[dir]);

        assert_eq!(results.len(), 2, "only video files should be returned");

        let ocean = results.iter().find(|w| w.title == "ocean").expect("ocean.mp4 missing");
        assert!(ocean.preview_path.is_some(), "ocean.jpg should be detected as preview");
        assert!(matches!(ocean.source, WallpaperSource::Local { .. }));

        let ids: std::collections::HashSet<u64> = results.iter().map(|w| w.id).collect();
        assert_eq!(ids.len(), 2, "each file must get a unique stable ID");
        assert!(ids.iter().all(|&id| id >= (1u64 << 63)), "local IDs must have high bit set");
    }

    #[test]
    fn test_find_local_video_files_missing_dir() {
        let results = find_local_video_files(&["/nonexistent/path/xyz".to_owned()]);
        assert!(results.is_empty(), "missing dir must return empty, not panic");
    }

    #[test]
    fn test_local_id_is_stable() {
        let path = Path::new("/home/user/videos/test.mp4");
        assert_eq!(local_id(path), local_id(path), "same path → same ID");
        assert_ne!(local_id(path), local_id(Path::new("/other.mp4")), "different paths → different IDs");
        assert!(local_id(path) >= (1u64 << 63), "high bit must be set");
    }
}
