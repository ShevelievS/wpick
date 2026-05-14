use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, watch};
use wpick_core::config::FitMode;
use wpick_core::model::WallpaperInfo;

pub struct DaemonState {
    pub current:         Option<WallpaperInfo>,
    pub volume:          f32,
    pub muted:           bool,
    pub wallpaper_tx:    watch::Sender<Option<WallpaperInfo>>,
    pub volume_tx:       watch::Sender<(f32, bool)>,
    pub shutdown_tx:     broadcast::Sender<()>,
    pub per_monitor_tx:  watch::Sender<HashMap<String, Option<WallpaperInfo>>>,
    /// (monitor_name_or_"*", fit) — "*" means all monitors.
    pub fit_tx:          watch::Sender<(String, FitMode)>,
    pub outputs:         Arc<Mutex<Vec<(String, u32, u32)>>>,
}

impl DaemonState {
    pub fn set_wallpaper(&mut self, info: WallpaperInfo) {
        self.current = Some(info.clone());
        let _ = self.wallpaper_tx.send(Some(info));
    }

    pub fn set_fit(&mut self, monitor: Option<String>, fit: FitMode) {
        let key = monitor.unwrap_or_else(|| "*".to_owned());
        let _ = self.fit_tx.send((key, fit));
    }

    /// Apply `info` to one specific monitor without changing the global wallpaper.
    pub fn set_wallpaper_for_monitor(&mut self, monitor: String, info: WallpaperInfo) {
        let mut pins = self.per_monitor_tx.borrow().clone();
        pins.insert(monitor, Some(info));
        let _ = self.per_monitor_tx.send(pins);
    }

    pub fn set_volume(&mut self, level: f32) {
        self.volume = level.clamp(0.0, 1.0);
        let _ = self.volume_tx.send((self.volume, self.muted));
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        let _ = self.volume_tx.send((self.volume, self.muted));
    }
}
