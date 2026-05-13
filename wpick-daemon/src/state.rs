use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{broadcast, watch};
use wpick_core::model::WallpaperInfo;

pub struct DaemonState {
    pub current:         Option<WallpaperInfo>,
    pub volume:          f32,
    pub muted:           bool,
    /// Single channel — both renderer and audio subscribe to the same receiver.
    /// Sending once guarantees they see the change in the same Tokio tick,
    /// eliminating the A/V skew caused by two sequential sends.
    pub wallpaper_tx:    watch::Sender<Option<WallpaperInfo>>,
    pub volume_tx:       watch::Sender<(f32, bool)>,
    pub shutdown_tx:     broadcast::Sender<()>,
    /// Per-monitor wallpaper overrides — `None` in the map value means "unpin".
    /// Renderer subscribes via `per_monitor_rx` and updates surface decoders.
    pub per_monitor_tx:  watch::Sender<HashMap<String, Option<WallpaperInfo>>>,
    /// Connected wl_output names published by the renderer after each init.
    pub outputs:         Arc<Mutex<Vec<String>>>,
    /// Running wpick-webview child processes (one per active web wallpaper).
    /// Killed when a new wallpaper is set or the daemon exits.
    pub webview_children: Arc<Mutex<Vec<std::process::Child>>>,
}

impl DaemonState {
    pub fn set_wallpaper(&mut self, info: WallpaperInfo) {
        self.current = Some(info.clone());
        let _ = self.wallpaper_tx.send(Some(info));
    }

    #[allow(dead_code)]
    pub fn stop(&mut self) {
        self.current = None;
        let _ = self.wallpaper_tx.send(None);
    }

    /// Clear the current wallpaper and tell the renderer to blank its surface.
    /// Used when a web wallpaper is set so the wl_shm surface stops occluding
    /// the wpick-webview GTK layer-shell surface.
    pub fn clear_wallpaper(&mut self) {
        self.current = None;
        let _ = self.wallpaper_tx.send(None);
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
