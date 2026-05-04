use tokio::sync::{broadcast, watch};
use wpick_core::model::WallpaperInfo;

pub struct DaemonState {
    pub current:      Option<WallpaperInfo>,
    pub volume:       f32,
    pub muted:        bool,
    /// Single channel — both renderer and audio subscribe to the same receiver.
    /// Sending once guarantees they see the change in the same Tokio tick,
    /// eliminating the A/V skew caused by two sequential sends.
    pub wallpaper_tx: watch::Sender<Option<WallpaperInfo>>,
    pub volume_tx:    watch::Sender<(f32, bool)>,
    pub shutdown_tx:  broadcast::Sender<()>,
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

    pub fn set_volume(&mut self, level: f32) {
        self.volume = level.clamp(0.0, 1.0);
        let _ = self.volume_tx.send((self.volume, self.muted));
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        let _ = self.volume_tx.send((self.volume, self.muted));
    }
}
