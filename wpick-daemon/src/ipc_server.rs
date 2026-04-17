use std::sync::Arc;
use std::time::UNIX_EPOCH;

use anyhow::Context;
use tokio::io::BufReader;
use tokio::net::UnixListener;
use tokio::sync::Mutex;

use wpick_core::cache::Cache;
use wpick_core::config::{AppDirs, WpickConfig};
use wpick_core::ipc::{recv_command, send_response};
use wpick_core::model::WallpaperInfo;
use wpick_core::{ClientCommand, DaemonResponse};

use crate::state::DaemonState;

// ─── Public entry point ───────────────────────────────────────────────────────

pub async fn run(
    listener: UnixListener,
    state:    Arc<Mutex<DaemonState>>,
    cache:    Arc<Mutex<Cache>>,
    dirs:     AppDirs,
) -> anyhow::Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let dirs  = dirs.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state, cache, dirs).await {
                tracing::warn!("IPC connection closed: {}", e);
            }
        });
    }
}

// ─── Per-connection handler ───────────────────────────────────────────────────

async fn handle_connection(
    stream: tokio::net::UnixStream,
    state:  Arc<Mutex<DaemonState>>,
    cache:  Arc<Mutex<Cache>>,
    dirs:   AppDirs,
) -> anyhow::Result<()> {
    use tokio::io::BufWriter;
    let (r, w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut writer = BufWriter::new(w);

    loop {
        let cmd = match recv_command(&mut reader).await {
            Ok(cmd)  => cmd,
            Err(_)   => break,
        };
        tracing::debug!("Command: {:?}", cmd);

        let response = dispatch(cmd, &state, &cache, &dirs).await;

        if let Err(e) = send_response(&mut writer, &response).await {
            tracing::warn!("Send failed: {}", e);
            break;
        }
    }
    Ok(())
}

// ─── Command dispatcher ───────────────────────────────────────────────────────

async fn dispatch(
    cmd:   ClientCommand,
    state: &Arc<Mutex<DaemonState>>,
    cache: &Arc<Mutex<Cache>>,
    dirs:  &AppDirs,
) -> DaemonResponse {
    match cmd {
        ClientCommand::List => {
            let count = {
                let guard = cache.lock().await;
                match guard.count() {
                    Ok(n)  => n,
                    Err(e) => return DaemonResponse::Error { message: e.to_string() },
                }
            };

            if count == 0 {
                match scan_and_populate(Arc::clone(cache), (*dirs).clone()).await {
                    Ok(items)  => DaemonResponse::WallpaperList { items },
                    Err(e)     => DaemonResponse::Error { message: e.to_string() },
                }
            } else {
                let guard = cache.lock().await;
                match guard.get_all() {
                    Ok(items)  => DaemonResponse::WallpaperList { items },
                    Err(e)     => DaemonResponse::Error { message: e.to_string() },
                }
            }
        }

        ClientCommand::Set { id } => {
            let info = {
                let guard = cache.lock().await;
                match guard.get_by_id(id) {
                    Ok(Some(i)) => i,
                    Ok(None)    => return DaemonResponse::Error {
                        message: format!("Wallpaper {} not found", id),
                    },
                    Err(e)      => return DaemonResponse::Error { message: e.to_string() },
                }
            };

            if !info.is_supported() {
                return DaemonResponse::Error {
                    message: format!("Unsupported type: {}", info.wallpaper_type),
                };
            }

            state.lock().await.set_wallpaper(info);
            DaemonResponse::Ok
        }

        ClientCommand::Volume { level } => {
            state.lock().await.set_volume(level);
            DaemonResponse::Ok
        }

        ClientCommand::Mute => {
            state.lock().await.toggle_mute();
            DaemonResponse::Ok
        }

        ClientCommand::Pause => {
            state.lock().await.set_paused(true);
            DaemonResponse::Ok
        }

        ClientCommand::Resume => {
            state.lock().await.set_paused(false);
            DaemonResponse::Ok
        }

        ClientCommand::Info { id } => {
            let guard = cache.lock().await;
            match guard.get_by_id(id) {
                Ok(Some(item)) => DaemonResponse::WallpaperInfo { item },
                Ok(None)       => DaemonResponse::Error {
                    message: format!("ID {} not found", id),
                },
                Err(e)         => DaemonResponse::Error { message: e.to_string() },
            }
        }

        ClientCommand::Kill => {
            tracing::info!("Kill received — shutting down");
            let _ = state.lock().await.shutdown_tx.send(());
            std::process::exit(0);
        }
    }
}

// ─── Cache population ─────────────────────────────────────────────────────────

async fn scan_and_populate(
    cache: Arc<Mutex<Cache>>,
    dirs:  AppDirs,
) -> anyhow::Result<Vec<WallpaperInfo>> {
    tokio::task::spawn_blocking(move || {
        let config = WpickConfig::load().context("load config")?;
        let wallpaper_dirs = wpick_core::discovery::find_wallpaper_dirs(&config)
            .context("find wallpaper dirs")?;

        tracing::info!("Scanning {} wallpaper dirs", wallpaper_dirs.len());

        let cache_guard = cache.blocking_lock();
        let mut results = Vec::new();

        for wd in wallpaper_dirs {
            match wpick_core::pkg::extract_and_parse(&wd, &dirs.wallpapers_dir) {
                Ok(Some(info)) => {
                    let mtime = std::fs::metadata(wd.path.join("scene.pkg"))
                        .and_then(|m| m.modified())
                        .map(|t| {
                            t.duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                        })
                        .unwrap_or(0);

                    if let Err(e) = cache_guard.upsert(&info, mtime) {
                        tracing::warn!("Cache upsert failed for {}: {}", wd.id, e);
                    }
                    results.push(info);
                }
                Ok(None) => {
                    tracing::debug!("Skipping non-video wallpaper {}", wd.id);
                }
                Err(e) => {
                    tracing::warn!("Failed to process wallpaper {}: {}", wd.id, e);
                }
            }
        }

        Ok::<Vec<WallpaperInfo>, anyhow::Error>(results)
    })
    .await?
}
