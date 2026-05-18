use crate::error::{Result, WpickError};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ─── Config structs ───────────────────────────────────────────────────────────

/// Top-level config, written as `~/.config/wpick/config.toml`.
/// `#[serde(default)]` means missing sections use their `Default` values.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WpickConfig {
    pub general:          GeneralConfig,
    pub paths:            PathsConfig,
    pub wayland:          WaylandConfig,
    // v0.2 additions:
    pub monitors:         HashMap<String, MonitorConfig>,  // keyed by wl_output name
    pub pause:            PauseConfig,
    pub audio:            AudioConfig,
    pub autostart:        bool,
    // v0.4 additions:
    /// Last wallpaper set by the user; restored on daemon restart.
    pub last_wallpaper_id: Option<u64>,
    // v0.5 additions:
    pub tui:              TuiConfig,
    /// Global hotkey that opens the wpick TUI popup window.
    pub hotkey:           HotkeyConfig,
    /// Per-workspace wallpaper assignments: workspace name → wallpaper id.
    /// When the user switches workspace the daemon applies the assigned wallpaper
    /// on the focused monitor (Hyprland / Sway).  Use `id = 0` to clear a mapping.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub workspace_wallpapers: HashMap<String, u64>,
}

// ─── TUI config ───────────────────────────────────────────────────────────────

/// Per-slot color configuration — each value is an index into PALETTE in app.rs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiColorConfig {
    pub border_active:  usize,
    pub border_idle:    usize,
    pub border_overlay: usize,
    pub sel_bg:         usize,
    pub sel_fg:         usize,
    pub color_hint:     usize,
    pub color_playing:  usize,
    pub color_fav:      usize,
    pub col_title:      usize,  // column header title text color
    pub text_dim:       usize,  // secondary / dim text color
    pub vol_bar:        usize,  // volume bar fill color
}

impl Default for TuiColorConfig {
    fn default() -> Self {
        Self {
            border_active:  3,   // Light Gray
            border_idle:    1,   // Dark Gray
            border_overlay: 9,   // Soft Blue
            sel_bg:         6,   // Dark Blue
            sel_fg:         4,   // Near White
            color_hint:     7,   // Dim Blue
            color_playing:  14,  // Green
            color_fav:      15,  // Yellow
            col_title:      9,   // Soft Blue
            text_dim:       2,   // Mid Gray
            vol_bar:        9,   // Soft Blue
        }
    }
}

/// TUI-specific preferences: favorites, packs, panel visibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    /// Wallpaper IDs marked as favorites (shown in the Favorites nav section).
    pub favorites:    Vec<u64>,
    /// User-created named collections of wallpapers.
    pub packs:        Vec<Pack>,
    /// Show the left navigation panel (toggle with `[`).
    #[serde(default = "default_true")]
    pub show_nav:     bool,
    /// Show the right preview panel (toggle with `]`).
    #[serde(default = "default_true")]
    pub show_preview: bool,
    /// Visual theme preset: "dark" | "minimal" | "contrast"
    #[serde(default)]
    pub theme:        String,
    /// Per-slot color overrides (palette indices).
    #[serde(default)]
    pub colors:       TuiColorConfig,
    /// Position of now-playing title in header: "top-left" | "top-center" | "top-right"
    #[serde(default = "default_now_playing_pos")]
    pub now_playing_pos: String,
    /// Volume display style: "bar" | "slim" | "number"
    #[serde(default = "default_vol_style")]
    pub vol_style: String,
    /// Windowed mode: render in a centered sub-area instead of fullscreen.
    #[serde(default)]
    pub windowed: bool,
    /// Seconds after daemon start before reasserting Wayland surfaces.
    /// Fixes z-order when another layer-shell client (e.g. QuickShell) creates
    /// its background surface lazily and ends up on top of wpick.
    /// Set to 0 to disable. Default: 12.
    #[serde(default = "default_surface_reassert_secs")]
    pub surface_reassert_secs: u64,
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            favorites:       Vec::new(),
            packs:           Vec::new(),
            show_nav:        true,
            show_preview:    true,
            theme:           "dark".to_owned(),
            colors:          TuiColorConfig::default(),
            now_playing_pos: default_now_playing_pos(),
            vol_style:       default_vol_style(),
            windowed:               false,
            surface_reassert_secs:  default_surface_reassert_secs(),
        }
    }
}

fn default_true() -> bool { true }
fn default_now_playing_pos() -> String { "top-right".to_owned() }
fn default_vol_style() -> String { "slim".to_owned() }
fn default_surface_reassert_secs() -> u64 { 12 }

/// A named collection of wallpaper IDs created by the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pack {
    pub name: String,
    pub ids:  Vec<u64>,
}

/// Playback / audio settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    pub volume: f32,
    pub muted:  bool,
    /// When `true`, competing wallpaper tools are suspended (SIGSTOP) instead
    /// of terminated (SIGTERM/SIGKILL).  Suspended tools are resumed (SIGCONT)
    /// automatically when the wpick daemon exits.
    ///
    /// Default `false` (terminate).  Use `true` if you want competing tools to
    /// resume after wpick exits without needing to restart them manually.
    #[serde(default)]
    pub pause_competitors: bool,
    /// Hard cap on committed frames per second regardless of video FPS.
    /// Reducing this value lowers compositor CPU load (wl_shm upload frequency)
    /// which prevents cursor jitter on high-resolution displays.
    /// Set to 0 to disable (use native video FPS). Default: 30.
    #[serde(default = "default_max_fps")]
    pub max_fps: u32,
}

fn default_max_fps() -> u32 { 30 }

impl Default for GeneralConfig {
    fn default() -> Self {
        Self { volume: 0.8, muted: false, pause_competitors: false, max_fps: default_max_fps() }
    }
}

/// User-overridable filesystem paths.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PathsConfig {
    #[serde(default)]
    pub cache_dir: String,
    #[serde(default)]
    pub wallpapers_dir: String,
    /// Additional directories to scan for video files (mp4, webm, mkv, etc.).
    /// Each entry is an absolute path.  Wallpapers found here get
    /// `WallpaperSource::Local { label: <dirname> }`.
    #[serde(default)]
    pub extra_dirs: Vec<String>,
}

/// Wayland / GPU preferences.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WaylandConfig {
    pub preferred_gpu: String,
}

/// Per-monitor wallpaper configuration, keyed by wl_output name in `WpickConfig::monitors`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MonitorConfig {
    pub wallpaper_id: Option<u64>,
    pub fit:          FitMode,
    pub mute:         bool,
}

/// How the wallpaper video is scaled to fill the monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum FitMode {
    #[default]
    Fill,
    Fit,
    Stretch,
    Center,
}

/// Auto-pause triggers — all default-off except `on_fullscreen`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PauseConfig {
    pub on_fullscreen: bool,
    pub on_battery:    bool,
    pub on_lid_close:  bool,
}

impl Default for PauseConfig {
    fn default() -> Self {
        Self { on_fullscreen: true, on_battery: false, on_lid_close: false }
    }
}

/// Audio pipeline tuning parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Frames per streaming chunk sent from decoder thread to rodio sink.
    /// 2048 @ 48 kHz stereo ≈ 42 ms of audio per chunk.
    pub chunk_frames:    usize,
    /// Hard cap on total in-flight audio RAM, MB.
    pub max_preload_mb:  u64,
    /// Fade out wpick audio when another app plays sound.
    pub ducking_enabled: bool,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self { chunk_frames: 2048, max_preload_mb: 50, ducking_enabled: true }
    }
}

// ─── Hotkey config ────────────────────────────────────────────────────────────

/// Global hotkey that opens the wpick TUI in a floating popup terminal.
///
/// Requires the daemon user to be in the `input` group:
///   `sudo usermod -aG input $USER`  (re-login to apply)
///
/// Example config:
/// ```toml
/// [hotkey]
/// enabled  = true
/// keys     = "super+w"
/// terminal = "foot"     # auto-detected if empty
/// width    = 960
/// height   = 640
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    /// Enable the global hotkey listener.
    pub enabled:  bool,
    /// Key combination: modifiers joined by `+`, e.g. `"super+w"`, `"ctrl+shift+p"`.
    /// Supported modifiers: `super`, `ctrl`, `shift`, `alt`.
    pub keys:     String,
    /// Terminal emulator to launch. Auto-detected (foot → kitty → alacritty → xterm) if empty.
    pub terminal: String,
    /// Popup window width in pixels passed to the compositor.
    pub width:    u32,
    /// Popup window height in pixels passed to the compositor.
    pub height:   u32,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            enabled:  false,
            keys:     "super+w".to_owned(),
            terminal: String::new(),
            width:    960,
            height:   640,
        }
    }
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

    /// Save daemon-owned fields while preserving the `[tui]` section from the
    /// on-disk config.  The daemon never modifies `tui.*` (packs, favorites,
    /// theme, layout) — those are written exclusively by the TUI process.
    /// Without this merge, a daemon save (triggered by a wallpaper change,
    /// volume update, etc.) would overwrite the file with its stale in-memory
    /// copy and permanently erase any packs the user created in the TUI.
    pub fn save_preserving_tui(&self) -> Result<()> {
        // Propagate load errors rather than falling back to self.save().
        // Calling self.save() when load fails would overwrite the disk file
        // with the daemon's stale in-memory copy, permanently erasing any
        // packs or favorites the TUI process wrote since daemon startup.
        let disk = Self::load()?;
        let mut merged = self.clone();
        merged.tui = disk.tui;
        merged.save()
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

        // Prefer $XDG_RUNTIME_DIR/wpick.sock; fall back to ~/.wpick.sock for
        // environments where XDG_RUNTIME_DIR is unset (e.g. tty sessions).
        let socket_path = std::env::var_os("XDG_RUNTIME_DIR")
            .map(|d| PathBuf::from(d).join("wpick.sock"))
            .unwrap_or_else(|| home.join(".wpick.sock"));

        let dirs_out = AppDirs {
            config_file:    config.join("wpick").join("config.toml"),
            cache_dir:      cache.clone(),
            wallpapers_dir: cache.join("wallpapers"),
            db_path:        cache.join("wpick.db"),
            socket_path,
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
    fn test_default_config_general() {
        let cfg = WpickConfig::default();
        assert_eq!(cfg.general.volume, 0.8_f32);
        assert!(!cfg.general.muted,             "default muted must be false");
        assert!(!cfg.general.pause_competitors,  "default pause_competitors must be false");
    }

    #[test]
    fn test_load_returns_default_when_no_file() -> Result<()> {
        let tmp = TempDir::new()?;
        let non_existent = tmp.path().join("does_not_exist.toml");

        let cfg = WpickConfig::load_from(&non_existent)?;
        assert_eq!(cfg.general.volume, 0.8_f32);
        assert!(!cfg.general.muted);

        // v0.2 defaults present even when no file exists
        assert!(cfg.pause.on_fullscreen);
        assert!(!cfg.pause.on_battery);
        assert!(!cfg.pause.on_lid_close);
        assert_eq!(cfg.audio.chunk_frames, 2048);
        assert_eq!(cfg.audio.max_preload_mb, 50);
        assert!(cfg.audio.ducking_enabled);
        assert!(cfg.monitors.is_empty());
        assert!(!cfg.autostart);
        assert_eq!(cfg.audio.chunk_frames, 2048);
        Ok(())
    }

    #[test]
    fn test_save_and_reload() -> Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("config.toml");

        let mut cfg = WpickConfig::default();
        cfg.general.volume = 0.5;
        cfg.pause.on_battery = true;
        cfg.audio.chunk_frames = 4096;
        cfg.autostart = true;
        cfg.general.pause_competitors = true;
        cfg.monitors.insert("HDMI-A-1".into(), MonitorConfig {
            wallpaper_id: Some(12345),
            fit: FitMode::Stretch,
            mute: true,
        });
        cfg.save_to(&path)?;

        let reloaded = WpickConfig::load_from(&path)?;
        assert_eq!(reloaded.general.volume, 0.5_f32);
        assert!(reloaded.general.pause_competitors, "pause_competitors must round-trip");
        assert!(reloaded.pause.on_battery);
        assert_eq!(reloaded.audio.chunk_frames, 4096);
        assert!(reloaded.autostart);

        let mon = reloaded.monitors.get("HDMI-A-1").expect("HDMI-A-1 missing");
        assert_eq!(mon.wallpaper_id, Some(12345));
        assert_eq!(mon.fit, FitMode::Stretch);
        assert!(mon.mute);
        Ok(())
    }

    #[test]
    fn test_v01_config_forward_compat() -> Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("config.toml");

        // Minimal v0.1-era config — only [general] section
        std::fs::write(&path, r#"
[general]
volume = 0.5
muted  = true

[paths]
cache_dir = ""
"#)?;

        let cfg = WpickConfig::load_from(&path)?;

        // v0.1 fields preserved
        assert_eq!(cfg.general.volume, 0.5_f32);
        assert!(cfg.general.muted);

        // v0.2 fields all have correct defaults
        assert!(cfg.pause.on_fullscreen,  "pause.on_fullscreen should default to true");
        assert!(!cfg.pause.on_battery,    "pause.on_battery should default to false");
        assert!(!cfg.pause.on_lid_close,  "pause.on_lid_close should default to false");
        assert_eq!(cfg.audio.chunk_frames,   2048);
        assert_eq!(cfg.audio.max_preload_mb, 50);
        assert!(cfg.audio.ducking_enabled,   "audio.ducking_enabled should default to true");
        assert!(cfg.monitors.is_empty(),     "monitors should be empty when not in config");
        assert!(!cfg.autostart,              "autostart should default to false");
        Ok(())
    }

    #[test]
    fn test_v02_new_sections_round_trip() -> Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("config.toml");

        let mut cfg = WpickConfig::default();
        cfg.pause = PauseConfig { on_fullscreen: false, on_battery: true, on_lid_close: true };
        cfg.audio = AudioConfig { chunk_frames: 1024, max_preload_mb: 100, ducking_enabled: false };
        cfg.autostart = true;
        cfg.monitors.insert("DP-1".into(), MonitorConfig {
            wallpaper_id: Some(99999),
            fit: FitMode::Center,
            mute: false,
        });
        cfg.save_to(&path)?;

        let r = WpickConfig::load_from(&path)?;
        assert!(!r.pause.on_fullscreen);
        assert!(r.pause.on_battery);
        assert!(r.pause.on_lid_close);
        assert_eq!(r.audio.chunk_frames, 1024);
        assert_eq!(r.audio.max_preload_mb, 100);
        assert!(!r.audio.ducking_enabled);
        assert!(r.autostart);

        let dp1 = r.monitors.get("DP-1").expect("DP-1 missing");
        assert_eq!(dp1.wallpaper_id, Some(99999));
        assert_eq!(dp1.fit, FitMode::Center);
        assert!(!dp1.mute);
        Ok(())
    }

    #[test]
    fn test_fitmode_default_is_fill() {
        assert_eq!(FitMode::default(), FitMode::Fill);
    }

    #[test]
    fn test_last_wallpaper_id_round_trip() -> Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("config.toml");

        let mut cfg = WpickConfig::default();
        assert!(cfg.last_wallpaper_id.is_none(), "default must be None");

        cfg.last_wallpaper_id = Some(42);
        cfg.save_to(&path)?;

        let r = WpickConfig::load_from(&path)?;
        assert_eq!(r.last_wallpaper_id, Some(42));
        Ok(())
    }

    #[test]
    fn test_last_wallpaper_id_default_when_absent() -> Result<()> {
        let tmp = TempDir::new()?;
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[general]\nvolume = 0.5\nmuted = false\n")?;

        let cfg = WpickConfig::load_from(&path)?;
        assert!(cfg.last_wallpaper_id.is_none(), "must default to None when key absent");
        Ok(())
    }

    #[test]
    fn test_socket_path_uses_xdg_runtime_dir() -> Result<()> {
        let tmp = TempDir::new()?;
        // Temporarily override XDG_RUNTIME_DIR in this process.
        // SAFETY: single-threaded test, no concurrent env access.
        let original = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", tmp.path()); }

        let cfg  = WpickConfig::default();
        let dirs = cfg.app_dirs()?;
        let expected = tmp.path().join("wpick.sock");
        assert_eq!(dirs.socket_path, expected,
            "socket_path should be $XDG_RUNTIME_DIR/wpick.sock");

        // Restore
        match original {
            Some(v) => unsafe { std::env::set_var("XDG_RUNTIME_DIR", v); },
            None    => unsafe { std::env::remove_var("XDG_RUNTIME_DIR"); },
        }
        Ok(())
    }

    #[test]
    fn test_fitmode_serde_lowercase() {
        // TOML can't serialize a bare value at root (E-36) — wrap in a struct.
        #[derive(Serialize, Deserialize)]
        struct Wrap { fit: FitMode }

        let s = toml::to_string(&Wrap { fit: FitMode::Stretch }).unwrap();
        assert!(s.contains("stretch"), "FitMode::Stretch should serialize as 'stretch', got: {s}");

        let back: Wrap = toml::from_str("fit = \"center\"").unwrap();
        assert_eq!(back.fit, FitMode::Center);
    }
}
