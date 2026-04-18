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
    /// Launch TUI (don't auto-start daemon)
    Tui,
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
    /// Start daemon in foreground (replaces current process)
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
        None => {
            if !is_daemon_running(&dirs.socket_path).await {
                start_daemon()?;
                wait_for_socket(&dirs.socket_path, std::time::Duration::from_secs(3))
                    .await
                    .context("Daemon started but socket didn't appear within 3s")?;
            }
            run_tui(config, dirs).await
        }
        Some(cmd) => run_cli(cmd, config, dirs).await,
    }
}

// ── CLI mode ──────────────────────────────────────────────────────────────────

async fn run_cli(cmd: Commands, config: WpickConfig, dirs: AppDirs) -> Result<()> {
    match cmd {
        Commands::Tui => return run_tui(config, dirs).await,
        Commands::Daemon => {
            println!("Starting wpick-daemon in foreground...");
            println!("Use 'wpick' (without arguments) to start daemon in background + TUI");
            let err = std::os::unix::process::CommandExt::exec(
                &mut std::process::Command::new("wpick-daemon"),
            );
            return Err(anyhow::anyhow!("Failed to exec wpick-daemon: {err}"));
        }
        _ => {}
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
                println!(
                    "{:<14} {:<8} {:<6} {}",
                    w.id,
                    w.wallpaper_type.to_string(),
                    if w.has_audio { "\u{266a}" } else { "-" },
                    w.title
                );
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
                    println!("Size:  {:.1} MB", item.file_size_bytes as f64 / 1_048_576.0);
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

        Commands::Tui | Commands::Daemon => unreachable!("handled above"),
    }

    Ok(())
}

// ── Daemon helpers ────────────────────────────────────────────────────────────

/// Returns true if daemon is reachable on the socket.
async fn is_daemon_running(socket_path: &std::path::Path) -> bool {
    tokio::net::UnixStream::connect(socket_path).await.is_ok()
}

/// Spawns wpick-daemon as a detached background process.
fn start_daemon() -> anyhow::Result<()> {
    std::process::Command::new("wpick-daemon")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("Failed to spawn wpick-daemon. Is it in PATH?")?;
    Ok(())
}

/// Polls socket path until it exists and is connectable, or timeout.
async fn wait_for_socket(
    socket_path: &std::path::Path,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if tokio::net::UnixStream::connect(socket_path).await.is_ok() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("Timeout waiting for daemon socket at {:?}", socket_path);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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
