use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::io::Stdout;
use std::time::Duration;
use wpick_core::config::{AppDirs, FitMode, WpickConfig};
use wpick_core::ipc::{ClientCommand, DaemonResponse};
use wpick_core::model::{WallpaperInfo, WallpaperSource};

use crate::client::IpcClient;
use crate::ui;

/// Shallow scan hint for a directory entry in the folder picker.
#[derive(Debug, Clone, PartialEq)]
pub enum FpHint {
    HasVideos,   // contains video files directly — show green
    HasSubdirs,  // no direct videos but has sub-dirs — show yellow
    Empty,       // nothing inside — show dim
    Unreadable,  // permission denied — show dim
    System,      // virtual/OS filesystem — blocked, show red
}

/// Source-based filter: All wallpapers, Workshop only, or a specific local folder by label.
#[derive(Debug, Clone, PartialEq)]
pub enum FilterType {
    All,
    Workshop,
    Local(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    Browse,
    Detail,
    FolderPicker,
}

pub struct App {
    pub config:                  WpickConfig,
    pub dirs:                    AppDirs,
    pub client:                  Option<IpcClient>,
    pub wallpapers:              Vec<WallpaperInfo>,
    pub selected:                usize,
    pub list_state:              ListState,
    pub current_wallpaper_id:    Option<u64>,
    pub mode:                    AppMode,
    pub status_message:          Option<String>,
    pub status_is_error:         bool,
    pub status_clear_at:         Option<std::time::Instant>,
    pub daemon_connected:        bool,
    pub loading:                 bool,
    pub should_quit:             bool,
    pub last_reconnect_attempt:  Option<std::time::Instant>,
    pub search_query:            String,
    pub search_active:           bool,
    pub filter_type:             FilterType,
    // Image preview
    pub picker:                  Option<Picker>,
    pub preview:                 Option<StatefulProtocol>,
    pub preview_id:              Option<u64>,
    /// Current active fit mode (for display and cycling).
    pub current_fit:             FitMode,
    // Monitor selector
    /// Connected wl_output names and resolutions fetched from the daemon.
    pub monitors:                Vec<(String, u32, u32)>,
    /// Whether the monitor-picker overlay is open.
    pub monitor_select_mode:     bool,
    /// Cursor inside the monitor picker (0 = "All monitors").
    pub monitor_selected:        usize,
    // ── Folder picker ────────────────────────────────────────────────────────
    /// Current directory being browsed in the folder picker.
    pub fp_path:                 std::path::PathBuf,
    /// Sorted list of subdirectory names in fp_path (first entry is ".." if not root).
    pub fp_entries:              Vec<String>,
    /// Selected index inside fp_entries.
    pub fp_selected:             usize,
    /// Shallow content hints for each entry in fp_entries (keyed by full path).
    pub fp_hints:                std::collections::HashMap<std::path::PathBuf, FpHint>,
}

impl App {
    pub fn new(config: WpickConfig, dirs: AppDirs, picker: Picker) -> Self {
        Self {
            config,
            dirs,
            client:                  None,
            wallpapers:              Vec::new(),
            selected:                0,
            list_state:              ListState::default(),
            current_wallpaper_id:    None,
            mode:                    AppMode::Browse,
            status_message:          None,
            status_is_error:         false,
            status_clear_at:         None,
            daemon_connected:        false,
            loading:                 false,
            should_quit:             false,
            last_reconnect_attempt:  None,
            search_query:            String::new(),
            search_active:           false,
            filter_type:             FilterType::All,
            current_fit:             FitMode::default(),
            picker:                  Some(picker),
            preview:                 None,
            preview_id:              None,
            monitors:                Vec::new(),
            monitor_select_mode:     false,
            monitor_selected:        0,
            fp_path:                 std::env::var("HOME")
                                         .map(std::path::PathBuf::from)
                                         .unwrap_or_else(|_| std::path::PathBuf::from("/")),
            fp_entries:              Vec::new(),
            fp_selected:             0,
            fp_hints:                std::collections::HashMap::new(),
        }
    }

    /// Load (or reload) the preview image for the currently selected wallpaper.
    /// No-op if the same wallpaper is already loaded.
    /// Sets `preview = None` silently on any error (missing file, unsupported format, etc.).
    pub fn update_preview(&mut self) {
        if self.picker.is_none() {
            self.preview = None;
            return;
        }

        let (id, preview_path) = {
            let filtered = self.filtered_wallpapers();
            match filtered.get(self.selected) {
                Some(w) => (w.id, w.preview_path.clone()),
                None => {
                    self.preview    = None;
                    self.preview_id = None;
                    return;
                }
            }
        };

        if self.preview_id == Some(id) {
            return;
        }

        self.preview    = None;
        self.preview_id = Some(id);

        let path = match preview_path {
            Some(p) => p,
            None    => return,
        };

        if let Ok(img) = image::open(&path) {
            let protocol = self.picker.as_ref().unwrap().new_resize_protocol(img);
            self.preview = Some(protocol);
        }
    }

    pub fn set_status_ok(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
        self.status_is_error = false;
        self.status_clear_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
    }

    pub fn set_status_error(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
        self.status_is_error = true;
        self.status_clear_at = Some(std::time::Instant::now() + std::time::Duration::from_secs(3));
    }

    pub fn filtered_wallpapers(&self) -> Vec<&WallpaperInfo> {
        self.wallpapers.iter().filter(|w| {
            let source_ok = match &self.filter_type {
                FilterType::All          => true,
                FilterType::Workshop     => w.source == WallpaperSource::Workshop,
                FilterType::Local(label) => matches!(
                    &w.source, WallpaperSource::Local { label: l } if l == label
                ),
            };
            let search_ok = self.search_query.is_empty()
                || w.title.to_lowercase().contains(&self.search_query.to_lowercase());
            source_ok && search_ok
        }).collect()
    }

    /// Collect distinct source labels from the wallpaper list for Tab cycling.
    /// Order: Workshop first, then Local labels alphabetically.
    pub fn available_sources(&self) -> Vec<FilterType> {
        let mut sources = vec![FilterType::All];
        let has_workshop = self.wallpapers.iter()
            .any(|w| w.source == WallpaperSource::Workshop);
        if has_workshop {
            sources.push(FilterType::Workshop);
        }
        let mut local_labels: Vec<String> = self.wallpapers.iter()
            .filter_map(|w| match &w.source {
                WallpaperSource::Local { label } => Some(label.clone()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        local_labels.sort();
        for label in local_labels {
            sources.push(FilterType::Local(label));
        }
        sources
    }

    fn select_next(&mut self) {
        let len = self.filtered_wallpapers().len();
        if len > 0 {
            self.selected = (self.selected + 1).min(len - 1);
            self.list_state.select(Some(self.selected));
            self.update_preview();
        }
    }

    fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.list_state.select(Some(self.selected));
            self.update_preview();
        }
    }

    pub async fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        self.try_reconnect(terminal).await;

        loop {
            if self.client.is_none() {
                self.try_reconnect(terminal).await;
            }

            if let Some(clear_at) = self.status_clear_at {
                if std::time::Instant::now() >= clear_at {
                    self.status_message = None;
                    self.status_clear_at = None;
                }
            }

            terminal.draw(|f| ui::render(f, self))?;

            if crossterm::event::poll(Duration::from_millis(250))? {
                if let Event::Key(key) = crossterm::event::read()? {
                    self.handle_key(key, terminal).await;
                }
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        // ── Folder picker overlay ─────────────────────────────────────────────
        if self.mode == AppMode::FolderPicker {
            self.handle_key_folder_picker(key, terminal).await;
            return;
        }

        // ── Monitor selector overlay ──────────────────────────────────────────
        if self.monitor_select_mode {
            match key.code {
                KeyCode::Esc => {
                    self.monitor_select_mode = false;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.monitor_selected > 0 {
                        self.monitor_selected -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    // 0 = "All monitors", 1..=N = specific monitors
                    let max = self.monitors.len(); // len()+1 items total, max idx = len()
                    if self.monitor_selected < max {
                        self.monitor_selected += 1;
                    }
                }
                KeyCode::Enter => {
                    self.monitor_select_mode = false;
                    self.cmd_set_to_selected_monitor().await;
                }
                _ => {}
            }
            return;
        }

        if self.search_active {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.cmd_kill().await;
                self.should_quit = true;
                return;
            }
            match key.code {
                KeyCode::Esc | KeyCode::Enter => {
                    self.search_active = false;
                }
                KeyCode::Backspace => {
                    if self.search_query.pop().is_some() {
                        self.selected = 0;
                        let empty = self.filtered_wallpapers().is_empty();
                        self.list_state.select(if empty { None } else { Some(0) });
                        self.preview_id = None;
                        self.update_preview();
                    }
                }
                KeyCode::Char(c) => {
                    self.search_query.push(c);
                    self.selected = 0;
                    let empty = self.filtered_wallpapers().is_empty();
                    self.list_state.select(if empty { None } else { Some(0) });
                    self.preview_id = None;
                    self.update_preview();
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.should_quit = true;
            }
            KeyCode::Char('Q') => {
                self.cmd_kill().await;
                self.should_quit = true;
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cmd_kill().await;
                self.should_quit = true;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
            }
            KeyCode::Enter => {
                self.cmd_set().await;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.cmd_volume_up().await;
            }
            KeyCode::Char('-') => {
                self.cmd_volume_down().await;
            }
            KeyCode::Char('m') => {
                self.cmd_mute().await;
            }
            KeyCode::Char('r') => {
                self.refresh_monitors().await;
                self.refresh_list(terminal).await;
            }
            KeyCode::Char('M') => {
                if !self.monitors.is_empty() {
                    self.monitor_select_mode = true;
                    self.monitor_selected    = 0;
                } else {
                    self.set_status_error("No monitors reported by daemon (try 'r' to refresh)");
                }
            }
            KeyCode::Char('s') => {
                self.open_folder_picker();
            }
            KeyCode::Char('f') => {
                self.cmd_cycle_fit().await;
            }
            KeyCode::Char('i') => {
                self.mode = match self.mode {
                    AppMode::Browse       => AppMode::Detail,
                    AppMode::Detail       => AppMode::Browse,
                    AppMode::FolderPicker => AppMode::Browse,
                };
            }
            KeyCode::Char('/') => {
                self.search_active = true;
                self.search_query = String::new();
                self.selected = 0;
                let empty = self.filtered_wallpapers().is_empty();
                self.list_state.select(if empty { None } else { Some(0) });
                self.preview_id = None;
                self.update_preview();
            }
            KeyCode::Tab => {
                let sources = self.available_sources();
                if sources.len() > 1 {
                    let cur = sources.iter().position(|s| s == &self.filter_type)
                        .unwrap_or(0);
                    self.filter_type = sources[(cur + 1) % sources.len()].clone();
                }
                self.selected = 0;
                let empty = self.filtered_wallpapers().is_empty();
                self.list_state.select(if empty { None } else { Some(0) });
                self.preview_id = None;
                self.update_preview();
            }
            _ => {}
        }
    }

    async fn cmd_set(&mut self) {
        let id = match self.filtered_wallpapers().get(self.selected) {
            Some(w) => w.id,
            None => return,
        };
        match self.send(ClientCommand::Set { id, monitor: None }).await {
            Ok(_) => {
                self.current_wallpaper_id = Some(id);
                self.set_status_ok("\u{2713} Applied");
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    /// Apply the selected wallpaper to the monitor chosen in the monitor selector.
    /// `monitor_selected = 0` means "all monitors" (no monitor filter).
    async fn cmd_set_to_selected_monitor(&mut self) {
        let id = match self.filtered_wallpapers().get(self.selected) {
            Some(w) => w.id,
            None => return,
        };
        let monitor = if self.monitor_selected == 0 {
            None
        } else {
            self.monitors.get(self.monitor_selected - 1).map(|(n, _, _)| n.clone())
        };
        match self.send(ClientCommand::Set { id, monitor }).await {
            Ok(_) => {
                self.current_wallpaper_id = Some(id);
                self.set_status_ok("\u{2713} Applied");
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    async fn cmd_volume_up(&mut self) {
        let new_vol = (self.config.general.volume + 0.05).clamp(0.0, 1.0);
        match self.send(ClientCommand::Volume { level: new_vol }).await {
            Ok(DaemonResponse::VolumeState { volume, muted, .. }) => {
                self.config.general.volume = volume;
                self.config.general.muted  = muted;
                self.set_status_ok(format!("Vol {:.0}%", volume * 100.0));
            }
            Ok(_) => {
                self.config.general.volume = new_vol;
                self.set_status_ok(format!("Vol {:.0}%", new_vol * 100.0));
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    async fn cmd_volume_down(&mut self) {
        let new_vol = (self.config.general.volume - 0.05).clamp(0.0, 1.0);
        match self.send(ClientCommand::Volume { level: new_vol }).await {
            Ok(DaemonResponse::VolumeState { volume, muted, .. }) => {
                self.config.general.volume = volume;
                self.config.general.muted  = muted;
                self.set_status_ok(format!("Vol {:.0}%", volume * 100.0));
            }
            Ok(_) => {
                self.config.general.volume = new_vol;
                self.set_status_ok(format!("Vol {:.0}%", new_vol * 100.0));
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    async fn cmd_mute(&mut self) {
        match self.send(ClientCommand::Mute).await {
            Ok(DaemonResponse::VolumeState { volume, muted, .. }) => {
                // Use the authoritative daemon state — no client-side guessing.
                self.config.general.volume = volume;
                self.config.general.muted  = muted;
                let label = if muted { "Muted" } else { "Unmuted" };
                self.set_status_ok(label);
            }
            Ok(_) => {
                self.config.general.muted = !self.config.general.muted;
                let label = if self.config.general.muted { "Muted" } else { "Unmuted" };
                self.set_status_ok(label);
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    async fn cmd_cycle_fit(&mut self) {
        let next = match self.current_fit {
            FitMode::Fit     => FitMode::Fill,
            FitMode::Fill    => FitMode::Stretch,
            FitMode::Stretch => FitMode::Center,
            FitMode::Center  => FitMode::Fit,
        };
        let monitor = self.monitors.get(
            self.monitor_selected.saturating_sub(1)
        ).map(|(n, _, _)| n.clone());
        match self.send(ClientCommand::SetFit { fit: next, monitor }).await {
            Ok(_) => {
                self.current_fit = next;
                self.set_status_ok(format!("Fit: {}", fit_label(next)));
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    async fn cmd_kill(&mut self) {
        let _ = self.send(ClientCommand::Kill).await;
    }

    pub async fn refresh_list(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        self.loading = true;
        let prev_id = self.filtered_wallpapers().get(self.selected).map(|w| w.id);

        // Send Scan command — borrow client briefly, then release.
        let send_ok = match self.client.as_mut() {
            None => {
                self.daemon_connected = false;
                self.loading = false;
                return;
            }
            Some(c) => tokio::time::timeout(
                Duration::from_secs(5),
                c.send_cmd_only(&wpick_core::ipc::ClientCommand::Scan),
            ).await,
        };
        if let Err(_) | Ok(Err(_)) = send_ok {
            self.client = None;
            self.daemon_connected = false;
            self.set_status_error("Scan failed (send)");
            self.loading = false;
            return;
        }

        // Drain responses: ScanProgress* then WallpaperList (or Error).
        // Use a short per-iteration timeout so we can poll key events between
        // each recv — this keeps the UI responsive during long scans.
        let items = loop {
            let _ = terminal.draw(|f| ui::render(f, self));

            // Non-blocking check for Esc — lets the user cancel a slow scan.
            if crossterm::event::poll(Duration::ZERO).unwrap_or(false) {
                if let Ok(Event::Key(k)) = crossterm::event::read() {
                    if k.code == KeyCode::Esc {
                        self.set_status_error("Scan cancelled");
                        self.loading = false;
                        break None;
                    }
                }
            }

            let resp = match self.client.as_mut() {
                None => break None,
                // Short timeout so we return to the key-poll above often enough
                // to feel responsive, while still batching daemon messages.
                Some(c) => tokio::time::timeout(
                    Duration::from_millis(50),
                    c.recv_resp(),
                ).await,
            };

            match resp {
                Ok(Ok(DaemonResponse::ScanProgress { done, total })) => {
                    self.status_message  = Some(format!("Scanning… {done}/{total}  [Esc] cancel"));
                    self.status_is_error = false;
                    self.status_clear_at = None;
                }
                Ok(Ok(DaemonResponse::WallpaperList { items })) => break Some(items),
                Ok(Ok(DaemonResponse::Error { message })) => {
                    self.set_status_error(message);
                    break None;
                }
                Ok(Ok(_)) => break None,
                Ok(Err(e)) => {
                    self.client = None;
                    self.daemon_connected = false;
                    self.set_status_error(e.to_string());
                    break None;
                }
                // Short timeout — just loop again to redraw and check keys.
                Err(_timeout) => continue,
            }
        };

        if let Some(items) = items {
            self.preview_id = None; // force preview reload for new list
            self.wallpapers = items;
            if let Some(id) = prev_id {
                if let Some(pos) = self.filtered_wallpapers().iter().position(|w| w.id == id) {
                    self.selected = pos;
                    let empty = self.filtered_wallpapers().is_empty();
                    self.list_state.select(if empty { None } else { Some(pos) });
                    self.loading = false;
                    self.update_preview();
                    return;
                }
            }
            let filtered_len = self.filtered_wallpapers().len();
            self.selected = self.selected.min(filtered_len.saturating_sub(1));
            self.list_state.select(if filtered_len == 0 { None } else { Some(self.selected) });
            self.update_preview();
        }

        self.loading = false;
    }

    async fn send(&mut self, cmd: ClientCommand) -> anyhow::Result<DaemonResponse> {
        let client = match self.client.as_mut() {
            None => {
                self.daemon_connected = false;
                anyhow::bail!("Not connected to daemon");
            }
            Some(c) => c,
        };

        // 2-second timeout: if the daemon is unresponsive (e.g. after a Kill that
        // closed the socket without sending a response), recv_response would block
        // forever. The timeout drops the connection and shows an error instead.
        match tokio::time::timeout(
            std::time::Duration::from_secs(2),
            client.send(&cmd),
        ).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e)) => {
                self.client = None;
                self.daemon_connected = false;
                Err(e)
            }
            Err(_timeout) => {
                self.client = None;
                self.daemon_connected = false;
                anyhow::bail!("Daemon did not respond (timeout)")
            }
        }
    }

    pub async fn try_reconnect(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        if let Some(last) = self.last_reconnect_attempt {
            if last.elapsed() < std::time::Duration::from_secs(2) {
                return;
            }
        }
        self.last_reconnect_attempt = Some(std::time::Instant::now());

        if let Some(client) = tokio::time::timeout(
            Duration::from_millis(200),
            IpcClient::try_connect(&self.dirs.socket_path),
        )
        .await
        .unwrap_or(None)
        {
            self.client = Some(client);
            self.daemon_connected = true;
            self.last_reconnect_attempt = None;
            self.sync_volume_state().await;
            self.refresh_monitors().await;
            self.refresh_list(terminal).await;
        }
    }

    /// Query the daemon for the list of connected monitors.
    pub async fn refresh_monitors(&mut self) {
        match self.send(ClientCommand::ListOutputs).await {
            Ok(DaemonResponse::OutputList { names, resolutions }) => {
                self.monitors = names.into_iter()
                    .enumerate()
                    .map(|(i, name)| {
                        let (w, h) = resolutions.get(i).copied().unwrap_or((0, 0));
                        (name, w, h)
                    })
                    .collect();
            }
            _ => {}
        }
    }

    /// Returns the resolution of the first connected monitor, if known.
    pub fn screen_resolution_for_wallpaper(&self, _info: &WallpaperInfo) -> Option<(u32, u32)> {
        self.monitors.first().map(|(_, w, h)| (*w, *h))
    }

    /// Query the daemon for current volume/muted/wallpaper and update local state.
    async fn sync_volume_state(&mut self) {
        match self.send(ClientCommand::Status).await {
            Ok(DaemonResponse::VolumeState { volume, muted, current_id }) => {
                self.config.general.volume = volume;
                self.config.general.muted  = muted;
                if current_id.is_some() {
                    self.current_wallpaper_id = current_id;
                }
            }
            Ok(_) | Err(_) => {}
        }
    }
}

// ── Folder picker helpers ─────────────────────────────────────────────────────

const VIDEO_EXTS_FP: &[&str] = &["mp4", "webm", "mkv", "avi", "mov", "gif", "wmv", "flv"];

/// Virtual / OS filesystems that should never be added as wallpaper dirs.
const SYSTEM_PATHS: &[&str] = &[
    "/proc", "/sys", "/dev", "/run",
];

pub fn fp_is_system(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    SYSTEM_PATHS.iter().any(|sp| s == *sp || s.starts_with(&format!("{}/", sp)))
}

/// Shallow (non-recursive) directory scan — reads at most 500 entries.
/// Returns a hint that describes what the directory visibly contains.
pub fn fp_dir_hint(path: &std::path::Path) -> FpHint {
    if fp_is_system(path) { return FpHint::System; }
    let Ok(rd) = std::fs::read_dir(path) else { return FpHint::Unreadable };
    let mut has_subdirs = false;
    for entry in rd.flatten().take(500) {
        let p = entry.path();
        if p.is_file() {
            if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
                if VIDEO_EXTS_FP.contains(&ext.to_lowercase().as_str()) {
                    return FpHint::HasVideos;
                }
            }
        } else if p.is_dir() {
            has_subdirs = true;
        }
    }
    if has_subdirs { FpHint::HasSubdirs } else { FpHint::Empty }
}

// ── Folder picker ─────────────────────────────────────────────────────────────

impl App {
    /// Open the folder picker starting at home dir (or last browsed path).
    pub fn open_folder_picker(&mut self) {
        self.mode = AppMode::FolderPicker;
        self.load_fp_entries();
    }

    /// Read subdirectories of `fp_path` into `fp_entries` and compute hints.
    pub fn load_fp_entries(&mut self) {
        let mut entries: Vec<String> = std::fs::read_dir(&self.fp_path)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.')) // hide dotfiles by default
            .collect();
        entries.sort();

        // Prepend ".." unless we're at root.
        if self.fp_path.parent().is_some() {
            entries.insert(0, "..".to_owned());
        }
        self.fp_entries = entries;
        self.fp_selected = self.fp_selected.min(
            self.fp_entries.len().saturating_sub(1)
        );

        // Compute shallow hints for each entry (skip ".." — it's just navigation).
        self.fp_hints.clear();
        for name in &self.fp_entries {
            if name == ".." { continue; }
            let full = self.fp_path.join(name);
            self.fp_hints.insert(full.clone(), fp_dir_hint(&full));
        }
    }

    async fn handle_key_folder_picker(
        &mut self,
        key: KeyEvent,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) {
        match key.code {
            // ── Navigation ──────────────────────────────────────────────────
            KeyCode::Up | KeyCode::Char('k') => {
                if self.fp_selected > 0 {
                    self.fp_selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.fp_selected + 1 < self.fp_entries.len() {
                    self.fp_selected += 1;
                }
            }

            // ── Enter directory ─────────────────────────────────────────────
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(name) = self.fp_entries.get(self.fp_selected).cloned() {
                    let next = if name == ".." {
                        self.fp_path.parent()
                            .map(|p| p.to_path_buf())
                            .unwrap_or_else(|| self.fp_path.clone())
                    } else {
                        self.fp_path.join(&name)
                    };
                    if fp_is_system(&next) {
                        self.set_status_error("System path — cannot browse");
                        return;
                    }
                    self.fp_path     = next;
                    self.fp_selected = 0;
                    self.load_fp_entries();
                }
            }

            // ── Go up (backspace / left) ─────────────────────────────────────
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                if let Some(parent) = self.fp_path.parent().map(|p| p.to_path_buf()) {
                    self.fp_path    = parent;
                    self.fp_selected = 0;
                    self.load_fp_entries();
                }
            }

            // ── Add current directory ────────────────────────────────────────
            KeyCode::Char('a') => {
                self.fp_add_current_dir(terminal).await;
            }

            // ── Delete selected extra_dir (if in list) ───────────────────────
            KeyCode::Char('d') | KeyCode::Delete => {
                let path_str = self.fp_path.to_string_lossy().into_owned();
                let already  = self.config.paths.extra_dirs.contains(&path_str);
                if already {
                    self.fp_remove_dir(&path_str, terminal).await;
                }
            }

            // ── Close ────────────────────────────────────────────────────────
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = AppMode::Browse;
            }

            _ => {}
        }
    }

    async fn fp_add_current_dir(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        let path_str = self.fp_path.to_string_lossy().into_owned();
        if fp_is_system(&self.fp_path) {
            self.set_status_error("System path — cannot add");
            return;
        }
        if self.config.paths.extra_dirs.contains(&path_str) {
            self.set_status_error("Folder already added");
            return;
        }

        self.config.paths.extra_dirs.push(path_str.clone());
        if let Err(e) = self.config.save() {
            self.config.paths.extra_dirs.retain(|d| d != &path_str);
            self.set_status_error(format!("Save failed: {}", e));
            return;
        }

        self.set_status_ok("\u{2713} Folder added");
        self.mode = AppMode::Browse;
        self.refresh_list(terminal).await;
    }

    async fn fp_remove_dir(
        &mut self,
        path_str: &str,
        terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    ) {
        self.config.paths.extra_dirs.retain(|d| d != path_str);
        if let Err(e) = self.config.save() {
            self.set_status_error(format!("Save failed: {}", e));
            return;
        }
        self.set_status_ok("\u{2713} Folder removed");
        self.refresh_list(terminal).await;
    }

    /// List of currently configured extra_dirs.
    pub fn extra_dirs(&self) -> &[String] {
        &self.config.paths.extra_dirs
    }
}

pub fn fit_label(fit: FitMode) -> &'static str {
    match fit {
        FitMode::Fit     => "letterbox",
        FitMode::Fill    => "fill",
        FitMode::Stretch => "stretch",
        FitMode::Center  => "center",
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wpick_core::config::WpickConfig;
    use wpick_core::model::{WallpaperInfo, WallpaperType};

    fn make_app() -> App {
        let config = WpickConfig::default();
        let dirs = config.app_dirs().unwrap();
        App {
            config,
            dirs,
            client:                 None,
            wallpapers:             Vec::new(),
            selected:               0,
            list_state:             ListState::default(),
            current_wallpaper_id:   None,
            mode:                   AppMode::Browse,
            status_message:         None,
            status_is_error:        false,
            status_clear_at:        None,
            daemon_connected:       false,
            loading:                false,
            should_quit:            false,
            last_reconnect_attempt: None,
            search_query:           String::new(),
            search_active:          false,
            filter_type:            FilterType::All,
            current_fit:            FitMode::default(),
            picker:                 None,
            preview:                None,
            preview_id:             None,
            monitors:               Vec::new(),
            monitor_select_mode:    false,
            monitor_selected:       0,
            fp_path:                std::path::PathBuf::from("/tmp"),
            fp_entries:             Vec::new(),
            fp_selected:            0,
            fp_hints:               std::collections::HashMap::new(),
        }
    }

    fn wallpaper(id: u64, preview_path: Option<&str>) -> WallpaperInfo {
        WallpaperInfo {
            id,
            title:           format!("Test {id}"),
            wallpaper_type:  WallpaperType::Video,
            file_path:       format!("/tmp/test_{id}.mp4"),
            preview_path:    preview_path.map(String::from),
            has_audio:       false,
            file_size_bytes: 1024,
            width:           0,
            height:          0,
            source:          wpick_core::model::WallpaperSource::Workshop,
        }
    }

    #[test]
    fn test_update_preview_empty_list_clears_state() {
        let mut app = make_app();
        app.update_preview();
        assert!(app.preview.is_none());
        assert!(app.preview_id.is_none());
    }

    #[test]
    fn test_update_preview_no_preview_path_sets_id_only() {
        let mut app = make_app();
        app.picker = Some(Picker::halfblocks());
        app.wallpapers = vec![wallpaper(42, None)];
        app.update_preview();
        assert!(app.preview.is_none());
        assert_eq!(app.preview_id, Some(42));
    }

    #[test]
    fn test_update_preview_missing_file_leaves_preview_none() {
        let mut app = make_app();
        app.picker = Some(Picker::halfblocks());
        app.wallpapers = vec![wallpaper(1, Some("/tmp/wpick_nonexistent_9876543.jpg"))];
        app.update_preview();
        assert!(app.preview.is_none());
        assert_eq!(app.preview_id, Some(1));
    }

    #[test]
    fn test_update_preview_same_id_skips_reload() {
        let mut app = make_app();
        app.picker = Some(Picker::halfblocks());
        app.wallpapers = vec![wallpaper(7, Some("/tmp/wpick_nonexistent_9876543.jpg"))];
        app.update_preview();
        assert_eq!(app.preview_id, Some(7));
        // Overwrite preview_id sentinel — second call must not clear it
        app.preview_id = Some(7);
        app.update_preview();
        assert_eq!(app.preview_id, Some(7)); // unchanged
    }

    #[test]
    fn test_update_preview_no_picker_sets_id_only() {
        let mut app = make_app();
        // picker is None, but we still record preview_id so we don't loop
        app.wallpapers = vec![wallpaper(5, Some("/tmp/some_preview.jpg"))];
        app.update_preview();
        assert!(app.preview.is_none());
        // preview_id is NOT set when picker is absent — update_preview returns early
        assert!(app.preview_id.is_none());
    }

    #[test]
    fn test_update_preview_with_real_image() {
        use image::{ImageBuffer, Rgb};
        let img: image::DynamicImage =
            ImageBuffer::from_fn(4, 4, |_, _| Rgb([200u8, 100, 50])).into();
        let tmp = std::env::temp_dir().join("wpick_test_preview_real.png");
        img.save(&tmp).unwrap();

        let mut app = make_app();
        app.picker = Some(Picker::halfblocks());
        app.wallpapers = vec![wallpaper(99, Some(tmp.to_str().unwrap()))];
        app.update_preview();

        assert!(app.preview.is_some(), "expected preview to be loaded");
        assert_eq!(app.preview_id, Some(99));

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_select_next_updates_preview_id() {
        let mut app = make_app();
        app.picker = Some(Picker::halfblocks());
        app.wallpapers = vec![
            wallpaper(1, None),
            wallpaper(2, None),
        ];
        app.update_preview();
        assert_eq!(app.preview_id, Some(1));

        app.select_next();
        assert_eq!(app.preview_id, Some(2));
    }

    #[test]
    fn test_select_prev_updates_preview_id() {
        let mut app = make_app();
        app.picker = Some(Picker::halfblocks());
        app.wallpapers = vec![wallpaper(10, None), wallpaper(20, None)];
        app.selected = 1;
        app.update_preview();
        assert_eq!(app.preview_id, Some(20));

        app.select_prev();
        assert_eq!(app.preview_id, Some(10));
    }
}