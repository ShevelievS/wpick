mod audio;
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

    // 2. File logging (daemon has no terminal)
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
    let (renderer_tx, _renderer_rx) = tokio::sync::watch::channel(None::<WallpaperInfo>);
    let (audio_tx,    _audio_rx)    = tokio::sync::watch::channel(None::<WallpaperInfo>);
    let (volume_tx,   _volume_rx)   = tokio::sync::watch::channel(
        (config.general.volume, config.general.muted),
    );
    let (pause_tx, _pause_rx)               = tokio::sync::watch::channel(false);
    let (shutdown_tx, mut shutdown_rx)       = tokio::sync::broadcast::channel::<()>(1);

    // 5. DaemonState
    let state = Arc::new(Mutex::new(DaemonState {
        current:     None,
        volume:      config.general.volume,
        muted:       config.general.muted,
        paused:      false,
        renderer_tx,
        audio_tx,
        volume_tx,
        pause_tx,
        shutdown_tx: shutdown_tx.clone(),
    }));

    // 6. Stale socket check
    let socket_path = dirs.socket_path.clone();
    if socket_path.exists() {
        if tokio::net::UnixStream::connect(&socket_path).await.is_ok() {
            anyhow::bail!(
                "Another wpick-daemon is already running at {:?}",
                socket_path
            );
        }
        std::fs::remove_file(&socket_path).context("Failed to remove stale socket")?;
    }

    // 7. Bind socket
    let listener = tokio::net::UnixListener::bind(&socket_path)
        .context("Failed to bind Unix socket")?;
    tracing::info!("Listening on {:?}", socket_path);

    // 8. Signal handlers
    let cleanup = {
        let sp = socket_path.clone();
        move || {
            let _ = std::fs::remove_file(&sp);
            tracing::info!("Socket removed");
        }
    };

    // SIGINT
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

    // SIGTERM
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

    // 9. IPC server task
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

    // 10. Renderer stub — wait for shutdown signal
    tracing::info!("Renderer not started (Phase 4)");
    let _ = shutdown_rx.recv().await;

    cleanup();
    Ok(())
}
