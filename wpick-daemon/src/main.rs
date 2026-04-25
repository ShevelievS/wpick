mod audio;
mod ducking;
mod ipc_server;
mod renderer;
mod state;
mod video;

use std::sync::Arc;

use anyhow::Context;
use tokio::sync::Mutex;

use wpick_core::cache::Cache;
use wpick_core::config::WpickConfig;
use wpick_core::model::WallpaperInfo;

use crate::state::DaemonState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Config + dirs
    let config = WpickConfig::load().context("Failed to load config")?;
    let dirs   = config.app_dirs().context("Failed to resolve app dirs")?;

    // 2. File logging
    let file_appender = tracing_appender::rolling::daily(&dirs.log_dir, "wpick.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("wpick_daemon=info".parse()?)
                .add_directive("wpick_core=info".parse()?),
        )
        .init();

    tracing::info!("wpick-daemon starting");

    // 3. Open database
    let cache = Arc::new(Mutex::new(
        Cache::open(&dirs.db_path).context("Failed to open cache DB")?,
    ));

    // 4. Watch/broadcast channels
    let (renderer_tx, renderer_rx) = tokio::sync::watch::channel(None::<WallpaperInfo>);
    let (audio_tx,    audio_rx)    = tokio::sync::watch::channel(None::<WallpaperInfo>);
    let (volume_tx,   volume_rx)   = tokio::sync::watch::channel(
        (config.general.volume, config.general.muted),
    );
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);

    // 5. DaemonState
    let state = Arc::new(Mutex::new(DaemonState {
        current:     None,
        volume:      config.general.volume,
        muted:       config.general.muted,
        renderer_tx,
        audio_tx,
        volume_tx,
        shutdown_tx: shutdown_tx.clone(),
    }));

    // 6 + 7. Atomic socket bind: try to bind first, then handle EADDRINUSE.
    // This avoids the TOCTOU race of exists()→remove()→bind(). (C-3)
    let socket_path = dirs.socket_path.clone();
    let listener = match tokio::net::UnixListener::bind(&socket_path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Socket file exists — check if a live daemon owns it
            if tokio::net::UnixStream::connect(&socket_path).await.is_ok() {
                anyhow::bail!(
                    "Another wpick-daemon is already running at {:?}",
                    socket_path
                );
            }
            // Stale socket (process gone) — remove and rebind
            tracing::info!("Removing stale socket at {:?}", socket_path);
            std::fs::remove_file(&socket_path).context("Failed to remove stale socket")?;
            tokio::net::UnixListener::bind(&socket_path)
                .context("Failed to bind socket after removing stale one")?
        }
        Err(e) => return Err(e).context("Failed to bind Unix socket"),
    };
    tracing::info!("Listening on {:?}", socket_path);

    // 8. Cleanup helper
    let cleanup = {
        let sp = socket_path.clone();
        move || { let _ = std::fs::remove_file(&sp); }
    };

    // 9. Signal handlers
    {
        let sp = socket_path.clone();
        let sd = shutdown_tx.clone();
        tokio::spawn(async move {
            tokio::signal::ctrl_c().await.ok();
            tracing::info!("SIGINT — shutting down");
            let _ = sd.send(());
            let _ = std::fs::remove_file(&sp);
            std::process::exit(0);
        });
    }
    #[cfg(unix)]
    {
        let sp = socket_path.clone();
        let sd = shutdown_tx.clone();
        tokio::spawn(async move {
            use tokio::signal::unix::{signal, SignalKind};
            if let Ok(mut sig) = signal(SignalKind::terminate()) {
                sig.recv().await;
                tracing::info!("SIGTERM — shutting down");
                let _ = sd.send(());
                let _ = std::fs::remove_file(&sp);
                std::process::exit(0);
            }
        });
    }

    // 10. IPC server task
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

    // 11. Audio task — dedicated OS thread (rodio OutputStream is !Send)
    // ducking::start() is called here so PulseAudio init doesn't block the
    // main thread and a slow/absent PA doesn't delay IPC or renderer startup
    {
        std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || {
                let duck = ducking::start();
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("audio runtime");
                if let Err(e) = rt.block_on(audio::run(duck, audio_rx, volume_rx)) {
                    tracing::error!("Audio task: {}", e);
                }
            })
            .context("Failed to spawn audio thread")?;
    }

    // 12. Renderer — must run on this thread (Wayland is !Send)
    tracing::info!("Starting renderer");
    eprintln!("DEBUG: about to start renderer");
    let local = tokio::task::LocalSet::new();
    local
        .run_until(renderer::run(renderer_rx, shutdown_rx))
        .await
        .context("Renderer error")?;

    cleanup();
    Ok(())
}