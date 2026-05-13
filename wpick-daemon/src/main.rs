mod audio;
mod ducking;
mod hw_decode;
mod ipc_server;
mod renderer;
mod state;
mod video;

use std::sync::{Arc, Mutex};

use anyhow::Context;
use tokio::sync;

use wpick_core::cache::Cache;
use wpick_core::config::WpickConfig;
use wpick_core::model::WallpaperInfo;

use crate::state::DaemonState;

// ─── Competing wallpaper tools ────────────────────────────────────────────────

struct WallpaperTool {
    process: &'static str,
    display: &'static str,
}

const COMPETING_TOOLS: &[WallpaperTool] = &[
    WallpaperTool { process: "hyprpaper",   display: "hyprpaper"  },
    WallpaperTool { process: "swww-daemon", display: "swww"       },
    WallpaperTool { process: "swaybg",      display: "swaybg"     },
    WallpaperTool { process: "mpvpaper",    display: "mpvpaper"   },
    WallpaperTool { process: "wpaperd",     display: "wpaperd"    },
    WallpaperTool { process: "feh",         display: "feh"        },
    WallpaperTool { process: "nitrogen",    display: "nitrogen"   },
    WallpaperTool { process: "xwallpaper",  display: "xwallpaper" },
];

/// Scan /proc for known competing wallpaper processes.
fn scan_competing_processes() -> Vec<(&'static str, u32)> {
    let mut found = Vec::new();
    let Ok(proc) = std::fs::read_dir("/proc") else { return found; };
    for entry in proc.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.chars().all(|c| c.is_ascii_digit()) { continue; }

        let cmdline_path = entry.path().join("cmdline");
        let Ok(cmdline)  = std::fs::read(&cmdline_path) else { continue };
        let exe      = cmdline.split(|&b| b == 0).next().unwrap_or(&[]);
        let exe      = std::str::from_utf8(exe).unwrap_or("").trim();
        let basename = exe.rsplit('/').next().unwrap_or(exe);

        for tool in COMPETING_TOOLS {
            if basename == tool.process {
                if let Ok(pid) = name.parse::<u32>() {
                    found.push((tool.display, pid));
                }
                break;
            }
        }
    }
    found
}

/// Resume all suspended (SIGSTOP'd) competitor processes.
fn resume_paused_tools(pids: &[u32]) {
    for &pid in pids {
        tracing::info!("resuming paused tool (pid {})", pid);
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGCONT); }
    }
}

/// Handle competing wallpaper tools.
///
/// `pause_mode = false` (default): SIGTERM → wait 500 ms → SIGKILL if still alive.
///   Removes their Wayland surfaces so our surface is visible immediately.
///
/// `pause_mode = true`: SIGSTOP (suspend, not kill).
///   Their surfaces stay on the compositor but their process is frozen.
///   Returns the paused PIDs so the caller can SIGCONT them on exit.
fn handle_competing_tools(pause_mode: bool) -> Vec<u32> {
    let found = scan_competing_processes();
    if found.is_empty() { return Vec::new(); }

    let mut paused_pids = Vec::new();

    for (name, pid) in &found {
        if pause_mode {
            tracing::info!("pausing competing tool: {} (pid {})", name, pid);
            eprintln!("wpick: приостанавливаю {} (pid {})...", name, pid);
            unsafe { libc::kill(*pid as libc::pid_t, libc::SIGSTOP); }
            paused_pids.push(*pid);
        } else {
            tracing::info!("stopping competing tool: {} (pid {})", name, pid);
            eprintln!("wpick: останавливаю {} (pid {})...", name, pid);
            unsafe { libc::kill(*pid as libc::pid_t, libc::SIGTERM); }
        }
    }

    if !pause_mode {
        // Grace period — let them exit cleanly before SIGKILL.
        std::thread::sleep(std::time::Duration::from_millis(500));
        for (name, pid) in &found {
            let alive = unsafe { libc::kill(*pid as libc::pid_t, 0) } == 0;
            if alive {
                tracing::warn!("{} (pid {}) did not exit — SIGKILL", name, pid);
                unsafe { libc::kill(*pid as libc::pid_t, libc::SIGKILL); }
            }
        }
        // Brief pause so the compositor can destroy their surfaces before ours appears.
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    paused_pids
}

// ─── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Config + dirs
    let config = WpickConfig::load().context("Failed to load config")?;
    let dirs   = config.app_dirs().context("Failed to resolve app dirs")?;

    // 2. Logging — journal when running under systemd, rolling file otherwise.
    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive("wpick_daemon=info".parse()?)
        .add_directive("wpick_core=info".parse()?);

    if std::env::var_os("JOURNAL_STREAM").is_some() {
        // systemd captures stderr; disable ANSI colours so journal formats cleanly.
        tracing_subscriber::fmt()
            .with_writer(std::io::stderr)
            .with_ansi(false)
            .with_env_filter(env_filter)
            .init();
    } else {
        let file_appender = tracing_appender::rolling::daily(&dirs.log_dir, "wpick.log");
        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
        tracing_subscriber::fmt()
            .with_writer(non_blocking)
            .with_env_filter(env_filter)
            .init();
    }

    tracing::info!("wpick-daemon starting");

    // 3. Open database
    let cache = Arc::new(sync::Mutex::new(
        Cache::open(&dirs.db_path).context("Failed to open cache DB")?,
    ));

    // 4. Channels
    //    Single watch channel — renderer and audio both subscribe to the same
    //    Sender so they see a wallpaper change in the same Tokio tick (no A/V skew).
    let (wallpaper_tx, wallpaper_rx) = sync::watch::channel(None::<WallpaperInfo>);
    let renderer_rx = wallpaper_rx.clone();
    let audio_rx    = wallpaper_rx;

    let (volume_tx,   volume_rx)   = sync::watch::channel(
        (config.general.volume, config.general.muted),
    );
    let (shutdown_tx, shutdown_rx) = sync::broadcast::channel::<()>(1);

    // Per-monitor overrides channel — renderer subscribes to apply dynamic pins.
    let (per_monitor_tx, per_monitor_rx) =
        sync::watch::channel(std::collections::HashMap::<String, Option<WallpaperInfo>>::new());
    // Shared output list published by the renderer and read by the IPC server.
    let outputs: Arc<std::sync::Mutex<Vec<String>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let outputs_renderer = Arc::clone(&outputs);

    // 5. DaemonState
    let state = Arc::new(sync::Mutex::new(DaemonState {
        current:        None,
        volume:         config.general.volume,
        muted:          config.general.muted,
        wallpaper_tx,
        volume_tx,
        shutdown_tx:    shutdown_tx.clone(),
        per_monitor_tx,
        outputs,
    }));

    // 5b. Restore last wallpaper from config (persist-on-restart).
    if let Some(last_id) = config.last_wallpaper_id {
        let guard = cache.lock().await;
        match guard.get_by_id(last_id) {
            Ok(Some(info)) if info.is_supported() => {
                tracing::info!("restoring last wallpaper id={}", last_id);
                state.lock().await.set_wallpaper(info);
            }
            Ok(Some(_)) => tracing::info!("last wallpaper id={} unsupported — skipping", last_id),
            Ok(None)    => tracing::info!("last wallpaper id={} not in cache — skipping", last_id),
            Err(e)      => tracing::warn!("failed to look up last wallpaper id={}: {}", last_id, e),
        }
    }

    // 6 + 7. Atomic socket bind (TOCTOU-safe: try-bind first).
    let socket_path = dirs.socket_path.clone();
    let listener = match tokio::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if tokio::net::UnixStream::connect(&socket_path).await.is_ok() {
                anyhow::bail!(
                    "Another wpick-daemon is already running at {:?}",
                    socket_path
                );
            }
            tracing::info!("Removing stale socket at {:?}", socket_path);
            std::fs::remove_file(&socket_path).context("Failed to remove stale socket")?;
            tokio::net::UnixListener::bind(&socket_path)
                .context("Failed to bind socket after removing stale one")?
        }
        Err(e) => return Err(e).context("Failed to bind Unix socket"),
    };
    tracing::info!("Listening on {:?}", socket_path);

    // 8. Handle competing wallpaper tools in a background thread so the daemon
    //    starts (IPC + renderer) immediately without blocking on the 700 ms
    //    SIGTERM grace period.
    //    The returned PIDs are shared with signal handlers for SIGCONT on exit.
    let paused_pids: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let pids       = Arc::clone(&paused_pids);
        let pause_mode = config.general.pause_competitors;
        std::thread::Builder::new()
            .name("handle-competitors".into())
            .spawn(move || {
                let stopped = handle_competing_tools(pause_mode);
                if !stopped.is_empty() {
                    match pids.lock() {
                        Ok(mut g) => { *g = stopped; }
                        Err(e)    => tracing::warn!("paused_pids mutex poisoned: {}", e),
                    }
                }
            })
            .context("Failed to spawn competitor-handler thread")?;
    }

    // 8b. Competitor watchdog — only active in kill-mode (pause_competitors = false).
    //     In pause-mode (SIGSTOP) the shell sees the process as alive and won't restart
    //     it, so no watchdog is needed.  In kill-mode the shell may restart the process,
    //     so we re-scan every 5 s (not faster — rapid scanning causes a kill/restart loop
    //     that crashes shell components like QuickShell).
    if !config.general.pause_competitors {
        let pids = Arc::clone(&paused_pids);
        std::thread::Builder::new()
            .name("competitor-watchdog".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(5));
                let stopped = handle_competing_tools(false);
                if !stopped.is_empty() {
                    tracing::info!("watchdog: re-killed {} competing tool(s)", stopped.len());
                    if let Ok(mut g) = pids.lock() { g.extend_from_slice(&stopped); }
                }
            })
            .context("Failed to spawn competitor watchdog thread")?;
    }

    // 9. Cleanup helper — removes socket, resumes any SIGSTOP'd competitors.
    let cleanup = {
        let sp   = socket_path.clone();
        let pids = Arc::clone(&paused_pids);
        move || {
            let _ = std::fs::remove_file(&sp);
            if let Ok(g) = pids.lock() { resume_paused_tools(&g); }
        }
    };

    // 10. Signal handlers (SIGINT / SIGTERM) — graceful shutdown via broadcast.
    {
        let sp   = socket_path.clone();
        let sd   = shutdown_tx.clone();
        let pids = Arc::clone(&paused_pids);
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("SIGINT — shutting down");
            if let Ok(g) = pids.lock() { resume_paused_tools(&g); }
            let _ = sd.send(());
            let _ = std::fs::remove_file(&sp);
            std::process::exit(0);
        });
    }
    #[cfg(unix)]
    {
        let sp   = socket_path.clone();
        let sd   = shutdown_tx.clone();
        let pids = Arc::clone(&paused_pids);
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            if let Ok(mut sig) = signal(SignalKind::terminate()) {
                sig.recv().await;
                tracing::info!("SIGTERM — shutting down");
                if let Ok(g) = pids.lock() { resume_paused_tools(&g); }
                let _ = sd.send(());
                let _ = std::fs::remove_file(&sp);
                std::process::exit(0);
            }
        });
    }

    // 11. IPC server task
    {
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let dirs  = dirs.clone();
        tokio::spawn(async move {
            if let Err(e) = ipc_server::run(listener, state, cache, dirs).await {
                tracing::error!("IPC server error: {}", e);
            }
        });
    }

    // 12. Audio task — dedicated OS thread (rodio OutputStream is !Send)
    {
        let audio_cfg       = config.audio.clone();
        let ducking_enabled = config.audio.ducking_enabled;
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let duck = if ducking_enabled {
                    ducking::start()
                } else {
                    tracing::info!("Ducking disabled by config");
                    ducking::start_noop()
                };
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => { tracing::error!("audio: failed to build runtime: {}", e); return; }
                };
                if let Err(e) = rt.block_on(audio::run(duck, audio_rx, volume_rx, audio_cfg)) {
                    tracing::error!("Audio task: {}", e);
                }
            })
            .context("Failed to spawn audio thread")?;
    }

    // 13. Renderer — must run on this thread (Wayland is !Send)
    tracing::info!("Starting renderer");
    // When fullscreen exits, any competitor that was restarted by a watchdog
    // (e.g. QuickShell's mpvpaper manager) is killed again so our surface stays on top.
    let on_fs_exit: Option<Arc<dyn Fn() + Send + Sync>> = {
        let pause_mode2   = config.general.pause_competitors;
        let paused_pids2  = Arc::clone(&paused_pids);
        Some(Arc::new(move || {
            tracing::info!("fullscreen exited — killing competing wallpaper tools");
            // Kill immediately
            let stopped = handle_competing_tools(pause_mode2);
            if !stopped.is_empty() {
                tracing::info!("fullscreen exit: killed {} tool(s) immediately", stopped.len());
                if let Ok(mut g) = paused_pids2.lock() { g.extend_from_slice(&stopped); }
            }
            // Kill again after 600 ms — some shells (e.g. QuickShell) restart the
            // wallpaper daemon within milliseconds of detecting it died.
            let pids3 = Arc::clone(&paused_pids2);
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(600));
                let stopped2 = handle_competing_tools(pause_mode2);
                if !stopped2.is_empty() {
                    tracing::info!("fullscreen exit (delayed): killed {} tool(s)", stopped2.len());
                    if let Ok(mut g) = pids3.lock() { g.extend_from_slice(&stopped2); }
                }
            });
        }))
    };
    let local = tokio::task::LocalSet::new();
    local
        .run_until(renderer::run(renderer_rx, shutdown_rx, config.pause, config.monitors, Arc::clone(&cache), on_fs_exit, per_monitor_rx, outputs_renderer))
        .await
        .context("Renderer error")?;

    cleanup();
    Ok(())
}
