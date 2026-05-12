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
            Err(e)   => {
                tracing::debug!("IPC recv error (connection closed): {}", e);
                break;
            }
        };
        tracing::debug!("Command: {:?}", cmd);

        // Scan is handled separately — it streams ScanProgress messages before
        // the final WallpaperList, so it cannot go through the single-response dispatcher.
        if let ClientCommand::Scan = &cmd {
            if let Err(e) = handle_scan(&cache, &dirs, &mut writer).await {
                tracing::warn!("Scan stream error: {}", e);
                break;
            }
            continue;
        }

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
            let guard = cache.lock().await;
            match guard.get_all() {
                Ok(items) => DaemonResponse::WallpaperList { items },
                Err(e)    => DaemonResponse::Error { message: e.to_string() },
            }
        }

        // Scan is handled before dispatch() is called in handle_connection.
        ClientCommand::Scan => unreachable!("Scan is handled by handle_scan"),

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
            let (vol, muted, current_id) = {
                let mut s = state.lock().await;
                s.set_volume(level);
                (s.volume, s.muted, s.current.as_ref().map(|w| w.id))
            };
            save_volume_config(vol, muted);
            DaemonResponse::VolumeState { volume: vol, muted, current_id }
        }

        ClientCommand::Mute => {
            let (vol, muted, current_id) = {
                let mut s = state.lock().await;
                s.toggle_mute();
                (s.volume, s.muted, s.current.as_ref().map(|w| w.id))
            };
            save_volume_config(vol, muted);
            DaemonResponse::VolumeState { volume: vol, muted, current_id }
        }

        ClientCommand::Status => {
            let (vol, muted, current_id) = {
                let s = state.lock().await;
                (s.volume, s.muted, s.current.as_ref().map(|w| w.id))
            };
            DaemonResponse::VolumeState { volume: vol, muted, current_id }
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
            tracing::info!("Kill received — initiating graceful shutdown");
            // Grab the shutdown sender from state before releasing the lock.
            let sd = state.lock().await.shutdown_tx.clone();
            // Delay long enough for the TUI to receive and process the Ok
            // response before the daemon begins tearing down Wayland objects.
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                // Broadcast shutdown — renderer exits cleanly, drops layer_surface
                // and wl_surface, then main() runs cleanup() which removes the socket.
                let _ = sd.send(());
            });
            DaemonResponse::Ok
        }
    }
}

// ─── Config persistence helper ────────────────────────────────────────────────

/// Persist volume and muted state to config.
/// Reads current config from disk once to preserve other fields, then saves.
/// Errors are logged and swallowed — a failed volume save is not fatal.
fn save_volume_config(volume: f32, muted: bool) {
    // Spawn blocking I/O off the async executor.
    tokio::task::spawn_blocking(move || {
        let mut cfg = WpickConfig::load().unwrap_or_default();
        cfg.general.volume = volume;
        cfg.general.muted  = muted;
        if let Err(e) = cfg.save() {
            tracing::warn!("Config save failed: {}", e);
        }
    });
}

// ─── Streaming scan ───────────────────────────────────────────────────────────

/// Handle a Scan command: run the scan in a background task, stream ScanProgress
/// messages to the client as each wallpaper is processed, then send the final
/// WallpaperList (or Error).
async fn handle_scan(
    cache:  &Arc<Mutex<Cache>>,
    dirs:   &AppDirs,
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> anyhow::Result<()> {
    let (prog_tx, mut prog_rx) = tokio::sync::mpsc::channel::<DaemonResponse>(16);
    let scan = tokio::task::spawn(scan_and_populate(
        Arc::clone(cache),
        (*dirs).clone(),
        prog_tx,
    ));

    // Stream progress messages until the sender is dropped (scan finishes).
    while let Some(resp) = prog_rx.recv().await {
        send_response(writer, &resp).await?;
    }

    // Send final result.
    let final_resp = match scan.await {
        Ok(Ok(items)) => DaemonResponse::WallpaperList { items },
        Ok(Err(e))    => DaemonResponse::Error { message: e.to_string() },
        Err(e)        => DaemonResponse::Error { message: format!("scan panic: {e}") },
    };
    send_response(writer, &final_resp).await.map_err(anyhow::Error::from)
}

// ─── Cache population ─────────────────────────────────────────────────────────

async fn scan_and_populate(
    cache:    Arc<Mutex<Cache>>,
    dirs:     AppDirs,
    progress: tokio::sync::mpsc::Sender<DaemonResponse>,
) -> anyhow::Result<Vec<WallpaperInfo>> {
    tokio::task::spawn_blocking(move || {
        let config = WpickConfig::load().context("load config")?;
        let wallpaper_dirs = wpick_core::discovery::find_wallpaper_dirs(&config)
            .context("find wallpaper dirs")?;

        let total = wallpaper_dirs.len();
        tracing::info!("Scanning {} wallpaper dirs", total);

        let cache_guard = cache.blocking_lock();
        let mut results = Vec::new();

        for (i, wd) in wallpaper_dirs.into_iter().enumerate() {
            match wpick_core::pkg::extract_and_parse(&wd, &dirs.wallpapers_dir) {
                Ok(Some(info)) => {
                    let mtime = std::fs::metadata(wd.path.join("project.json"))
                        .and_then(|m| m.modified())
                        .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
                        .unwrap_or(0);

                    if let Err(e) = cache_guard.upsert(&info, mtime) {
                        tracing::warn!("Cache upsert failed for {}: {}", wd.id, e);
                    }
                    results.push(info);
                }
                Ok(None) => tracing::debug!("Skipping non-video wallpaper {}", wd.id),
                Err(e)   => tracing::warn!("Failed to process wallpaper {}: {}", wd.id, e),
            }

            // Ignore send errors — client may have disconnected mid-scan.
            let _ = progress.blocking_send(DaemonResponse::ScanProgress {
                done:  i + 1,
                total,
            });
        }

        let active_ids: Vec<u64> = results.iter().map(|w| w.id).collect();
        if let Err(e) = cache_guard.prune(&active_ids) {
            tracing::warn!("Cache prune failed: {}", e);
        }

        let all = cache_guard.get_all().context("get_all after scan")?;
        Ok::<Vec<WallpaperInfo>, anyhow::Error>(all)
    })
    .await?
}
