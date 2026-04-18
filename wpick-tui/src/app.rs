use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use std::io::Stdout;
use std::time::Duration;
use wpick_core::config::{AppDirs, WpickConfig};
use wpick_core::ipc::{ClientCommand, DaemonResponse};
use wpick_core::model::WallpaperInfo;

use crate::client::IpcClient;
use crate::ui;

#[derive(Debug, Clone, PartialEq)]
pub enum AppMode {
    Browse,
    Detail,
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
}

impl App {
    pub fn new(config: WpickConfig, dirs: AppDirs) -> Self {
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

    fn select_next(&mut self) {
        let len = self.wallpapers.len();
        if len > 0 {
            self.selected = (self.selected + 1).min(len - 1);
            self.list_state.select(Some(self.selected));
        }
    }

    fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.list_state.select(Some(self.selected));
        }
    }

    pub async fn run(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        self.try_reconnect().await;

        loop {
            if self.client.is_none() {
                self.try_reconnect().await;
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
                    self.handle_key(key).await;
                }
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }

    async fn handle_key(&mut self, key: KeyEvent) {
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
            KeyCode::Char('a') => {
                self.cmd_mute().await;
            }
            KeyCode::Char('r') => {
                self.refresh_list().await;
            }
            KeyCode::Char('i') => {
                self.mode = match self.mode {
                    AppMode::Browse => AppMode::Detail,
                    AppMode::Detail => AppMode::Browse,
                };
            }
            _ => {}
        }
    }

    async fn cmd_set(&mut self) {
        let id = match self.wallpapers.get(self.selected) {
            Some(w) => w.id,
            None => return,
        };
        match self.send(ClientCommand::Set { id }).await {
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
            Ok(_) => {
                self.config.general.volume = new_vol;
                self.set_status_ok(format!("Vol {:.0}%", new_vol * 100.0));
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    async fn cmd_mute(&mut self) {
        match self.send(ClientCommand::Mute).await {
            Ok(_) => {
                self.config.general.muted = !self.config.general.muted;
                let label = if self.config.general.muted { "Muted" } else { "Unmuted" };
                self.set_status_ok(label);
            }
            Err(e) => self.set_status_error(e.to_string()),
        }
    }

    async fn cmd_kill(&mut self) {
        let _ = self.send(ClientCommand::Kill).await;
    }

    pub async fn refresh_list(&mut self) {
        self.loading = true;
        let prev_id = self.wallpapers.get(self.selected).map(|w| w.id);

        match self.send(ClientCommand::List).await {
            Ok(DaemonResponse::WallpaperList { items }) => {
                self.wallpapers = items;
                if let Some(id) = prev_id {
                    if let Some(pos) = self.wallpapers.iter().position(|w| w.id == id) {
                        self.selected = pos;
                        self.list_state.select(if self.wallpapers.is_empty() { None } else { Some(self.selected) });
                        self.loading = false;
                        return;
                    }
                }
                self.selected = self.selected.min(self.wallpapers.len().saturating_sub(1));
                self.list_state.select(if self.wallpapers.is_empty() { None } else { Some(self.selected) });
            }
            Ok(DaemonResponse::Error { message }) => {
                self.set_status_error(message);
            }
            Ok(_) => {}
            Err(e) => self.set_status_error(e.to_string()),
        }

        self.loading = false;
    }

    async fn send(&mut self, cmd: ClientCommand) -> anyhow::Result<DaemonResponse> {
        match self.client.as_mut() {
            None => {
                self.daemon_connected = false;
                anyhow::bail!("Not connected to daemon")
            }
            Some(client) => match client.send(&cmd).await {
                Ok(resp) => Ok(resp),
                Err(e) => {
                    self.client = None;
                    self.daemon_connected = false;
                    Err(e)
                }
            },
        }
    }

    pub async fn try_reconnect(&mut self) {
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
            self.refresh_list().await;
        }
    }
}