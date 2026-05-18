use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use std::collections::HashSet;
use std::io::Stdout;
use std::time::Duration;
use wpick_core::config::{AppDirs, FitMode, Pack, WpickConfig};
use wpick_core::ipc::{ClientCommand, DaemonResponse};
use wpick_core::model::{WallpaperInfo, WallpaperSource};

use crate::client::IpcClient;
use crate::ui;

// ── Theme ─────────────────────────────────────────────────────────────────────

/// Named color palette — each entry is (name, (r, g, b)).
/// Index values here correspond to TuiColorConfig fields.
pub const PALETTE: &[(&str, (u8, u8, u8))] = &[
    // Neutrals (0-7)
    ("Black",         (15,  15,  20)),
    ("Dark Gray",     (48,  48,  58)),
    ("Mid Gray",      (80,  80,  95)),
    ("Light Gray",    (140, 140, 155)),
    ("Near White",    (200, 200, 215)),
    ("White",         (230, 230, 240)),
    ("Warm Gray",     (90,  88,  82)),
    ("Cool Gray",     (72,  78,  95)),
    // Blues (8-15)
    ("Dark Blue",     (28,  40,  72)),
    ("Navy",          (38,  52,  100)),
    ("Dim Blue",      (58,  72,  115)),
    ("Steel Blue",    (85,  108, 148)),
    ("Soft Blue",     (88,  128, 168)),
    ("Sky Blue",      (100, 158, 210)),
    ("Ice Blue",      (140, 195, 225)),
    ("Cyan",          (78,  175, 175)),
    // Greens & teals (16-20)
    ("Teal",          (52,  128, 128)),
    ("Dim Green",     (62,  112, 72)),
    ("Sage",          (88,  145, 100)),
    ("Green",         (92,  172, 98)),
    ("Lime",          (140, 195, 80)),
    // Warm colors (21-29)
    ("Yellow",        (195, 175, 72)),
    ("Gold",          (210, 165, 50)),
    ("Orange",        (200, 132, 65)),
    ("Rust",          (172, 78,  55)),
    ("Red",           (172, 72,  72)),
    ("Crimson",       (145, 48,  65)),
    ("Pink",          (175, 92,  132)),
    ("Rose",          (210, 128, 148)),
    ("Warm White",    (218, 198, 172)),
    // Purples (30-35)
    ("Purple",        (112, 72,  172)),
    ("Violet",        (88,  68,  148)),
    ("Lavender",      (148, 125, 195)),
    ("Indigo",        (68,  58,  148)),
    ("Mauve",         (138, 98,  145)),
    ("Plum",          (95,  62,  112)),
];

/// Named presets from real palettes.
/// Slots: [border_active, border_idle, border_overlay, sel_bg, sel_fg,
///         color_hint, color_playing, color_fav, col_title, text_dim, vol_bar]
pub const COLOR_PRESETS: &[(&str, [usize; 11])] = &[
    //          act  idle  ovly  sel   sfg   hint  play  fav   title dim   vol
    // Nord — arctic, calm, frost blue (#88c0d0) on polar night (#2e3440)
    ("nord",    [11, 8,  13, 9,  4,  10, 18, 21, 13, 7,  11]),
    // Dracula — deep purple (#bd93f9), pink (#ff79c6) on #282a36
    ("dracula", [32, 7,  34, 31, 4,  27, 19, 22, 32, 10, 32]),
    // Tokyo Night — electric blue (#7aa2f7), purple (#bb9af7) on #1a1b26
    ("tokyo",   [13, 8,  32, 33, 4,  10, 20, 22, 14, 10, 13]),
    // Forrest — deep forest, leaf green on dark earth
    ("forrest", [19, 1,  18, 17, 4,  16, 20, 22, 18, 6,  19]),
    // Deep — near-black void, cold steel blue, almost no chrome
    ("deep",    [11, 0,  10, 8,  3,  10, 13, 31, 11, 1,  11]),
];

pub fn palette_to_color(idx: usize) -> ratatui::style::Color {
    let &(_, (r, g, b)) = &PALETTE[idx.min(PALETTE.len() - 1)];
    ratatui::style::Color::Rgb(r, g, b)
}

/// Colors used by the current visual theme preset.
pub struct ThemeColors {
    pub border_active:   ratatui::style::Color,
    pub border_idle:     ratatui::style::Color,
    pub border_overlay:  ratatui::style::Color,
    pub sel_bg:          ratatui::style::Color,
    pub sel_fg:          ratatui::style::Color,
    pub color_hint:      ratatui::style::Color,
    pub color_playing:   ratatui::style::Color,
    pub color_fav:       ratatui::style::Color,
    pub color_col_title: ratatui::style::Color,
    pub text_dim:        ratatui::style::Color,
    pub vol_bar:         ratatui::style::Color,
}

// ── Enums ─────────────────────────────────────────────────────────────────────

/// Which panel has keyboard focus.
#[derive(Debug, Clone, PartialEq)]
pub enum Panel {
    Nav,
    List,
}

/// An item in the left navigation panel.
#[derive(Debug, Clone, PartialEq)]
pub enum NavItem {
    Favorites,
    Frequent,
    Pack(usize),        // index into App::packs
    Source(SourceFilter),
}

#[derive(Debug, Clone, PartialEq)]
pub enum SourceFilter {
    All,
    Workshop,
    Local(String),
}

/// Overall UI mode.
#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    Browse,
    Detail,
    FolderPicker,
    TimerDialog,
    PackNameInput,
    Settings,
    Help,
    PackPicker,
    SortDialog,
}

/// Shallow scan hint for a directory entry in the folder picker.
#[derive(Debug, Clone, PartialEq)]
pub enum FpHint {
    HasVideos,
    HasSubdirs,
    Empty,
    Unreadable,
    System,
}

/// Sort field for the wallpaper list.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum SortMode {
    #[default]
    Default,
    Name,
    Size,
    Resolution,
}

// ── App state ─────────────────────────────────────────────────────────────────

pub struct App {
    pub config:               WpickConfig,
    pub dirs:                 AppDirs,
    pub client:               Option<IpcClient>,
    pub wallpapers:           Vec<WallpaperInfo>,
    pub frequent:             Vec<WallpaperInfo>,  // top-10 by play count
    pub selected:             usize,               // index within current_view()
    pub list_state:           ListState,
    pub current_wallpaper_id: Option<u64>,
    pub mode:                 AppMode,
    pub status_message:       Option<String>,
    pub status_is_error:      bool,
    pub status_clear_at:      Option<std::time::Instant>,
    pub daemon_connected:     bool,
    pub loading:              bool,
    pub should_quit:          bool,
    pub last_reconnect:       Option<std::time::Instant>,
    pub last_status_sync:     Option<std::time::Instant>,
    pub search_query:         String,
    pub search_active:        bool,
    // Image preview
    pub picker:               Option<Picker>,
    pub preview:              Option<StatefulProtocol>,
    pub preview_id:           Option<u64>,
    pub current_fit:          FitMode,
    // Monitor selector
    pub monitors:             Vec<(String, u32, u32)>,
    pub monitor_select_mode:  bool,
    pub monitor_selected:     usize,
    // ── Spotify layout ────────────────────────────────────────────────────────
    pub active_panel:         Panel,
    pub nav_selected:         usize,           // selected row in nav panel
    pub show_nav:             bool,
    pub show_preview:         bool,
    // ── Favorites & packs ────────────────────────────────────────────────────
    pub favorites:            HashSet<u64>,
    pub packs:                Vec<Pack>,
    // ── App lifecycle ─────────────────────────────────────────────────────────
    pub start_time:           std::time::Instant,
    pub wallpaper_applied_at: Option<std::time::Instant>,
    // ── Timer (local TUI state) ───────────────────────────────────────────────
    pub timer_active:         bool,
    pub timer_interval_secs:  u64,
    pub timer_remaining_secs: u64,
    pub timer_last_tick:      Option<std::time::Instant>,
    pub timer_ids:            Vec<u64>,         // wallpaper IDs in current rotation
    // Timer dialog state
    pub timer_dialog_idx:     usize,           // selected interval option
    // Pack name input
    pub pack_name_buf:        String,
    // Settings dialog state — 2-level navigation (categories → items)
    pub settings_level:       u8,    // 0 = category list, 1 = inside category
    pub settings_cat:         usize, // selected category index
    pub settings_selected:    usize, // selected item within a category
    // ── Folder picker ─────────────────────────────────────────────────────────
    pub fp_path:              std::path::PathBuf,
    pub fp_entries:           Vec<String>,
    pub fp_selected:          usize,
    pub fp_hints:             std::collections::HashMap<std::path::PathBuf, FpHint>,
    // ── Pack picker ───────────────────────────────────────────────────────────
    pub pack_picker_sel:      usize,
    pub pack_picker_pack:     usize,
    // ── Sort ─────────────────────────────────────────────────────────────────
    pub sort_mode:            SortMode,
    pub sort_desc:            bool,
    pub sort_dialog_sel:      usize,
}

/// Timer interval options shown in the dialog (label, seconds).
pub const TIMER_OPTIONS: &[(&str, u64)] = &[
    ("5 minutes",  5 * 60),
    ("15 minutes", 15 * 60),
    ("30 minutes", 30 * 60),
    ("1 hour",     3600),
    ("2 hours",    7200),
];

/// Sort dialog options: (label, SortMode variant).
pub const SORT_OPTIONS: &[(&str, SortMode)] = &[
    ("Default (DB order)", SortMode::Default),
    ("Name",               SortMode::Name),
    ("Size",               SortMode::Size),
    ("Resolution",         SortMode::Resolution),
];

impl App {
    pub fn new(config: WpickConfig, dirs: AppDirs, picker: Picker) -> Self {
        let favorites: HashSet<u64> = config.tui.favorites.iter().copied().collect();
        let packs     = config.tui.packs.clone();
        let show_nav  = config.tui.show_nav;
        let show_prev = config.tui.show_preview;
        Self {
            config,
            dirs,
            client:               None,
            wallpapers:           Vec::new(),
            frequent:             Vec::new(),
            selected:             0,
            list_state:           ListState::default(),
            current_wallpaper_id: None,
            mode:                 AppMode::Browse,
            status_message:       None,
            status_is_error:      false,
            status_clear_at:      None,
            daemon_connected:     false,
            loading:              false,
            should_quit:          false,
            last_reconnect:       None,
            last_status_sync:     None,
            search_query:         String::new(),
            search_active:        false,
            picker:               Some(picker),
            preview:              None,
            preview_id:           None,
            current_fit:          FitMode::default(),
            monitors:             Vec::new(),
            monitor_select_mode:  false,
            monitor_selected:     0,
            active_panel:         Panel::List,
            nav_selected:         0,
            show_nav,
            show_preview:         show_prev,
            favorites,
            packs,
            start_time:           std::time::Instant::now(),
            wallpaper_applied_at: None,
            timer_active:         false,
            timer_interval_secs:  0,
            timer_remaining_secs: 0,
            timer_last_tick:      None,
            timer_ids:            Vec::new(),
            timer_dialog_idx:     2, // default 30 min
            pack_name_buf:        String::new(),
            settings_level:       0,
            settings_cat:         0,
            settings_selected:    0,
            fp_path:              std::env::var("HOME")
                                      .map(std::path::PathBuf::from)
                                      .unwrap_or_else(|_| std::path::PathBuf::from("/")),
            fp_entries:           Vec::new(),
            fp_selected:          0,
            fp_hints:             std::collections::HashMap::new(),
            pack_picker_sel:      0,
            pack_picker_pack:     0,
            sort_mode:            SortMode::Default,
            sort_desc:            false,
            sort_dialog_sel:      0,
        }
    }

    // ── Theme ─────────────────────────────────────────────────────────────────

    /// Return the active theme's color set, driven by per-slot palette indices.
    pub fn theme(&self) -> ThemeColors {
        let c = &self.config.tui.colors;
        ThemeColors {
            border_active:   palette_to_color(c.border_active),
            border_idle:     palette_to_color(c.border_idle),
            border_overlay:  palette_to_color(c.border_overlay),
            sel_bg:          palette_to_color(c.sel_bg),
            sel_fg:          palette_to_color(c.sel_fg),
            color_hint:      palette_to_color(c.color_hint),
            color_playing:   palette_to_color(c.color_playing),
            color_fav:       palette_to_color(c.color_fav),
            color_col_title: palette_to_color(c.col_title),
            text_dim:        palette_to_color(c.text_dim),
            vol_bar:         palette_to_color(c.vol_bar),
        }
    }

    // ── Nav items ─────────────────────────────────────────────────────────────

    /// Build the ordered list of navigation items based on current library.
    /// Order: All first (default), then Favorites, Frequent, Packs, sources.
    pub fn nav_items(&self) -> Vec<NavItem> {
        let mut items = vec![
            NavItem::Source(SourceFilter::All),
            NavItem::Favorites,
            NavItem::Frequent,
        ];
        for (i, _) in self.packs.iter().enumerate() {
            items.push(NavItem::Pack(i));
        }
        if self.wallpapers.iter().any(|w| w.source == WallpaperSource::Workshop) {
            items.push(NavItem::Source(SourceFilter::Workshop));
        }
        let local_labels: Vec<String> = self.wallpapers.iter()
            .filter_map(|w| match &w.source {
                WallpaperSource::Local { label } => Some(label.clone()),
                _ => None,
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        for label in local_labels {
            items.push(NavItem::Source(SourceFilter::Local(label)));
        }
        items
    }

    /// The currently selected nav item.
    pub fn current_nav(&self) -> NavItem {
        let items = self.nav_items();
        items.into_iter().nth(self.nav_selected).unwrap_or(NavItem::Source(SourceFilter::All))
    }

    // ── Filtered view ─────────────────────────────────────────────────────────

    /// Wallpapers to show in the list panel, filtered by nav + search + sort.
    pub fn current_view(&self) -> Vec<&WallpaperInfo> {
        let base: Vec<&WallpaperInfo> = match self.current_nav() {
            NavItem::Favorites => self.wallpapers.iter()
                .filter(|w| self.favorites.contains(&w.id))
                .collect(),
            NavItem::Frequent => self.frequent.iter().collect(),
            NavItem::Pack(i) => {
                let ids = self.packs.get(i).map(|p| &p.ids[..]).unwrap_or(&[]);
                self.wallpapers.iter()
                    .filter(|w| ids.contains(&w.id))
                    .collect()
            }
            NavItem::Source(SourceFilter::All) => self.wallpapers.iter().collect(),
            NavItem::Source(SourceFilter::Workshop) => self.wallpapers.iter()
                .filter(|w| w.source == WallpaperSource::Workshop)
                .collect(),
            NavItem::Source(SourceFilter::Local(label)) => self.wallpapers.iter()
                .filter(|w| matches!(&w.source, WallpaperSource::Local { label: l } if l == &label))
                .collect(),
        };

        let mut view: Vec<&WallpaperInfo> = if self.search_query.is_empty() {
            base
        } else {
            let q = self.search_query.to_lowercase();
            base.into_iter().filter(|w| w.title.to_lowercase().contains(&q)).collect()
        };

        match self.sort_mode {
            SortMode::Default => {}
            SortMode::Name => view.sort_by(|a, b| {
                let ord = a.title.to_lowercase().cmp(&b.title.to_lowercase());
                if self.sort_desc { ord.reverse() } else { ord }
            }),
            SortMode::Size => view.sort_by(|a, b| {
                let ord = a.file_size_bytes.cmp(&b.file_size_bytes);
                if self.sort_desc { ord.reverse() } else { ord }
            }),
            SortMode::Resolution => view.sort_by(|a, b| {
                let a_px = a.width as u64 * a.height as u64;
                let b_px = b.width as u64 * b.height as u64;
                let ord = a_px.cmp(&b_px);
                if self.sort_desc { ord.reverse() } else { ord }
            }),
        }

        view
    }

    // ── Preview ───────────────────────────────────────────────────────────────

    pub fn update_preview(&mut self) {
        if self.picker.is_none() {
            self.preview = None;
            return;
        }
        let (id, preview_path) = {
            let view = self.current_view();
            match view.get(self.selected) {
                Some(w) => (w.id, w.preview_path.clone()),
                None => {
                    self.preview    = None;
                    self.preview_id = None;
                    return;
                }
            }
        };
        if self.preview_id == Some(id) { return; }
        self.preview    = None;
        self.preview_id = Some(id);
        let path = match preview_path { Some(p) => p, None => return };
        if let Ok(img) = image::open(&path) {
            // Upscale tiny preview images so they fill the panel.
            // Steam Workshop previews can be as small as 100×100.
            let img = upscale_preview(img);
            self.preview = Some(self.picker.as_ref().unwrap().new_resize_protocol(img));
        }
    }

    // ── Status ────────────────────────────────────────────────────────────────

    pub fn set_status_ok(&mut self, msg: impl Into<String>) {
        self.status_message  = Some(msg.into());
        self.status_is_error = false;
        self.status_clear_at = Some(std::time::Instant::now() + Duration::from_secs(3));
    }

    pub fn set_status_error(&mut self, msg: impl Into<String>) {
        self.status_message  = Some(msg.into());
        self.status_is_error = true;
        self.status_clear_at = Some(std::time::Instant::now() + Duration::from_secs(3));
    }

    // ── Navigation ────────────────────────────────────────────────────────────

    fn select_next(&mut self) {
        let len = self.current_view().len();
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

    fn reset_list_selection(&mut self) {
        self.selected = 0;
        let empty = self.current_view().is_empty();
        self.list_state.select(if empty { None } else { Some(0) });
        self.preview_id = None;
        self.update_preview();
    }

    // ── Main loop ─────────────────────────────────────────────────────────────

    pub async fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        self.try_reconnect(terminal).await;

        loop {
            if self.client.is_none() {
                self.try_reconnect(terminal).await;
            }

            // Clear expired status messages.
            if let Some(t) = self.status_clear_at {
                if std::time::Instant::now() >= t {
                    self.status_message  = None;
                    self.status_clear_at = None;
                }
            }

            // Tick timer countdown.
            if self.timer_active {
                if let Some(tick) = self.timer_last_tick {
                    let elapsed = tick.elapsed().as_secs();
                    self.timer_remaining_secs = self.timer_interval_secs
                        .saturating_sub(elapsed % self.timer_interval_secs.max(1));
                }
                // Sync current_wallpaper_id from daemon every 2 s so the rotation
                // list reflects wallpaper changes made by the timer task.
                let needs_sync = self.last_status_sync
                    .map(|t| t.elapsed() >= Duration::from_secs(2))
                    .unwrap_or(true);
                if needs_sync {
                    self.last_status_sync = Some(std::time::Instant::now());
                    if let Ok(DaemonResponse::VolumeState { current_id, .. }) =
                        self.send(ClientCommand::Status).await
                    {
                        if current_id.is_some() { self.current_wallpaper_id = current_id; }
                    }
                }
            }

            terminal.draw(|f| ui::render(f, self))?;

            if crossterm::event::poll(Duration::from_millis(250))? {
                match crossterm::event::read()? {
                    Event::Key(key) => {
                        self.handle_key(key, terminal).await;
                    }
                    Event::Mouse(mouse) => {
                        match mouse.kind {
                            MouseEventKind::ScrollDown => {
                                if self.active_panel == Panel::List {
                                    self.select_next();
                                } else {
                                    let nav_len = self.nav_items().len();
                                    if self.nav_selected + 1 < nav_len {
                                        self.nav_selected += 1;
                                        self.reset_list_selection();
                                    }
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                if self.active_panel == Panel::List {
                                    self.select_prev();
                                } else if self.nav_selected > 0 {
                                    self.nav_selected -= 1;
                                    self.reset_list_selection();
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            if self.should_quit {
                // Final flush: ensures packs/favorites are on disk even if an
                // earlier save silently failed (e.g. temporary permissions issue).
                self.save_tui_config();
                break;
            }
        }
        Ok(())
    }

    // ── Key handling ──────────────────────────────────────────────────────────

    async fn handle_key(&mut self, key: KeyEvent, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        // Ctrl-C / Ctrl-Q always quit.
        if (key.code == KeyCode::Char('c') || key.code == KeyCode::Char('q'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.cmd_kill().await;
            self.should_quit = true;
            return;
        }

        match &self.mode {
            AppMode::FolderPicker => {
                self.handle_key_folder_picker(key, terminal).await;
                return;
            }
            AppMode::TimerDialog => {
                self.handle_key_timer_dialog(key, terminal).await;
                return;
            }
            AppMode::PackNameInput => {
                self.handle_key_pack_name(key, terminal).await;
                return;
            }
            AppMode::Settings => {
                self.handle_key_settings(key);
                return;
            }
            AppMode::Help => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')) {
                    self.mode = AppMode::Browse;
                }
                return;
            }
            AppMode::PackPicker => {
                self.handle_key_pack_picker(key);
                return;
            }
            AppMode::SortDialog => {
                self.handle_key_sort_dialog(key);
                return;
            }
            _ => {}
        }

        if self.monitor_select_mode {
            self.handle_key_monitor(key).await;
            return;
        }

        if self.search_active {
            self.handle_key_search(key);
            return;
        }

        // Panel-specific bindings.
        match self.active_panel {
            Panel::Nav  => self.handle_key_nav(key, terminal).await,
            Panel::List => self.handle_key_list(key, terminal).await,
        }
    }

    async fn handle_key_monitor(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => { self.monitor_select_mode = false; }
            KeyCode::Up | KeyCode::Char('k') if self.monitor_selected > 0 => {
                self.monitor_selected -= 1;
            }
            KeyCode::Down | KeyCode::Char('j')
                if self.monitor_selected < self.monitors.len() =>
            {
                self.monitor_selected += 1;
            }
            KeyCode::Enter => {
                self.monitor_select_mode = false;
                self.cmd_set_to_selected_monitor().await;
            }
            _ => {}
        }
    }

    fn handle_key_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Enter => { self.search_active = false; }
            KeyCode::Backspace if self.search_query.pop().is_some() => {
                self.reset_list_selection();
            }
            KeyCode::Char(c) => {
                self.search_query.push(c);
                self.reset_list_selection();
            }
            _ => {}
        }
    }

    async fn handle_key_nav(&mut self, key: KeyEvent, _terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        let nav_len = self.nav_items().len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => { self.should_quit = true; }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.cmd_kill().await;
                self.should_quit = true;
            }
            KeyCode::Down | KeyCode::Char('j') if self.nav_selected + 1 < nav_len => {
                self.nav_selected += 1;
                self.reset_list_selection();
            }
            KeyCode::Up | KeyCode::Char('k') if self.nav_selected > 0 => {
                self.nav_selected -= 1;
                self.reset_list_selection();
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.active_panel = Panel::List;
            }
            KeyCode::Tab => { self.active_panel = Panel::List; }
            KeyCode::Char('[') => { self.show_nav = !self.show_nav; self.save_tui_config(); }
            KeyCode::Char(']') => { self.show_preview = !self.show_preview; self.save_tui_config(); }
            KeyCode::Char('n') => { self.open_pack_name_dialog(); }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let NavItem::Pack(i) = self.current_nav() {
                    let name = self.packs[i].name.clone();
                    self.packs.remove(i);
                    self.nav_selected = self.nav_selected.min(self.nav_items().len().saturating_sub(1));
                    self.save_tui_config();
                    self.set_status_ok(format!("Pack '{}' deleted", name));
                }
            }
            KeyCode::Char('S') | KeyCode::Char('e') => { self.mode = AppMode::Settings; self.settings_level = 0; self.settings_cat = 0; self.settings_selected = 0; }
            _ => {}
        }
    }

    async fn handle_key_list(&mut self, key: KeyEvent, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => { self.should_quit = true; }
            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::ALT) => {
                self.cmd_kill().await;
                self.should_quit = true;
            }
            KeyCode::Down | KeyCode::Char('j') => { self.select_next(); }
            KeyCode::Up   | KeyCode::Char('k') => { self.select_prev(); }
            KeyCode::Left | KeyCode::Char('h') if self.show_nav => {
                self.active_panel = Panel::Nav;
            }
            KeyCode::Tab if self.show_nav => { self.active_panel = Panel::Nav; }
            KeyCode::Enter => { self.cmd_set().await; }
            KeyCode::Char('M') => {
                if !self.monitors.is_empty() {
                    self.monitor_select_mode = true;
                    self.monitor_selected    = 0;
                } else {
                    self.set_status_error("No monitors reported (try 'r')");
                }
            }
            KeyCode::Char('p') => { self.cmd_toggle_favorite().await; }
            KeyCode::Char('a') => {
                if matches!(self.current_nav(), NavItem::Pack(_)) {
                    self.open_pack_picker();
                } else {
                    self.cmd_add_to_pack();
                }
            }
            KeyCode::Char('t') => { self.open_timer_dialog(); }
            KeyCode::Char('+') | KeyCode::Char('=') => { self.cmd_change_volume( 0.05).await; }
            KeyCode::Char('-')                       => { self.cmd_change_volume(-0.05).await; }
            KeyCode::Char('m') => { self.cmd_mute().await; }
            KeyCode::Char('r') => {
                self.refresh_monitors().await;
                self.refresh_list(terminal).await;
            }
            KeyCode::Char('s') => { self.open_folder_picker(); }
            KeyCode::Char('f') => { self.cmd_cycle_fit().await; }
            KeyCode::Char('i') => {
                self.mode = match self.mode {
                    AppMode::Detail => AppMode::Browse,
                    _               => AppMode::Detail,
                };
            }
            KeyCode::Char('/') => {
                self.search_active = true;
                self.search_query  = String::new();
                self.reset_list_selection();
            }
            KeyCode::Char('[') => { self.show_nav = !self.show_nav; self.save_tui_config(); }
            KeyCode::Char(']') => { self.show_preview = !self.show_preview; self.save_tui_config(); }
            KeyCode::Char('n') => { self.open_pack_name_dialog(); }
            KeyCode::Char('o') => {
                self.sort_dialog_sel = SORT_OPTIONS.iter().position(|(_, m)| *m == self.sort_mode).unwrap_or(0);
                self.mode = AppMode::SortDialog;
            }
            KeyCode::Char('w') => {
                self.config.tui.windowed = !self.config.tui.windowed;
                self.save_tui_config();
            }
            KeyCode::Char('S') | KeyCode::Char('e') => { self.mode = AppMode::Settings; self.settings_level = 0; self.settings_cat = 0; self.settings_selected = 0; }
            KeyCode::Char('?') => { self.mode = AppMode::Help; }
            _ => {}
        }
    }

    // ── Settings dialog ───────────────────────────────────────────────────────

    /// Category names shown at the top level.
    pub const SETTINGS_CATS: &'static [&'static str] = &["TUI", "Visual", "Keys"];

    /// Items per category: (label, section_header).
    pub fn settings_cat_items(cat: usize) -> &'static [(&'static str, bool)] {
        match cat {
            0 => &[
                ("Nav panel (Library column)",   false),
                ("Preview column",               false),
                ("Now-playing: title position",  false),
                ("Volume bar style",             false),
            ],
            // Visual presets are derived from COLOR_PRESETS at render time.
            1 => &[],
            2 => &[
                ("Keys work only inside the wpick TUI window", true),
                ("Apply wallpaper    Enter",     false),
                ("Toggle favorite    p",         false),
                ("Add to pack        a",         false),
                ("Open timer         t",         false),
                ("Search             /",         false),
                ("Fit mode           f",         false),
                ("Folders            s",         false),
                ("Detail view        i",         false),
                ("Nav panel toggle   [",         false),
                ("Preview toggle     ]",         false),
                ("Volume +/-         +/-",       false),
                ("Mute               m",         false),
                ("Refresh/Scan       r",         false),
                ("Settings           S",         false),
                ("Help               ?",         false),
                ("Quit (daemon on)   q",         false),
                ("Kill daemon        Q",         false),
            ],
            _ => &[],
        }
    }

    fn handle_key_settings(&mut self, key: KeyEvent) {
        if self.settings_level == 0 {
            // Category list navigation
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('S') => {
                    self.mode = AppMode::Browse;
                }
                KeyCode::Up | KeyCode::Char('k') if self.settings_cat > 0 => {
                    self.settings_cat -= 1;
                }
                KeyCode::Down | KeyCode::Char('j')
                    if self.settings_cat + 1 < Self::SETTINGS_CATS.len() =>
                {
                    self.settings_cat += 1;
                }
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    self.settings_level    = 1;
                    self.settings_selected = 0;
                }
                _ => {}
            }
        } else {
            // Inside a category
            let items = Self::settings_cat_items(self.settings_cat);
            let toggleable: Vec<usize> = items.iter().enumerate()
                .filter(|(_, (_, hdr))| !hdr)
                .map(|(i, _)| i)
                .collect();
            match key.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('q') => {
                    self.settings_level = 0;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.settings_cat == 1 {
                        if self.settings_selected > 0 { self.settings_selected -= 1; }
                    } else {
                        let pos = toggleable.iter().position(|&i| i == self.settings_selected);
                        if let Some(p) = pos {
                            if p > 0 { self.settings_selected = toggleable[p - 1]; }
                        } else if !toggleable.is_empty() {
                            self.settings_selected = *toggleable.last().unwrap();
                        }
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.settings_cat == 1 {
                        if self.settings_selected + 1 < COLOR_PRESETS.len() {
                            self.settings_selected += 1;
                        }
                    } else {
                        let pos = toggleable.iter().position(|&i| i == self.settings_selected);
                        if let Some(p) = pos {
                            if p + 1 < toggleable.len() { self.settings_selected = toggleable[p + 1]; }
                        } else if !toggleable.is_empty() {
                            self.settings_selected = toggleable[0];
                        }
                    }
                }
                KeyCode::Enter | KeyCode::Char(' ') if self.settings_cat == 0 => {
                    match self.settings_selected {
                        0 => { self.show_nav     = !self.show_nav;     self.save_tui_config(); }
                        1 => { self.show_preview = !self.show_preview; self.save_tui_config(); }
                        2 => {
                            // Cycle now_playing_pos
                            let next = match self.config.tui.now_playing_pos.as_str() {
                                "top-right"  => "top-center",
                                "top-center" => "top-left",
                                "top-left"   => "none",
                                _            => "top-right",
                            };
                            self.config.tui.now_playing_pos = next.to_owned();
                            self.save_tui_config();
                        }
                        3 => {
                            // Cycle vol_style
                            let next = match self.config.tui.vol_style.as_str() {
                                "slim"   => "bar",
                                "bar"    => "number",
                                _        => "slim",
                            };
                            self.config.tui.vol_style = next.to_owned();
                            self.save_tui_config();
                        }
                        _ => {}
                    }
                }
                KeyCode::Enter | KeyCode::Char(' ') if self.settings_cat == 1 => {
                    if let Some(&(_, indices)) = COLOR_PRESETS.get(self.settings_selected) {
                        let c = &mut self.config.tui.colors;
                        c.border_active  = indices[0];
                        c.border_idle    = indices[1];
                        c.border_overlay = indices[2];
                        c.sel_bg         = indices[3];
                        c.sel_fg         = indices[4];
                        c.color_hint     = indices[5];
                        c.color_playing  = indices[6];
                        c.color_fav      = indices[7];
                        c.col_title      = indices[8];
                        c.text_dim       = indices[9];
                        c.vol_bar        = indices[10];
                        self.save_tui_config();
                    }
                }
                _ => {}
            }
            // Initialise selection to first non-header on entry
            if !toggleable.is_empty() && items.get(self.settings_selected).map(|(_, h)| *h).unwrap_or(false) {
                self.settings_selected = toggleable[0];
            }
        }
    }

    // ── Timer dialog ──────────────────────────────────────────────────────────

    pub fn open_timer_dialog(&mut self) {
        self.mode = AppMode::TimerDialog;
    }

    async fn handle_key_timer_dialog(&mut self, key: KeyEvent, _terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = AppMode::Browse;
            }
            KeyCode::Up | KeyCode::Char('k') if self.timer_dialog_idx > 0 => {
                self.timer_dialog_idx -= 1;
            }
            KeyCode::Down | KeyCode::Char('j')
                if self.timer_dialog_idx + 1 < TIMER_OPTIONS.len() =>
            {
                self.timer_dialog_idx += 1;
            }
            // Stop timer
            KeyCode::Char('s') => {
                self.cmd_stop_timer().await;
                self.mode = AppMode::Browse;
            }
            KeyCode::Enter => {
                self.cmd_start_timer().await;
                self.mode = AppMode::Browse;
            }
            _ => {}
        }
    }

    async fn cmd_start_timer(&mut self) {
        let interval_secs = TIMER_OPTIONS[self.timer_dialog_idx].1;
        // Build ID list from current nav view (or all wallpapers if < 2 visible).
        let ids: Vec<u64> = {
            let view = self.current_view();
            if view.len() >= 2 {
                view.iter().map(|w| w.id).collect()
            } else {
                self.wallpapers.iter().map(|w| w.id).collect()
            }
        };
        if ids.is_empty() {
            self.set_status_error("No wallpapers to rotate");
            return;
        }
        let ids_clone = ids.clone();
        match self.send(ClientCommand::SetTimer { ids, interval_secs, shuffle: false }).await {
            Ok(DaemonResponse::TimerState { active, interval_secs: iv, ids: resp_ids, .. }) => {
                self.timer_active         = active;
                self.timer_interval_secs  = iv;
                self.timer_remaining_secs = iv;
                self.timer_last_tick      = Some(std::time::Instant::now());
                self.timer_ids            = if resp_ids.is_empty() { ids_clone } else { resp_ids };
                self.set_status_ok(format!("Timer: every {}", TIMER_OPTIONS[self.timer_dialog_idx].0));
            }
            Err(e) => self.set_status_error(e.to_string()),
            _ => {}
        }
    }

    async fn cmd_stop_timer(&mut self) {
        match self.send(ClientCommand::StopTimer).await {
            Ok(_) => {
                self.timer_active        = false;
                self.timer_interval_secs = 0;
                self.timer_last_tick     = None;
                self.set_status_ok("Timer stopped");
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    // ── Pack name dialog ──────────────────────────────────────────────────────

    pub fn open_pack_name_dialog(&mut self) {
        self.pack_name_buf = String::new();
        self.mode = AppMode::PackNameInput;
    }

    async fn handle_key_pack_name(&mut self, key: KeyEvent, _terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        match key.code {
            KeyCode::Esc => { self.mode = AppMode::Browse; }
            KeyCode::Backspace => { self.pack_name_buf.pop(); }
            KeyCode::Enter => {
                let name = self.pack_name_buf.trim().to_owned();
                if !name.is_empty() && !self.packs.iter().any(|p| p.name == name) {
                    self.packs.push(Pack { name, ids: Vec::new() });
                    self.save_tui_config();
                    self.set_status_ok("Pack created");
                }
                self.mode = AppMode::Browse;
            }
            KeyCode::Char(c) if self.pack_name_buf.len() < 32 => {
                self.pack_name_buf.push(c);
            }
            _ => {}
        }
    }

    // ── Favorites ─────────────────────────────────────────────────────────────

    async fn cmd_toggle_favorite(&mut self) {
        let id = match self.current_view().get(self.selected) {
            Some(w) => w.id,
            None    => return,
        };
        if self.favorites.contains(&id) {
            self.favorites.remove(&id);
            self.set_status_ok("Removed from Favorites");
        } else {
            self.favorites.insert(id);
            self.set_status_ok("★ Added to Favorites");
        }
        self.save_tui_config();
    }

    // ── Add to pack ───────────────────────────────────────────────────────────

    fn cmd_add_to_pack(&mut self) {
        let id = match self.current_view().get(self.selected) {
            Some(w) => w.id,
            None    => return,
        };
        if let NavItem::Pack(i) = self.current_nav() {
            if let Some(pack) = self.packs.get_mut(i) {
                let (added, name) = if !pack.ids.contains(&id) {
                    pack.ids.push(id);
                    (true, pack.name.clone())
                } else {
                    pack.ids.retain(|&x| x != id);
                    (false, pack.name.clone())
                };
                self.save_tui_config();
                if added {
                    self.set_status_ok(format!("Added to pack '{name}'"));
                } else {
                    self.set_status_ok("Removed from pack");
                }
                return;
            }
        }
        self.set_status_error("Navigate to a Pack in the left panel first");
    }

    // ── Pack picker ───────────────────────────────────────────────────────────

    fn open_pack_picker(&mut self) {
        if let NavItem::Pack(i) = self.current_nav() {
            self.pack_picker_pack = i;
            self.pack_picker_sel  = 0;
            self.mode = AppMode::PackPicker;
        } else {
            self.set_status_error("Navigate to a Pack in the left panel first");
        }
    }

    fn handle_key_pack_picker(&mut self, key: KeyEvent) {
        let total = self.wallpapers.len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.mode = AppMode::Browse;
            }
            KeyCode::Up | KeyCode::Char('k') if self.pack_picker_sel > 0 => {
                self.pack_picker_sel -= 1;
            }
            KeyCode::Down | KeyCode::Char('j') if self.pack_picker_sel + 1 < total => {
                self.pack_picker_sel += 1;
            }
            KeyCode::Char(' ') | KeyCode::Enter => {
                if let Some(w) = self.wallpapers.get(self.pack_picker_sel) {
                    let id = w.id;
                    if let Some(pack) = self.packs.get_mut(self.pack_picker_pack) {
                        if pack.ids.contains(&id) {
                            pack.ids.retain(|&x| x != id);
                        } else {
                            pack.ids.push(id);
                        }
                        self.save_tui_config();
                    }
                }
            }
            _ => {}
        }
    }

    // ── Sort dialog ───────────────────────────────────────────────────────────

    fn handle_key_sort_dialog(&mut self, key: KeyEvent) {
        let max = SORT_OPTIONS.len() - 1;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => { self.mode = AppMode::Browse; }
            KeyCode::Up | KeyCode::Char('k') if self.sort_dialog_sel > 0 => {
                self.sort_dialog_sel -= 1;
            }
            KeyCode::Down | KeyCode::Char('j') if self.sort_dialog_sel < max => {
                self.sort_dialog_sel += 1;
            }
            KeyCode::Char('d') => { self.sort_desc = !self.sort_desc; }
            KeyCode::Enter | KeyCode::Char(' ') => {
                self.sort_mode = SORT_OPTIONS[self.sort_dialog_sel].1;
                self.reset_list_selection();
                self.mode = AppMode::Browse;
            }
            _ => {}
        }
    }

    // ── Persist TUI config ────────────────────────────────────────────────────

    fn save_tui_config(&mut self) {
        self.config.tui.favorites    = self.favorites.iter().copied().collect();
        self.config.tui.packs        = self.packs.clone();
        self.config.tui.show_nav     = self.show_nav;
        self.config.tui.show_preview = self.show_preview;
        // theme is already written directly to self.config.tui.theme on selection
        if let Err(e) = self.config.save() {
            tracing::warn!("TUI config save failed: {}", e);
        }
    }

    // ── Wallpaper commands ────────────────────────────────────────────────────

    async fn cmd_set(&mut self) {
        let id = match self.current_view().get(self.selected) {
            Some(w) => w.id,
            None    => return,
        };
        match self.send(ClientCommand::Set { id, monitor: None }).await {
            Ok(_) => {
                self.current_wallpaper_id = Some(id);
                self.wallpaper_applied_at = Some(std::time::Instant::now());
                self.set_status_ok("\u{2713} Applied");
                // Record play for Frequent tracking.
                let _ = self.send(ClientCommand::RecordPlay { id }).await;
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    async fn cmd_set_to_selected_monitor(&mut self) {
        let id = match self.current_view().get(self.selected) {
            Some(w) => w.id,
            None    => return,
        };
        let monitor = if self.monitor_selected == 0 {
            None
        } else {
            self.monitors.get(self.monitor_selected - 1).map(|(n, _, _)| n.clone())
        };
        match self.send(ClientCommand::Set { id, monitor }).await {
            Ok(_) => {
                self.current_wallpaper_id = Some(id);
                self.wallpaper_applied_at = Some(std::time::Instant::now());
                self.set_status_ok("\u{2713} Applied");
                let _ = self.send(ClientCommand::RecordPlay { id }).await;
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    async fn cmd_change_volume(&mut self, delta: f32) {
        // Round to nearest 1 % after each step to prevent f32 drift.
        // Without rounding, 0.85 - 0.05 = 0.7999... which accumulates:
        // 85→79→74→69 instead of the correct 85→80→75→70.
        let raw     = self.config.general.volume + delta;
        let new_vol = ((raw * 100.0).round() / 100.0).clamp(0.0, 1.0);
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
                self.config.general.volume = volume;
                self.config.general.muted  = muted;
                self.set_status_ok(if muted { "Muted" } else { "Unmuted" });
            }
            Ok(_) => {
                self.config.general.muted = !self.config.general.muted;
                self.set_status_ok(if self.config.general.muted { "Muted" } else { "Unmuted" });
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

    // ── Refresh ───────────────────────────────────────────────────────────────

    pub async fn refresh_list(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        self.loading = true;
        let prev_id = self.current_view().get(self.selected).map(|w| w.id);

        let send_ok = match self.client.as_mut() {
            None => { self.daemon_connected = false; self.loading = false; return; }
            Some(c) => tokio::time::timeout(
                Duration::from_secs(5),
                c.send_cmd_only(&ClientCommand::Scan),
            ).await,
        };
        if let Err(_) | Ok(Err(_)) = send_ok {
            self.client = None;
            self.daemon_connected = false;
            self.set_status_error("Scan failed");
            self.loading = false;
            return;
        }

        let items = loop {
            let _ = terminal.draw(|f| ui::render(f, self));
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
                Some(c) => tokio::time::timeout(Duration::from_millis(50), c.recv_resp()).await,
            };
            match resp {
                Ok(Ok(DaemonResponse::ScanProgress { done, total })) => {
                    self.status_message  = Some(format!("Scanning\u{2026} {done}/{total}  [Esc] cancel"));
                    self.status_is_error = false;
                    self.status_clear_at = None;
                }
                Ok(Ok(DaemonResponse::WallpaperList { items })) => break Some(items),
                Ok(Ok(DaemonResponse::Error { message })) => {
                    self.set_status_error(message);
                    break None;
                }
                Ok(Ok(_))     => break None,
                Ok(Err(e))    => {
                    self.client = None;
                    self.daemon_connected = false;
                    self.set_status_error(e.to_string());
                    break None;
                }
                Err(_timeout) => continue,
            }
        };

        if let Some(items) = items {
            self.preview_id = None;
            self.wallpapers = items;
            // Refresh frequent list.
            self.refresh_frequent().await;
            if let Some(id) = prev_id {
                if let Some(pos) = self.current_view().iter().position(|w| w.id == id) {
                    self.selected = pos;
                    self.list_state.select(Some(pos));
                    self.loading = false;
                    self.update_preview();
                    return;
                }
            }
            let len = self.current_view().len();
            self.selected = self.selected.min(len.saturating_sub(1));
            self.list_state.select(if len == 0 { None } else { Some(self.selected) });
            self.update_preview();
        }
        self.loading = false;
    }

    async fn refresh_frequent(&mut self) {
        // Ask the daemon to query play counts from the DB.
        if let Ok(DaemonResponse::FrequentList { items }) =
            self.send(ClientCommand::GetFrequent { limit: 10 }).await
        {
            self.frequent = items;
            return;
        }
        // Fallback: sort wallpapers by insertion order (most-recently-added first)
        // when the daemon doesn't support GetFrequent yet.
        self.frequent = self.wallpapers.iter().rev().take(10).cloned().collect();
    }

    async fn send(&mut self, cmd: ClientCommand) -> anyhow::Result<DaemonResponse> {
        let client = match self.client.as_mut() {
            None => { self.daemon_connected = false; anyhow::bail!("Not connected"); }
            Some(c) => c,
        };
        match tokio::time::timeout(Duration::from_secs(2), client.send(&cmd)).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(e))   => { self.client = None; self.daemon_connected = false; Err(e) }
            Err(_)       => { self.client = None; self.daemon_connected = false; anyhow::bail!("Timeout") }
        }
    }

    pub async fn try_reconnect(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        if self.client.is_some() { return; } // already connected — don't leak or replace the socket
        if let Some(last) = self.last_reconnect {
            if last.elapsed() < Duration::from_secs(2) { return; }
        }
        self.last_reconnect = Some(std::time::Instant::now());
        if let Some(client) = tokio::time::timeout(
            Duration::from_millis(200),
            IpcClient::try_connect(&self.dirs.socket_path),
        ).await.unwrap_or(None) {
            self.client = Some(client);
            self.daemon_connected = true;
            self.last_reconnect = None;
            self.sync_volume_state().await;
            self.sync_timer_state().await;
            self.refresh_monitors().await;
            self.refresh_list(terminal).await;
        }
    }

    pub async fn refresh_monitors(&mut self) {
        if let Ok(DaemonResponse::OutputList { names, resolutions }) =
            self.send(ClientCommand::ListOutputs).await
        {
            self.monitors = names.into_iter()
                .enumerate()
                .map(|(i, name)| {
                    let (w, h) = resolutions.get(i).copied().unwrap_or((0, 0));
                    (name, w, h)
                })
                .collect();
        }
    }


    async fn sync_volume_state(&mut self) {
        if let Ok(DaemonResponse::VolumeState { volume, muted, current_id }) =
            self.send(ClientCommand::Status).await
        {
            self.config.general.volume = volume;
            self.config.general.muted  = muted;
            if current_id.is_some() { self.current_wallpaper_id = current_id; }
        }
    }

    async fn sync_timer_state(&mut self) {
        if let Ok(DaemonResponse::TimerState { active, interval_secs, remaining_secs, ids }) =
            self.send(ClientCommand::GetTimerState).await
        {
            self.timer_active         = active;
            self.timer_interval_secs  = interval_secs;
            self.timer_remaining_secs = remaining_secs;
            self.timer_ids            = ids;
            self.timer_last_tick = if active && interval_secs > 0 {
                // Offset tick backwards so the countdown shows the correct remaining time
                // immediately after reconnect (not a full reset to interval_secs).
                let elapsed_offset = interval_secs.saturating_sub(remaining_secs);
                Some(std::time::Instant::now() - Duration::from_secs(elapsed_offset))
            } else {
                None
            };
        }
    }

}

// ── Folder picker ─────────────────────────────────────────────────────────────

const VIDEO_EXTS_FP: &[&str] = &["mp4", "webm", "mkv", "avi", "mov", "gif", "wmv", "flv"];
const SYSTEM_PATHS: &[&str]  = &["/proc", "/sys", "/dev", "/run"];

pub fn fp_is_system(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    SYSTEM_PATHS.iter().any(|sp| s == *sp || s.starts_with(&format!("{}/", sp)))
}

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

impl App {
    pub fn open_folder_picker(&mut self) {
        self.mode = AppMode::FolderPicker;
        self.load_fp_entries();
    }

    pub fn load_fp_entries(&mut self) {
        let mut entries: Vec<String> = std::fs::read_dir(&self.fp_path)
            .into_iter().flatten().flatten()
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| !n.starts_with('.'))
            .collect();
        entries.sort();
        if self.fp_path.parent().is_some() { entries.insert(0, "..".to_owned()); }
        self.fp_entries  = entries;
        self.fp_selected = self.fp_selected.min(self.fp_entries.len().saturating_sub(1));
        self.fp_hints.clear();
        for name in &self.fp_entries {
            if name == ".." { continue; }
            let full = self.fp_path.join(name);
            self.fp_hints.insert(full.clone(), fp_dir_hint(&full));
        }
    }

    async fn handle_key_folder_picker(&mut self, key: KeyEvent, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if self.fp_selected > 0 => {
                self.fp_selected -= 1;
            }
            KeyCode::Down | KeyCode::Char('j')
                if self.fp_selected + 1 < self.fp_entries.len() =>
            {
                self.fp_selected += 1;
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(name) = self.fp_entries.get(self.fp_selected).cloned() {
                    let next = if name == ".." {
                        self.fp_path.parent().map(|p| p.to_path_buf())
                            .unwrap_or_else(|| self.fp_path.clone())
                    } else {
                        self.fp_path.join(&name)
                    };
                    if fp_is_system(&next) { self.set_status_error("System path"); return; }
                    self.fp_path     = next;
                    self.fp_selected = 0;
                    self.load_fp_entries();
                }
            }
            KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                if let Some(p) = self.fp_path.parent().map(|p| p.to_path_buf()) {
                    self.fp_path     = p;
                    self.fp_selected = 0;
                    self.load_fp_entries();
                }
            }
            KeyCode::Char('a') => { self.fp_add_current_dir(terminal).await; }
            KeyCode::Char('d') | KeyCode::Delete => {
                let s = self.fp_path.to_string_lossy().into_owned();
                if self.config.paths.extra_dirs.contains(&s) {
                    self.fp_remove_dir(&s, terminal).await;
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => { self.mode = AppMode::Browse; }
            _ => {}
        }
    }

    async fn fp_add_current_dir(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        let s = self.fp_path.to_string_lossy().into_owned();
        if fp_is_system(&self.fp_path) { self.set_status_error("System path"); return; }
        if self.config.paths.extra_dirs.contains(&s) { self.set_status_error("Already added"); return; }
        self.config.paths.extra_dirs.push(s.clone());
        if let Err(e) = self.config.save() {
            self.config.paths.extra_dirs.retain(|d| d != &s);
            self.set_status_error(format!("Save failed: {e}"));
            return;
        }
        self.set_status_ok("\u{2713} Folder added");
        self.mode = AppMode::Browse;
        self.refresh_list(terminal).await;
    }

    async fn fp_remove_dir(&mut self, path_str: &str, terminal: &mut Terminal<CrosstermBackend<Stdout>>) {
        self.config.paths.extra_dirs.retain(|d| d != path_str);
        if let Err(e) = self.config.save() { self.set_status_error(format!("Save failed: {e}")); return; }
        self.set_status_ok("\u{2713} Folder removed");
        self.refresh_list(terminal).await;
    }

}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Normalize preview image to exactly 16:9 (960×540) using cover-scale:
/// fills the entire area with minimal center-crop, no black bars.
fn upscale_preview(img: image::DynamicImage) -> image::DynamicImage {
    img.resize_to_fill(960, 540, image::imageops::FilterType::Lanczos3)
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
        let dirs   = config.app_dirs().unwrap();
        App::new(config, dirs, ratatui_image::picker::Picker::halfblocks())
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
        app.wallpapers = vec![wallpaper(42, None)];
        app.update_preview();
        assert!(app.preview.is_none());
        assert_eq!(app.preview_id, Some(42));
    }

    #[test]
    fn test_update_preview_missing_file_leaves_preview_none() {
        let mut app = make_app();
        app.wallpapers = vec![wallpaper(1, Some("/tmp/wpick_nonexistent_9876543.jpg"))];
        app.update_preview();
        assert!(app.preview.is_none());
        assert_eq!(app.preview_id, Some(1));
    }

    #[test]
    fn test_update_preview_same_id_skips_reload() {
        let mut app = make_app();
        app.wallpapers = vec![wallpaper(7, Some("/tmp/wpick_nonexistent_9876543.jpg"))];
        app.update_preview();
        assert_eq!(app.preview_id, Some(7));
        app.preview_id = Some(7);
        app.update_preview();
        assert_eq!(app.preview_id, Some(7));
    }

    #[test]
    fn test_update_preview_no_picker_sets_id_only() {
        let mut app = make_app();
        app.picker     = None;
        app.wallpapers = vec![wallpaper(5, Some("/tmp/some_preview.jpg"))];
        app.update_preview();
        assert!(app.preview.is_none());
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
        app.wallpapers = vec![wallpaper(99, Some(tmp.to_str().unwrap()))];
        app.update_preview();
        assert!(app.preview.is_some());
        assert_eq!(app.preview_id, Some(99));
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn test_select_next_updates_preview_id() {
        let mut app = make_app();
        app.wallpapers = vec![wallpaper(1, None), wallpaper(2, None)];
        app.update_preview();
        assert_eq!(app.preview_id, Some(1));
        app.select_next();
        assert_eq!(app.preview_id, Some(2));
    }

    #[test]
    fn test_select_prev_updates_preview_id() {
        let mut app = make_app();
        app.wallpapers = vec![wallpaper(10, None), wallpaper(20, None)];
        app.selected   = 1;
        app.update_preview();
        assert_eq!(app.preview_id, Some(20));
        app.select_prev();
        assert_eq!(app.preview_id, Some(10));
    }

    #[test]
    fn test_favorites_toggle() {
        let mut app = make_app();
        app.wallpapers = vec![wallpaper(42, None)];
        assert!(!app.favorites.contains(&42));
        app.favorites.insert(42);
        assert!(app.favorites.contains(&42));
        app.favorites.remove(&42);
        assert!(!app.favorites.contains(&42));
    }

    #[test]
    fn test_nav_items_contains_favorites_and_frequent() {
        let app   = make_app();
        let items = app.nav_items();
        assert!(items.contains(&NavItem::Favorites));
        assert!(items.contains(&NavItem::Frequent));
    }

    #[test]
    fn test_pack_creation() {
        let mut app = make_app();
        app.packs.push(Pack { name: "Gaming".into(), ids: vec![1, 2] });
        let items = app.nav_items();
        assert!(items.contains(&NavItem::Pack(0)));
    }
}
