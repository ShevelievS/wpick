use crate::error::{Result, WpickError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// ─── Config structs ───────────────────────────────────────────────────────────

/// Top-level config, written as `~/.config/wpick/config.toml`.
/// `#[serde(default)]` means missing sections use their `Default` values.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WpickConfig {
    pub general: GeneralConfig,
    pub paths:   PathsConfig,
    pub wayland: WaylandConfig,
}

/// Playback / audio settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub volume: f32,
    pub muted:  bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self { volume: 0.8, muted: false }
    }
}

/// User-overridable filesystem paths.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathsConfig {
    #[serde(default)]
    pub cache_dir: String,
    #[serde(default)]
    pub wallpapers_dir: String,
}

/// Wayland / GPU preferences.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WaylandConfig {
    pub preferred_gpu: String,
}

// ─── AppDirs ─────────────────────────────────────────────────────────────────

/// Resolved runtime paths derived from `WpickConfig`.
/// Directories that must exist are created by `WpickConfig::app_dirs()`.
#[derive(Clone)]
pub struct AppDirs {
    /// Path to the TOML config file itself (not a dir — not created automatically)
    pub config_file:    PathBuf,
    /// Root cache directory (created by `app_dirs()`)
    pub cache_dir:      PathBuf,
    /// Extracted wallpaper assets live here (created by `app_dirs()`)
    pub wallpapers_dir: PathBuf,
    /// SQLite database path (not created — just a path inside cache_dir)
    pub db_path:        PathBuf,
    /// Unix domain socket at `$HOME/.wpick.sock` (not created — daemon owns it)
    pub socket_path:    PathBuf,
    /// Log file directory (created by `app_dirs()`)
    pub log_dir:        PathBuf,
}

// ─── WpickConfig impl ────────────────────────────────────────────────────────

impl WpickConfig {
    /// Load config from the canonical XDG location.
    ///
    /// - Config dir missing → created, returns `Default`
    /// - Config file missing → returns `Default` (not an error)
    /// - Config file present but invalid TOML → `Err(WpickError::ConfigToml)`
    pub fn load() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| WpickError::Config("No XDG config dir available".into()))?
            .join("wpick");

        if !config_dir.exists() {
            std::fs::create_dir_all(&config_dir)?;
            return Ok(Self::default());
        }

        Self::load_from(&config_dir.join("config.toml"))
    }

    /// Load config from an explicit path. Returns `Default` when file does not exist.
    pub fn load_from(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)?;
        let cfg: Self = toml::from_str(&content)?;
        Ok(cfg)
    }

    /// Atomically save config to the canonical XDG location.
    pub fn save(&self) -> Result<()> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| WpickError::Config("No XDG config dir available".into()))?
            .join("wpick");

        std::fs::create_dir_all(&config_dir)?;
        self.save_to(&config_dir.join("config.toml"))
    }

    /// Atomically save config to an explicit path (write to `.tmp`, then rename).
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| WpickError::Config("Invalid config file path".into()))?;

        let tmp_path = path.with_file_name(format!("{}.tmp", file_name));

        let content = toml::to_string(self)
            .map_err(|e| WpickError::Config(format!("TOML serialization failed: {}", e)))?;

        std::fs::write(&tmp_path, content)?;
        std::fs::rename(&tmp_path, path)?;
        Ok(())
    }

    /// Resolve and return all runtime paths.
    ///
    /// Creates `cache_dir`, `wallpapers_dir`, and `log_dir`.
    /// Does **not** create `config_file`, `socket_path`, or `db_path`.
    pub fn app_dirs(&self) -> Result<AppDirs> {
        let home = dirs::home_dir()
            .ok_or_else(|| WpickError::Config("No home dir".into()))?;
        let config = dirs::config_dir()
            .ok_or_else(|| WpickError::Config("No config dir".into()))?;

        let cache = if self.paths.cache_dir.is_empty() {
            dirs::cache_dir()
                .ok_or_else(|| WpickError::Config("No cache dir".into()))?
                .join("wpick")
        } else {
            PathBuf::from(&self.paths.cache_dir)
        };

        let log_dir = dirs::data_local_dir()
            .unwrap_or_else(|| home.join(".local/share"))
            .join("wpick");

        let dirs_out = AppDirs {
            config_file:    config.join("wpick").join("config.toml"),
            cache_dir:      cache.clone(),
            wallpapers_dir: cache.join("wallpapers"),
            db_path:        cache.join("wpick.db"),
            socket_path:    home.join(".wpick.sock"),
            log_dir:        log_dir.clone(),
        };

        // Create directories that must exist at runtime
        std::fs::create_dir_all(&dirs_out.cache_dir)?;
        std::fs::create_dir_all(&dirs_out.wallpapers_dir)?;
        std::fs::create_dir_all(&dirs_out.log_dir)?;

        Ok(dirs_out)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config_volume() {
        assert_eq!(WpickConfig::default().general.volume, 0.8_f32);
    }

    #[test]
    fn test_load_returns_default_when_no_file() -> Result<()> {
        let tmp = TempDir::new()?;
        let non_existent = tmp.path().join("does_not_exist.toml");

        let cfg = WpickConfig::load_from(&non_existent)?;
        assert_eq!(cfg.general.volume, 0.8_f32);
        assert!(!cfg.general.muted);
        Ok(())
    }

    #[test]
    fn test_save_and_reload() -> Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("config.toml");

        let mut cfg = WpickConfig::default();
        cfg.general.volume = 0.5;
        cfg.save_to(&path)?;

        let reloaded = WpickConfig::load_from(&path)?;
        assert_eq!(reloaded.general.volume, 0.5_f32);
        Ok(())
    }
}
