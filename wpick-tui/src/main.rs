mod app;
mod client;
mod ui;

use anyhow::{Context, Result};
use app::App;
use client::IpcClient;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use wpick_core::config::{AppDirs, WpickConfig};
use wpick_core::ipc::{ClientCommand, DaemonResponse};

// ── CLI definition ────────────────────────────────────────────────────────────

#[derive(clap::Parser)]
#[command(name = "wpick", about = "Wallpaper Engine for Wayland")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(clap::Subcommand)]
enum Commands {
    /// List all wallpapers
    List,
    /// Set wallpaper by ID
    Set { id: u64 },
    /// Set volume (0-100)
    Volume { percent: u8 },
    /// Toggle mute
    Mute,
    /// Show wallpaper info
    Info { id: u64 },
    /// Start daemon in background
    Daemon,
    /// Kill daemon
    Kill,
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    use clap::Parser;
    let cli = Cli::parse();

    let config = WpickConfig::load().context("Failed to load config")?;
    let dirs   = config.app_dirs().context("Failed to resolve app dirs")?;

    match cli.command {
        None           => run_tui(config, dirs).await,
        Some(cmd)      => run_cli(cmd, &dirs).await,
    }
}

// ── CLI mode ──────────────────────────────────────────────────────────────────

async fn run_cli(cmd: Commands, dirs: &AppDirs) -> Result<()> {
    // Daemon subcommand doesn't need a socket connection
    if let Commands::Daemon = cmd {
        std::process::Command::new("wpick-daemon")
            .spawn()
            .context("Failed to start wpick-daemon")?;
        println!("\u{2713} Daemon started");
        return Ok(());
    }

    let mut client = IpcClient::connect(&dirs.socket_path)
        .await
        .context("Cannot connect. Start daemon with: wpick daemon")?;

    match cmd {
        Commands::List => {
            let items = client.list_wallpapers().await?;
            println!("{:<14} {:<8} {:<6} TITLE", "ID", "TYPE", "AUDIO");
            println!("{}", "-".repeat(62));
            for w in &items {
                println!("{:<14} {:<8} {:<6} {}",
                    w.id,
                    w.wallpaper_type.to_string(),
                    if w.has_audio { "\u{266a}" } else { "-" },
                    w.title);
            }
            println!("\n{} wallpapers found", items.len());
        }

        Commands::Set { id } => {
            client.set_wallpaper(id).await?;
            println!("\u{2713} Wallpaper set: {}", id);
        }

        Commands::Volume { percent } => {
            let level = (percent as f32 / 100.0).clamp(0.0, 1.0);
            client.send(&ClientCommand::Volume { level }).await?;
            println!("\u{2713} Volume: {}%", percent);
        }

        Commands::Mute => {
            client.send(&ClientCommand::Mute).await?;
            println!("\u{2713} Mute toggled");
        }



        Commands::Info { id } => {
            match client.send(&ClientCommand::Info { id }).await? {
                DaemonResponse::WallpaperInfo { item } => {
                    println!("ID:    {}", item.id);
                    println!("Title: {}", item.title);
                    println!("Type:  {}", item.wallpaper_type);
                    println!("Audio: {}", if item.has_audio { "Yes" } else { "No" });
                    println!("Size:  {:.1} MB",
                        item.file_size_bytes as f64 / 1_048_576.0);
                    println!("File:  {}", item.file_path);
                }
                DaemonResponse::Error { message } => {
                    eprintln!("Error: {}", message);
                }
                _ => {}
            }
        }

        Commands::Kill => {
            client.send(&ClientCommand::Kill).await.ok();
            println!("\u{2713} Daemon killed");
        }

        Commands::Daemon => unreachable!("handled above"),
    }

    Ok(())
}

// ── TUI mode ──────────────────────────────────────────────────────────────────

async fn run_tui(config: WpickConfig, dirs: AppDirs) -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;

    let result = {
        let backend  = ratatui::backend::CrosstermBackend::new(std::io::stdout());
        let mut term = ratatui::Terminal::new(backend)?;
        let mut app  = App::new(config, dirs);
        app.run(&mut term).await
    };

    // Always restore terminal even if app returned an error
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), LeaveAlternateScreen)?;

    result
}
