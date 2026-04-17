use tokio::sync::{broadcast, watch};
use wpick_core::model::WallpaperInfo;

pub struct DaemonState {
    pub current:     Option<WallpaperInfo>,
    pub volume:      f32,
    pub muted:       bool,
    pub paused:      bool,
    pub renderer_tx: watch::Sender<Option<WallpaperInfo>>,
    pub audio_tx:    watch::Sender<Option<WallpaperInfo>>,
    pub volume_tx:   watch::Sender<(f32, bool)>,
    pub pause_tx:    watch::Sender<bool>,
    pub shutdown_tx: broadcast::Sender<()>,
}

impl DaemonState {
    pub fn set_wallpaper(&mut self, info: WallpaperInfo) {
        self.current = Some(info.clone());
        let _ = self.renderer_tx.send(Some(info.clone()));
        let _ = self.audio_tx.send(Some(info));
    }

    #[allow(dead_code)]
    pub fn stop(&mut self) {
        self.current = None;
        let _ = self.renderer_tx.send(None);
        let _ = self.audio_tx.send(None);
    }

    pub fn set_volume(&mut self, level: f32) {
        self.volume = level.clamp(0.0, 1.0);
        let _ = self.volume_tx.send((self.volume, self.muted));
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        let _ = self.volume_tx.send((self.volume, self.muted));
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        let _ = self.pause_tx.send(paused);
    }
}
