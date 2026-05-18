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
    config:   Arc<tokio::sync::Mutex<WpickConfig>>,
    dirty_tx: tokio::sync::watch::Sender<()>,
) -> anyhow::Result<()> {

    loop {
        let (stream, _) = listener.accept().await?;
        let state    = Arc::clone(&state);
        let cache    = Arc::clone(&cache);
        let dirs     = dirs.clone();
        let config   = Arc::clone(&config);
        let dirty_tx = dirty_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state, cache, dirs, config, dirty_tx).await {
                tracing::warn!("IPC connection closed: {}", e);
            }
        });
    }
}

// ─── Per-connection handler ───────────────────────────────────────────────────

async fn handle_connection(
    stream:   tokio::net::UnixStream,
    state:    Arc<Mutex<DaemonState>>,
    cache:    Arc<Mutex<Cache>>,
    dirs:     AppDirs,
    config:   Arc<tokio::sync::Mutex<WpickConfig>>,
    dirty_tx: tokio::sync::watch::Sender<()>,
) -> anyhow::Result<()> {
    use std::os::unix::io::AsRawFd;
    use tokio::io::BufWriter;

    // Capture peer credentials before split — used to authenticate Kill.
    let peer_uid: Option<u32> = unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let ret = libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut libc::c_void,
            &mut len,
        );
        if ret == 0 { Some(cred.uid) } else { None }
    };
    let my_uid: u32 = unsafe { libc::getuid() };

    let (r, w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut writer = BufWriter::new(w);

    loop {
        // 30-second idle timeout — prevents stalled clients from leaking tasks.
        let cmd = match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            recv_command(&mut reader),
        ).await {
            Ok(Ok(cmd))  => cmd,
            Ok(Err(e))   => {
                tracing::debug!("IPC recv error (connection closed): {}", e);
                break;
            }
            Err(_elapsed) => {
                tracing::debug!("IPC recv timeout — closing idle connection");
                break;
            }
        };
        tracing::debug!("Command: {:?}", cmd);

        // Only the daemon owner may issue Kill.
        // If SO_PEERCRED failed (peer_uid == None), we deny — fail-closed.
        if let ClientCommand::Kill = &cmd {
            let allowed = peer_uid.is_some_and(|uid| uid == my_uid);
            if !allowed {
                tracing::warn!("Kill rejected: peer uid={:?} != daemon uid={}", peer_uid, my_uid);
                let _ = send_response(&mut writer, &DaemonResponse::Error {
                    message: "Permission denied: Kill requires daemon owner UID".into(),
                }).await;
                break;
            }
        }

        // Scan is handled separately — it streams ScanProgress messages before
        // the final WallpaperList, so it cannot go through the single-response dispatcher.
        if let ClientCommand::Scan = &cmd {
            if let Err(e) = handle_scan(&cache, &dirs, &mut writer).await {
                tracing::warn!("Scan stream error: {}", e);
                break;
            }
            continue;
        }

        let response = dispatch(cmd, &state, &cache, &config, &dirty_tx).await;

        if let Err(e) = send_response(&mut writer, &response).await {
            tracing::warn!("Send failed: {}", e);
            break;
        }
    }
    Ok(())
}

// ─── Command dispatcher ───────────────────────────────────────────────────────

async fn dispatch(
    cmd:      ClientCommand,
    state:    &Arc<Mutex<DaemonState>>,
    cache:    &Arc<Mutex<Cache>>,
    config:   &Arc<tokio::sync::Mutex<WpickConfig>>,
    dirty_tx: &tokio::sync::watch::Sender<()>,
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

        ClientCommand::Set { id, monitor } => {
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

            match monitor {
                Some(name) => {
                    state.lock().await.set_wallpaper_for_monitor(name.clone(), info);
                    config.lock().await.monitors.entry(name).or_default().wallpaper_id = Some(id);
                }
                None => {
                    state.lock().await.set_wallpaper(info);
                    config.lock().await.last_wallpaper_id = Some(id);
                }
            }
            let _ = dirty_tx.send(());
            DaemonResponse::Ok
        }

        ClientCommand::ListOutputs => {
            let outputs_arc = state.lock().await.outputs.clone();
            let outputs = outputs_arc.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let names       = outputs.iter().map(|o| o.name.clone()).collect();
            let resolutions = outputs.iter().map(|o| (o.width, o.height)).collect();
            DaemonResponse::OutputList { names, resolutions }
        }

        ClientCommand::Volume { level } => {
            let (vol, muted, current_id) = {
                let mut s = state.lock().await;
                s.set_volume(level);
                (s.volume, s.muted, s.current.as_ref().map(|w| w.id))
            };
            {
                let mut cfg = config.lock().await;
                cfg.general.volume = vol;
                cfg.general.muted  = muted;
            }
            let _ = dirty_tx.send(());
            DaemonResponse::VolumeState { volume: vol, muted, current_id }
        }

        ClientCommand::Mute => {
            let (vol, muted, current_id) = {
                let mut s = state.lock().await;
                s.toggle_mute();
                (s.volume, s.muted, s.current.as_ref().map(|w| w.id))
            };
            {
                let mut cfg = config.lock().await;
                cfg.general.volume = vol;
                cfg.general.muted  = muted;
            }
            let _ = dirty_tx.send(());
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

        ClientCommand::SetFit { fit, monitor } => {
            state.lock().await.set_fit(monitor.clone(), fit);
            {
                let mut cfg = config.lock().await;
                match monitor {
                    Some(name) => { cfg.monitors.entry(name).or_default().fit = fit; }
                    None       => { for m in cfg.monitors.values_mut() { m.fit = fit; } }
                }
            }
            let _ = dirty_tx.send(());
            DaemonResponse::Ok
        }

        ClientCommand::SetTimer { ids, interval_secs, shuffle } => {
            if ids.is_empty() || interval_secs == 0 {
                return DaemonResponse::Error {
                    message: "SetTimer: ids must be non-empty and interval_secs > 0".into(),
                };
            }
            // Resolve wallpaper infos for the given IDs.
            let wallpapers: Vec<WallpaperInfo> = {
                let guard = cache.lock().await;
                ids.iter().filter_map(|&id| guard.get_by_id(id).ok().flatten()).collect()
            };
            if wallpapers.is_empty() {
                return DaemonResponse::Error { message: "SetTimer: no valid IDs found in cache".into() };
            }

            let wallpaper_tx = state.lock().await.wallpaper_tx.clone();
            let state_ref    = Arc::clone(state);
            let interval     = std::time::Duration::from_secs(interval_secs);

            // Start from the wallpaper after the currently playing one (if it's in the list).
            let current_id = state.lock().await.current.as_ref().map(|w| w.id);
            let start_idx = current_id
                .and_then(|cid| wallpapers.iter().position(|w| w.id == cid))
                .map(|p| (p + 1) % wallpapers.len())
                .unwrap_or(0);

            // Abort the old timer BEFORE spawning the new one to prevent
            // a brief window where two timers run concurrently.
            state.lock().await.stop_timer();

            let task = tokio::spawn(async move {
                let mut seq: Vec<WallpaperInfo> = wallpapers;
                let mut idx = start_idx;
                loop {
                    // Sleep first — current wallpaper keeps playing for a full interval.
                    tokio::time::sleep(interval).await;

                    if shuffle {
                        fastrand::shuffle(&mut seq);
                        idx = 0;
                    }
                    let wp = seq[idx % seq.len()].clone();
                    tracing::info!("timer: applying '{}'", wp.title);
                    // Update state.current so TUI Status queries reflect the change.
                    state_ref.lock().await.current = Some(wp.clone());
                    let _ = wallpaper_tx.send(Some(wp));
                    idx += 1;
                }
            });

            let stored_ids = ids.clone();
            {
                let mut s = state.lock().await;
                s.timer_task     = Some(task);
                s.timer_interval = interval_secs;
                s.timer_started  = std::time::Instant::now();
                s.timer_ids      = stored_ids;
            }
            DaemonResponse::TimerState {
                active:        true,
                interval_secs,
                remaining_secs: interval_secs,
                ids,
            }
        }

        ClientCommand::StopTimer => {
            state.lock().await.stop_timer();
            DaemonResponse::TimerState { active: false, interval_secs: 0, remaining_secs: 0, ids: vec![] }
        }

        ClientCommand::GetTimerState => {
            let s = state.lock().await;
            let active = s.timer_task.is_some();
            let remaining = if active && s.timer_interval > 0 {
                let elapsed = s.timer_started.elapsed().as_secs() % s.timer_interval;
                s.timer_interval.saturating_sub(elapsed)
            } else { 0 };
            DaemonResponse::TimerState {
                active,
                interval_secs:  s.timer_interval,
                remaining_secs: remaining,
                ids:            s.timer_ids.clone(),
            }
        }

        ClientCommand::RecordPlay { id } => {
            let guard = cache.lock().await;
            if let Err(e) = guard.record_play(id) {
                tracing::warn!("record_play({}): {}", id, e);
            }
            DaemonResponse::Ok
        }

        ClientCommand::GetFrequent { limit } => {
            let guard = cache.lock().await;
            match guard.get_frequent(limit) {
                Ok(items) => DaemonResponse::FrequentList { items },
                Err(e)    => DaemonResponse::Error { message: e.to_string() },
            }
        }

        ClientCommand::SetWorkspaceWallpaper { workspace, id } => {
            // Validate non-zero IDs against the cache (consistent with ClientCommand::Set).
            if id != 0 {
                let guard = cache.lock().await;
                match guard.get_by_id(id) {
                    Ok(Some(_)) => {}
                    Ok(None)    => return DaemonResponse::Error {
                        message: format!("Wallpaper {} not found", id),
                    },
                    Err(e)      => return DaemonResponse::Error { message: e.to_string() },
                }
            }
            let new_map = {
                let mut cfg = config.lock().await;
                if id == 0 {
                    cfg.workspace_wallpapers.remove(&workspace);
                } else {
                    cfg.workspace_wallpapers.insert(workspace.clone(), id);
                }
                cfg.workspace_wallpapers.clone()
            };
            let _ = state.lock().await.workspace_wallpaper_tx.send(new_map.clone());
            let _ = dirty_tx.send(());
            DaemonResponse::WorkspaceMap { map: new_map }
        }

        ClientCommand::GetWorkspaceMap => {
            let map = config.lock().await.workspace_wallpapers.clone();
            DaemonResponse::WorkspaceMap { map }
        }

        ClientCommand::Kill => {
            tracing::info!("Kill received — initiating graceful shutdown");
            let sd = state.lock().await.shutdown_tx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let _ = sd.send(());
            });
            DaemonResponse::Ok
        }
    }
}

// ─── Config persistence ────────────────────────────────────────────────────────

/// Debounced config writer — coalesces rapid mutations into a single disk write.
///
/// Wakes on `dirty_rx` signal, waits 500 ms for additional changes, then saves.
/// On shutdown signal: flushes immediately without debounce.
pub async fn run_config_writer(
    config:       Arc<tokio::sync::Mutex<WpickConfig>>,
    mut dirty_rx: tokio::sync::watch::Receiver<()>,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    loop {
        tokio::select! {
            result = dirty_rx.changed() => {
                if result.is_err() { return; } // sender dropped — writer is done
                // Debounce: coalesce all writes within a 500 ms window.
                while let Ok(Ok(_)) = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    dirty_rx.changed(),
                ).await {}
                let cfg = config.lock().await.clone();
                if let Err(e) = cfg.save_preserving_tui() {
                    tracing::warn!("Config debounced save failed: {}", e);
                }
            }
            _ = shutdown.recv() => {
                // Shutdown: flush immediately, no debounce.
                let cfg = config.lock().await.clone();
                if let Err(e) = cfg.save_preserving_tui() {
                    tracing::warn!("Config flush on shutdown failed: {}", e);
                }
                return;
            }
        }
    }
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
    // Abort the scan task if the client disconnects mid-stream to prevent
    // cache-lock pile-up when many clients issue Scan and disconnect.
    while let Some(resp) = prog_rx.recv().await {
        if let Err(e) = send_response(writer, &resp).await {
            scan.abort();
            return Err(anyhow::Error::from(e));
        }
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

        // ── Phase 1: Steam Workshop wallpapers ────────────────────────────────
        let wallpaper_dirs = wpick_core::discovery::find_wallpaper_dirs(&config)
            .context("find wallpaper dirs")?;

        // ── Phase 2: local extra_dirs ─────────────────────────────────────────
        let local_infos = wpick_core::discovery::find_local_video_files(
            &config.paths.extra_dirs,
        );

        let workshop_total = wallpaper_dirs.len();
        let total          = workshop_total + local_infos.len();
        tracing::info!(
            workshop = workshop_total,
            local    = local_infos.len(),
            "Scanning wallpapers"
        );

        // Parse workshop items — collect (info, mtime) pairs, emit progress per item.
        let mut batch: Vec<(WallpaperInfo, u64)> = Vec::with_capacity(total);
        let mut done: usize = 0;

        for wd in wallpaper_dirs {
            match wpick_core::pkg::extract_and_parse(&wd, &dirs.wallpapers_dir) {
                Ok(Some(info)) => {
                    let mtime = std::fs::metadata(wd.path.join("project.json"))
                        .and_then(|m| m.modified())
                        .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
                        .unwrap_or(0);
                    batch.push((info, mtime));
                }
                Ok(None) => tracing::debug!("Skipping non-video wallpaper {}", wd.id),
                Err(e)   => tracing::warn!("Failed to process wallpaper {}: {}", wd.id, e),
            }
            done += 1;
            let _ = progress.blocking_send(DaemonResponse::ScanProgress { done, total });
        }

        // Parse local files — same pattern.
        for info in local_infos {
            let mtime = std::fs::metadata(&info.file_path)
                .and_then(|m| m.modified())
                .map(|t| t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs())
                .unwrap_or(0);
            batch.push((info, mtime));
            done += 1;
            let _ = progress.blocking_send(DaemonResponse::ScanProgress { done, total });
        }

        // Single lock acquisition: batch upsert + prune + get_all in one transaction.
        let guard = cache.blocking_lock();
        if let Err(e) = guard.upsert_batch(&batch) {
            tracing::warn!("Batch upsert failed: {}", e);
        }
        let active_ids: Vec<u64> = batch.iter().map(|(w, _)| w.id).collect();
        if let Err(e) = guard.prune(&active_ids) {
            tracing::warn!("Cache prune failed: {}", e);
        }
        let all = guard.get_all().context("get_all after scan")?;
        Ok::<Vec<WallpaperInfo>, anyhow::Error>(all)
    })
    .await?
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::sync::{self, watch};
    use wpick_core::config::{FitMode, WpickConfig};
    use wpick_core::ipc::{ClientCommand, DaemonResponse};

    use crate::state::{DaemonState, OutputInfo};

    fn make_state() -> (Arc<sync::Mutex<DaemonState>>, Arc<sync::Mutex<WpickConfig>>, sync::watch::Sender<()>) {
        let (wallpaper_tx, _)     = watch::channel(None);
        let (volume_tx, _)        = watch::channel((0.8f32, false));
        let (shutdown_tx, _)      = sync::broadcast::channel(1);
        let (per_monitor_tx, _)   = watch::channel(HashMap::new());
        let (fit_tx, _)           = watch::channel(("*".to_owned(), FitMode::Fill));
        let (workspace_wp_tx, _)  = watch::channel(HashMap::new());
        let (dirty_tx, _)         = watch::channel(());

        let state = Arc::new(sync::Mutex::new(DaemonState {
            current:              None,
            volume:               0.8,
            muted:                false,
            wallpaper_tx,
            volume_tx,
            shutdown_tx:          shutdown_tx.clone(),
            per_monitor_tx,
            fit_tx,
            outputs:              Arc::new(Mutex::new(vec![
                OutputInfo { name: "DP-1".into(), width: 2560, height: 1440 },
            ])),
            timer_task:           None,
            timer_interval:       0,
            timer_started:        std::time::Instant::now(),
            timer_ids:            Vec::new(),
            workspace_wallpaper_tx: workspace_wp_tx,
        }));
        let config = Arc::new(sync::Mutex::new(WpickConfig::default()));
        (state, config, dirty_tx)
    }

    #[tokio::test]
    async fn test_dispatch_status_returns_volume_state() {
        let (state, config, dirty_tx) = make_state();
        let cache = Arc::new(sync::Mutex::new(
            wpick_core::cache::Cache::open_in_memory().expect("in-memory cache"),
        ));
        let resp = dispatch(ClientCommand::Status, &state, &cache, &config, &dirty_tx).await;
        assert!(
            matches!(resp, DaemonResponse::VolumeState { volume, muted, .. }
                if (volume - 0.8).abs() < f32::EPSILON && !muted),
            "expected VolumeState, got {:?}", resp
        );
    }

    #[tokio::test]
    async fn test_dispatch_mute_toggles_state() {
        let (state, config, dirty_tx) = make_state();
        let cache = Arc::new(sync::Mutex::new(
            wpick_core::cache::Cache::open_in_memory().expect("in-memory cache"),
        ));
        let resp = dispatch(ClientCommand::Mute, &state, &cache, &config, &dirty_tx).await;
        assert!(matches!(resp, DaemonResponse::VolumeState { muted: true, .. }), "{:?}", resp);
        // Second toggle restores
        let resp2 = dispatch(ClientCommand::Mute, &state, &cache, &config, &dirty_tx).await;
        assert!(matches!(resp2, DaemonResponse::VolumeState { muted: false, .. }), "{:?}", resp2);
    }

    #[tokio::test]
    async fn test_dispatch_list_outputs() {
        let (state, config, dirty_tx) = make_state();
        let cache = Arc::new(sync::Mutex::new(
            wpick_core::cache::Cache::open_in_memory().expect("in-memory cache"),
        ));
        let resp = dispatch(ClientCommand::ListOutputs, &state, &cache, &config, &dirty_tx).await;
        match resp {
            DaemonResponse::OutputList { names, resolutions } => {
                assert_eq!(names, vec!["DP-1"]);
                assert_eq!(resolutions, vec![(2560, 1440)]);
            }
            other => panic!("expected OutputList, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_dispatch_get_workspace_map_empty() {
        let (state, config, dirty_tx) = make_state();
        let cache = Arc::new(sync::Mutex::new(
            wpick_core::cache::Cache::open_in_memory().expect("in-memory cache"),
        ));
        let resp = dispatch(ClientCommand::GetWorkspaceMap, &state, &cache, &config, &dirty_tx).await;
        assert!(matches!(resp, DaemonResponse::WorkspaceMap { ref map } if map.is_empty()), "{:?}", resp);
    }

    #[tokio::test]
    async fn test_dispatch_set_workspace_wallpaper_clear() {
        let (state, config, dirty_tx) = make_state();
        let cache = Arc::new(sync::Mutex::new(
            wpick_core::cache::Cache::open_in_memory().expect("in-memory cache"),
        ));
        // Set workspace "1" → id 0 means clear (no entry expected)
        let resp = dispatch(
            ClientCommand::SetWorkspaceWallpaper { workspace: "1".into(), id: 0 },
            &state, &cache, &config, &dirty_tx,
        ).await;
        assert!(matches!(resp, DaemonResponse::WorkspaceMap { ref map } if map.is_empty()), "{:?}", resp);
    }

    #[tokio::test]
    async fn test_dispatch_volume_clamps_to_range() {
        let (state, config, dirty_tx) = make_state();
        let cache = Arc::new(sync::Mutex::new(
            wpick_core::cache::Cache::open_in_memory().expect("in-memory cache"),
        ));
        // Volume above 1.0 should be clamped to 1.0
        let resp = dispatch(ClientCommand::Volume { level: 2.5 }, &state, &cache, &config, &dirty_tx).await;
        assert!(matches!(resp, DaemonResponse::VolumeState { volume, .. } if (volume - 1.0).abs() < f32::EPSILON),
            "{:?}", resp);
    }
}
