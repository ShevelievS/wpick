// Wallpaper renderer — wl_shm CPU path, Hyprland-aware surface recovery.
//
// Surface recovery after fullscreen:
//   A) Still alive → nudge (re-set layer props + commit) → fresh Configure.
//      Fallback: if no Configure within 2 s → path B.
//   B) Closed during fullscreen → recreate after fullscreen exits.

use std::os::unix::io::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::sync::{broadcast, watch};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_output, wl_region,
        wl_registry, wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};
use std::collections::HashMap;

use wpick_core::cache::Cache;
use wpick_core::config::{FitMode, MonitorConfig, PauseConfig};
use wpick_core::model::WallpaperInfo;

use crate::hw_decode::HwDecoder;
use crate::video::VideoDecoder;

// ─── Fullscreen monitors (Hyprland + Sway) ───────────────────────────────────

/// Shared workspace-change sender: the fs-mon thread holds an `Arc` to this and
/// always sends to whatever `SyncSender` is currently installed.  The render loop
/// swaps in a fresh sender (and keeps the matching receiver) on each reinit.
type WorkspaceSenderSlot = Arc<std::sync::Mutex<Option<std::sync::mpsc::SyncSender<String>>>>;

fn start_fullscreen_monitor(
    on_exit:             Option<Arc<dyn Fn() + Send + Sync>>,
    reassert_flag:       Arc<AtomicBool>,
    workspace_sender_slot: WorkspaceSenderSlot,
) -> Arc<AtomicBool> {
    let flag  = Arc::new(AtomicBool::new(false));
    let flag2 = Arc::clone(&flag);

    if let Ok(sig) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        let cb   = on_exit.clone();
        let rf   = Arc::clone(&reassert_flag);
        let slot = Arc::clone(&workspace_sender_slot);
        std::thread::Builder::new().name("fs-mon".into()).spawn(move || {
            hyprland_fullscreen_loop(flag2, sig, cb, rf, slot);
        }).ok();
    } else if std::env::var("SWAYSOCK").is_ok() {
        let slot = Arc::clone(&workspace_sender_slot);
        std::thread::Builder::new().name("fs-mon".into()).spawn(move || {
            sway_fullscreen_loop(flag2, on_exit, slot);
        }).ok();
    } else {
        tracing::info!("unknown compositor — fullscreen detection off");
    }

    flag
}

/// Query the active workspace's fullscreen state via Hyprland socket1.
/// Returns true if the current workspace has a fullscreen window.
fn hyprland_query_fullscreen(sock1: &str) -> bool {
    use std::io::{Read, Write};
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(sock1) else { return false; };
    if stream.write_all(b"j/activeworkspace").is_err() { return false; }
    let mut resp = String::new();
    if stream.read_to_string(&mut resp).is_err() { return false; }
    // "hasfullscreen": true  (Hyprland >= 0.30 JSON field, may have space after colon)
    resp.contains("\"hasfullscreen\":true") || resp.contains("\"hasfullscreen\": true")
}

fn hyprland_fullscreen_loop(
    flag:                  Arc<AtomicBool>,
    sig:                   String,
    on_exit:               Option<Arc<dyn Fn() + Send + Sync>>,
    reassert_flag:         Arc<AtomicBool>,
    workspace_sender_slot: WorkspaceSenderSlot,
) {
    // Hyprland >= 0.30 moved the socket from /tmp/hypr/ to $XDG_RUNTIME_DIR/hypr/.
    let xdg = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path_xdg = format!("{}/hypr/{}/.socket2.sock", xdg, sig);
    let path_tmp = format!("/tmp/hypr/{}/.socket2.sock", sig);
    let path = if std::path::Path::new(&path_xdg).exists() { path_xdg } else { path_tmp };
    let sock1_xdg = format!("{}/hypr/{}/.socket.sock", xdg, sig);
    let sock1_tmp = format!("/tmp/hypr/{}/.socket.sock", sig);
    let sock1 = if std::path::Path::new(&sock1_xdg).exists() { sock1_xdg } else { sock1_tmp };
    tracing::info!("hyprland fullscreen monitor: {}", path);
    let mut prev_active = false;
    let mut backoff_secs: u64 = 2;
    loop {
        if let Ok(stream) = std::os::unix::net::UnixStream::connect(&path) {
            backoff_secs = 2;

            // Re-query fullscreen state immediately on (re)connect.
            // If fullscreen>>0 was sent while we were disconnected (during backoff or
            // a Hyprland restart) the event is permanently lost and the flag would stay
            // true forever, freezing the render loop indefinitely.  An explicit query
            // after every connect makes us resilient to any missed transition event.
            let reconnect_active = hyprland_query_fullscreen(&sock1);
            if prev_active && !reconnect_active {
                if let Some(ref cb) = on_exit { cb(); }
            }
            prev_active = reconnect_active;
            flag.store(reconnect_active, Ordering::Relaxed);
            tracing::info!("hyprland socket2 connected — fullscreen={}", reconnect_active);

            // 15-second read timeout: if no event arrives within 15 s we re-query
            // the real fullscreen state.  This is the safety net that corrects a
            // stuck flag even while the socket stays connected (e.g. user closes a
            // fullscreen window during a long idle period with no other events).
            stream.set_read_timeout(Some(Duration::from_secs(15))).ok();

            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(stream);
            'event: loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break 'event, // EOF / disconnected
                    Err(e) if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {
                        // No event in 15 s — re-query to correct any stale flag.
                        let active = hyprland_query_fullscreen(&sock1);
                        if prev_active && !active {
                            if let Some(ref cb) = on_exit { cb(); }
                        }
                        prev_active = active;
                        flag.store(active, Ordering::Relaxed);
                        continue 'event;
                    }
                    Err(e) => { tracing::debug!("hyprland ipc: {}", e); break 'event; }
                    Ok(_)  => {}
                }
                let l = line.trim_end();
                if l.starts_with("fullscreen>>") {
                    let p = l.trim_start_matches("fullscreen>>");
                    let active = !p.starts_with('0') && !p.contains(",0");
                    if prev_active && !active {
                        if let Some(ref cb) = on_exit { cb(); }
                    }
                    prev_active = active;
                    flag.store(active, Ordering::Relaxed);
                    tracing::info!("hyprland fullscreen → {}", active);
                } else if l.starts_with("lockguard>>0") {
                    // Screen unlocked — reassert wpick surface so it ends up on top of
                    // QuickShell's background layer which re-renders on unlock.
                    tracing::info!("hyprland: screen unlocked — scheduling surface reassert");
                    let rf = Arc::clone(&reassert_flag);
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(1));
                        rf.store(true, Ordering::Relaxed);
                    });
                } else if l.starts_with("workspacev2>>") {
                    // Hyprland v0.40+: "workspacev2>>id,name"
                    // workspace>> does not resend fullscreen>> — re-query real state.
                    let active = hyprland_query_fullscreen(&sock1);
                    flag.store(active, Ordering::Relaxed);
                    let rest = l.trim_start_matches("workspacev2>>");
                    if let Some((_, ws_name)) = rest.split_once(',') {
                        let ws_name = ws_name.to_owned();
                        tracing::info!("workspacev2 '{}' → fullscreen={}", ws_name, active);
                        if let Ok(guard) = workspace_sender_slot.lock() {
                            if let Some(ref tx) = *guard { let _ = tx.try_send(ws_name); }
                        }
                    }
                } else if l.starts_with("workspace>>") {
                    // workspace>> does not resend fullscreen>> — re-query real state.
                    let active = hyprland_query_fullscreen(&sock1);
                    flag.store(active, Ordering::Relaxed);
                    let ws_name = l.trim_start_matches("workspace>>").to_owned();
                    tracing::info!("workspace change '{}' → fullscreen={}", ws_name, active);
                    if let Ok(guard) = workspace_sender_slot.lock() {
                        if let Some(ref tx) = *guard { let _ = tx.try_send(ws_name); }
                    }
                } else if l.starts_with("focusedmon>>") {
                    let active = hyprland_query_fullscreen(&sock1);
                    flag.store(active, Ordering::Relaxed);
                    let rest = l.trim_start_matches("focusedmon>>");
                    if let Some((_, ws_name)) = rest.split_once(',') {
                        let ws_name = ws_name.to_owned();
                        tracing::info!("focusedmon: workspace '{}' → fullscreen={}", ws_name, active);
                        if let Ok(guard) = workspace_sender_slot.lock() {
                            if let Some(ref tx) = *guard { let _ = tx.try_send(ws_name); }
                        }
                    }
                }
            }
        }
        tracing::debug!("hyprland ipc disconnected — reconnecting in {}s", backoff_secs);
        std::thread::sleep(Duration::from_secs(backoff_secs));
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

/// Query Sway for current fullscreen state via GET_TREE (type=4).
/// Returns true if any node in the tree has fullscreen_mode > 0.
fn sway_query_fullscreen(sock: &str) -> bool {
    use std::io::{Read, Write};
    const MAGIC: &[u8] = b"i3-ipc";
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(sock) else { return false };
    let mut msg = [0u8; 14];
    msg[..6].copy_from_slice(MAGIC);
    // payload_len = 0, msg_type = 4 (GET_TREE)
    msg[10..14].copy_from_slice(&4u32.to_le_bytes());
    if stream.write_all(&msg).is_err() { return false; }
    let mut hdr = [0u8; 14];
    if stream.read_exact(&mut hdr).is_err() { return false; }
    let body_len = u32::from_le_bytes(hdr[6..10].try_into().unwrap_or_default()) as usize;
    let mut body = vec![0u8; body_len];
    if stream.read_exact(&mut body).is_err() { return false; }
    let s = std::str::from_utf8(&body).unwrap_or("");
    s.contains("\"fullscreen_mode\":1") || s.contains("\"fullscreen_mode\":2")
}

/// Sway native IPC — subscribe to window and workspace events.
///
/// Protocol: 6-byte magic "i3-ipc" + u32 LE payload_len + u32 LE msg_type.
/// SUBSCRIBE = type 2; window events = type 0x80000003; workspace events = type 0x80000000.
fn sway_fullscreen_loop(
    flag:                  Arc<AtomicBool>,
    on_exit:               Option<Arc<dyn Fn() + Send + Sync>>,
    workspace_sender_slot: WorkspaceSenderSlot,
) {
    use std::io::{Read, Write};
    const MAGIC: &[u8] = b"i3-ipc";
    const SUBSCRIBE: u32 = 2;
    const EV_WINDOW: u32    = 0x8000_0003;
    const EV_WORKSPACE: u32 = 0x8000_0000;

    let sock = match std::env::var("SWAYSOCK") {
        Ok(s) => s,
        Err(_) => return,
    };

    let mut prev_active = false;
    let mut backoff_secs: u64 = 2;
    loop {
        match std::os::unix::net::UnixStream::connect(&sock) {
            Ok(mut stream) => {
                backoff_secs = 2;

                // Re-query fullscreen state on (re)connect — same race as Hyprland:
                // if the socket was down when fullscreen exited, we'd never see the
                // event and is_paused would be stuck true forever.
                let reconnect_active = sway_query_fullscreen(&sock);
                if prev_active && !reconnect_active {
                    if let Some(ref cb) = on_exit { cb(); }
                }
                prev_active = reconnect_active;
                flag.store(reconnect_active, Ordering::Relaxed);
                tracing::info!("sway socket connected — fullscreen={}", reconnect_active);

                let payload = b"[\"window\",\"workspace\"]";
                let mut msg = Vec::with_capacity(14 + payload.len());
                msg.extend_from_slice(MAGIC);
                msg.extend_from_slice(&(payload.len() as u32).to_le_bytes());
                msg.extend_from_slice(&SUBSCRIBE.to_le_bytes());
                msg.extend_from_slice(payload);

                if stream.write_all(&msg).is_err() { continue; }

                // drain subscribe ACK
                let mut hdr = [0u8; 14];
                if stream.read_exact(&mut hdr).is_err() { continue; }
                let ack_len = u32::from_le_bytes(hdr[6..10].try_into().unwrap_or_default()) as usize;
                let mut ack = vec![0u8; ack_len];
                if stream.read_exact(&mut ack).is_err() { continue; }

                // 15-second read timeout — periodic re-query safety net.
                stream.set_read_timeout(Some(Duration::from_secs(15))).ok();

                // event loop
                'sway: loop {
                    match stream.read_exact(&mut hdr) {
                        Ok(()) => {}
                        Err(e) if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {
                            let active = sway_query_fullscreen(&sock);
                            if prev_active && !active {
                                if let Some(ref cb) = on_exit { cb(); }
                            }
                            prev_active = active;
                            flag.store(active, Ordering::Relaxed);
                            continue 'sway;
                        }
                        Err(_) => break 'sway,
                    }
                    let body_len = u32::from_le_bytes(hdr[6..10].try_into().unwrap_or_default()) as usize;
                    let msg_type = u32::from_le_bytes(hdr[10..14].try_into().unwrap_or_default());
                    let mut body = vec![0u8; body_len];
                    if stream.read_exact(&mut body).is_err() { break 'sway; }

                    if msg_type == EV_WINDOW {
                        let s = std::str::from_utf8(&body).unwrap_or("");
                        if s.contains("\"change\":\"fullscreen_mode\"") {
                            let active = s.contains("\"fullscreen_mode\":1")
                                      || s.contains("\"fullscreen_mode\":2");
                            if prev_active && !active {
                                if let Some(ref cb) = on_exit { cb(); }
                            }
                            prev_active = active;
                            flag.store(active, Ordering::Relaxed);
                            tracing::debug!("sway fullscreen → {}", active);
                        }
                    } else if msg_type == EV_WORKSPACE {
                        let s = std::str::from_utf8(&body).unwrap_or("");
                        if s.contains("\"change\":\"focus\"") {
                            if let Some(cur_pos) = s.find("\"current\"") {
                                let after = &s[cur_pos..];
                                if let Some(name_pos) = after.find("\"name\":\"") {
                                    let start = name_pos + 8;
                                    let slice  = &after[start..];
                                    if let Some(end) = slice.find('"') {
                                        let ws_name = slice[..end].to_owned();
                                        tracing::debug!("sway workspace focus → '{}'", ws_name);
                                        if let Ok(guard) = workspace_sender_slot.lock() {
                                            if let Some(ref tx) = *guard { let _ = tx.try_send(ws_name); }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                tracing::debug!("sway ipc disconnected — reconnecting in {}s", backoff_secs);
            }
            Err(e) => tracing::debug!("sway ipc connect: {} — reconnecting in {}s", e, backoff_secs),
        }
        std::thread::sleep(Duration::from_secs(backoff_secs));
        backoff_secs = (backoff_secs * 2).min(30);
    }
}

// ─── Pause monitors ───────────────────────────────────────────────────────────

struct PauseMonitors {
    on_fullscreen: bool,
    fullscreen:    Arc<AtomicBool>,
    battery:       Arc<AtomicBool>,
    lid:           Arc<AtomicBool>,
}

impl PauseMonitors {
    fn is_paused(&self) -> bool {
        (self.on_fullscreen && self.fullscreen.load(Ordering::Relaxed))
            || self.battery.load(Ordering::Relaxed)
            || self.lid.load(Ordering::Relaxed)
    }
}

fn start_battery_monitor() -> Arc<AtomicBool> {
    let flag  = Arc::new(AtomicBool::new(is_on_battery()));
    let flag2 = Arc::clone(&flag);
    std::thread::Builder::new().name("bat-mon".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(30));
            flag2.store(is_on_battery(), Ordering::Relaxed);
        }
    }).ok();
    flag
}

fn is_on_battery() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else { return false; };
    for entry in entries.flatten() {
        let path = entry.path().join("status");
        if let Ok(s) = std::fs::read_to_string(&path) {
            if s.trim() == "Discharging" { return true; }
        }
    }
    false
}

fn start_lid_monitor() -> Arc<AtomicBool> {
    let flag  = Arc::new(AtomicBool::new(is_lid_closed()));
    let flag2 = Arc::clone(&flag);
    std::thread::Builder::new().name("lid-mon".into()).spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            flag2.store(is_lid_closed(), Ordering::Relaxed);
        }
    }).ok();
    flag
}

fn is_lid_closed() -> bool {
    let Ok(entries) = std::fs::read_dir("/proc/acpi/button/lid") else { return false; };
    for entry in entries.flatten() {
        let path = entry.path().join("state");
        if let Ok(s) = std::fs::read_to_string(&path) {
            if s.contains("closed") { return true; }
        }
    }
    false
}

// ─── Wayland state ────────────────────────────────────────────────────────────

/// A discovered wl_output, kept alive for the lifetime of the renderer.
struct OutputEntry {
    _wl_output: wl_output::WlOutput,
    name:       String,
    width:      u32,
    height:     u32,
}

/// Per-surface Wayland events written by Dispatch handlers,
/// read by the render loop.  Indexed by the `usize` user-data
/// attached to each ZwlrLayerSurfaceV1 / WlCallback at creation time.
struct SurfaceEvent {
    configured:       bool,
    configure_serial: u32,
    surf_width:       u32,
    surf_height:      u32,
    needs_ack:        bool,
    closed:           bool,
    /// Set true by `wl_callback::Done`; cleared by the render loop before each commit.
    frame_ready:      bool,
}

impl Default for SurfaceEvent {
    fn default() -> Self {
        Self {
            configured: false, configure_serial: 0,
            surf_width: 0, surf_height: 0,
            needs_ack: false, closed: false,
            frame_ready: true, // allow first frame immediately
        }
    }
}

#[derive(Default)]
struct WaylandState {
    compositor:  Option<wl_compositor::WlCompositor>,
    layer_shell: Option<ZwlrLayerShellV1>,
    shm:         Option<wl_shm::WlShm>,
    /// All discovered wl_outputs; index matches the usize user-data on each WlOutput.
    outputs:     Vec<OutputEntry>,
    /// Per-surface event state; index matches the usize user-data on each layer surface.
    surf_ev:     Vec<SurfaceEvent>,
}

// ─── Dispatch impls ───────────────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(state: &mut Self, registry: &wl_registry::WlRegistry, event: wl_registry::Event,
             _: &(), _: &Connection, qh: &QueueHandle<WaylandState>) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor"       => { state.compositor  = Some(registry.bind(name, version.min(4), qh, ())); }
                "zwlr_layer_shell_v1" => { state.layer_shell = Some(registry.bind(name, version.min(4), qh, ())); }
                "wl_shm"              => { state.shm         = Some(registry.bind(name, version.min(1), qh, ())); }
                "wl_output" => {
                    // user-data = index into state.outputs
                    let idx = state.outputs.len();
                    let o = registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, idx);
                    state.outputs.push(OutputEntry {
                        _wl_output: o,
                        name:   String::new(),
                        width:  1920,
                        height: 1080,
                    });
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_compositor::WlCompositor, _: wl_compositor::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_region::WlRegion, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_region::WlRegion, _: wl_region::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_surface::WlSurface, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_surface::WlSurface, _: wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_shm::WlShm, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<wl_shm_pool::WlShmPool, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

/// wl_buffer user-data is the `in_use` flag of the owning ShmSlot.
/// Release → compositor no longer reads this buffer → slot is free.
impl Dispatch<wl_buffer::WlBuffer, Arc<AtomicBool>> for WaylandState {
    fn event(_: &mut Self, _: &wl_buffer::WlBuffer, event: wl_buffer::Event,
             in_use: &Arc<AtomicBool>, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            in_use.store(false, Ordering::Release);
        }
    }
}

/// Frame callback user-data = surface index into `WaylandState::surf_ev`.
impl Dispatch<wl_callback::WlCallback, usize> for WaylandState {
    fn event(state: &mut Self, _: &wl_callback::WlCallback, event: wl_callback::Event,
             idx: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = event {
            if let Some(ev) = state.surf_ev.get_mut(*idx) {
                ev.frame_ready = true;
            }
            // wayland-client 0.31 automatically destroys the wl_callback proxy
            // when the Done event fires — no manual cleanup needed.
        }
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for WaylandState {
    fn event(_: &mut Self, _: &ZwlrLayerShellV1, _: zwlr_layer_shell_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
/// Layer surface user-data = surface index into `WaylandState::surf_ev`.
impl Dispatch<ZwlrLayerSurfaceV1, usize> for WaylandState {
    fn event(state: &mut Self, _: &ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event,
             idx: &usize, _: &Connection, _: &QueueHandle<Self>) {
        let Some(ev) = state.surf_ev.get_mut(*idx) else { return; };
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                ev.configure_serial = serial;
                ev.surf_width       = width;
                ev.surf_height      = height;
                ev.configured       = true;
                ev.needs_ack        = true;
            }
            zwlr_layer_surface_v1::Event::Closed => {
                tracing::warn!("layer_surface[{}] closed", idx);
                ev.closed = true;
            }
            _ => {}
        }
    }
}
/// wl_output user-data = index into `WaylandState::outputs`.
impl Dispatch<wl_output::WlOutput, usize> for WaylandState {
    fn event(state: &mut Self, _: &wl_output::WlOutput, event: wl_output::Event,
             idx: &usize, _: &Connection, _: &QueueHandle<Self>) {
        let Some(o) = state.outputs.get_mut(*idx) else { return; };
        match event {
            wl_output::Event::Mode { flags, width, height, .. } => {
                use wayland_client::WEnum;
                if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current))
                    && width > 0 && height > 0
                {
                    o.width  = width  as u32;
                    o.height = height as u32;
                }
            }
            wl_output::Event::Name { name } => { o.name = name; }
            _ => {}
        }
    }
}

// ─── SHM canvas (single allocation) ──────────────────────────────────────────

struct ShmCanvas {
    _fd:    OwnedFd,
    ptr:    *mut u8,
    size:   usize,
    buffer: wl_buffer::WlBuffer,
    _pool:  wl_shm_pool::WlShmPool,
}

unsafe impl Send for ShmCanvas {}

impl Drop for ShmCanvas {
    fn drop(&mut self) {
        self.buffer.destroy();
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.size); }
    }
}

impl ShmCanvas {
    fn create(
        shm:    &wl_shm::WlShm,
        qh:     &QueueHandle<WaylandState>,
        w:      u32,
        h:      u32,
        in_use: Arc<AtomicBool>,
    ) -> anyhow::Result<Self> {
        let stride = w.checked_mul(4).context("shm stride overflow")?;
        let size   = stride.checked_mul(h).context("shm size overflow")? as usize;

        // memfd_create: anonymous file, no path on disk, no TOCTOU.
        let name_c = std::ffi::CString::new("wpick-shm").unwrap();
        let fd = unsafe { libc::memfd_create(name_c.as_ptr(), 0) };
        anyhow::ensure!(fd >= 0, "memfd_create: {}", std::io::Error::last_os_error());
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.set_len(size as u64).context("ftruncate")?;

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(), size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(), 0,
            )
        };
        anyhow::ensure!(ptr != libc::MAP_FAILED, "mmap: {}", std::io::Error::last_os_error());
        // wl_shm_pool takes pool size as i32; guard against >2 GB surfaces.
        anyhow::ensure!(size <= i32::MAX as usize, "shm pool too large ({size} bytes)");
        let pool   = shm.create_pool(file.as_fd(), size as i32, qh, ());
        // in_use is passed as wl_buffer user-data — Release event clears it.
        let buffer = pool.create_buffer(
            0, w as i32, h as i32, stride as i32,
            wl_shm::Format::Argb8888,
            qh, in_use,
        );

        Ok(Self {
            _fd: OwnedFd::from(file),
            ptr: ptr as *mut u8, size,
            buffer, _pool: pool,
        })
    }

    fn fill_black(&mut self) {
        self.pixels_mut().chunks_exact_mut(4).for_each(|p| {
            p[0] = 0; p[1] = 0; p[2] = 0; p[3] = 255;
        });
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) }
    }
}

// ─── SHM buffer pool (3 slots) ───────────────────────────────────────────────

struct ShmSlot {
    canvas: ShmCanvas,
    in_use: Arc<AtomicBool>,
}

struct ShmPool {
    slots: [ShmSlot; 3],
}

impl ShmPool {
    fn create(shm: &wl_shm::WlShm, qh: &QueueHandle<WaylandState>, w: u32, h: u32) -> anyhow::Result<Self> {
        let mk = || -> anyhow::Result<ShmSlot> {
            let in_use = Arc::new(AtomicBool::new(false));
            let canvas = ShmCanvas::create(shm, qh, w, h, Arc::clone(&in_use))?;
            Ok(ShmSlot { canvas, in_use })
        };
        Ok(Self { slots: [mk()?, mk()?, mk()?] })
    }

    /// Returns the index of the first slot not currently held by the compositor.
    fn free_idx(&self) -> Option<usize> {
        self.slots.iter().position(|s| !s.in_use.load(Ordering::Acquire))
    }

    fn fill_all_black(&mut self) {
        for slot in &mut self.slots {
            slot.canvas.fill_black();
        }
    }
}

// ─── StaticDecoder ────────────────────────────────────────────────────────────

/// Single-frame decoder for static images (JPEG, PNG, WebP, etc.).
/// Decodes once at construction; `next_frame_bgra` copies the same data every call.
struct StaticDecoder {
    data: Vec<u8>,
    w:    u32,
    h:    u32,
}

impl StaticDecoder {
    fn try_open(path: &str, target_w: u32, target_h: u32, fit: FitMode) -> Option<Self> {
        let img = image::open(path).ok()?;

        // Exact pixel match — direct RGBA→BGRA, no scaling.
        if img.width() == target_w && img.height() == target_h {
            let rgba = img.to_rgba8();
            let mut bgra = rgba.into_raw();
            for px in bgra.chunks_exact_mut(4) { px.swap(0, 2); }
            return Some(Self { data: bgra, w: target_w, h: target_h });
        }

        let scaled = match fit {
            FitMode::Fit => {
                // Letterbox: resize preserving aspect ratio, black bars.
                let resized = img.resize(target_w, target_h, image::imageops::FilterType::Lanczos3);
                let mut bg = image::RgbaImage::new(target_w, target_h);
                for px in bg.pixels_mut() { *px = image::Rgba([0, 0, 0, 255]); }
                let x = (target_w.saturating_sub(resized.width()))  / 2;
                let y = (target_h.saturating_sub(resized.height())) / 2;
                image::imageops::overlay(&mut bg, &resized.to_rgba8(), x as i64, y as i64);
                bg.into_raw()
            }
            FitMode::Fill => {
                // Scale to fill, center-crop overflow.
                let resized = img.resize_to_fill(target_w, target_h, image::imageops::FilterType::Lanczos3);
                resized.to_rgba8().into_raw()
            }
            FitMode::Stretch => {
                // Stretch to fill — no aspect ratio preservation.
                let resized = img.resize_exact(target_w, target_h, image::imageops::FilterType::Lanczos3);
                resized.to_rgba8().into_raw()
            }
            FitMode::Center => {
                // No scaling — center, black borders.
                let mut bg = image::RgbaImage::new(target_w, target_h);
                for px in bg.pixels_mut() { *px = image::Rgba([0, 0, 0, 255]); }
                let x = (target_w.saturating_sub(img.width()))  / 2;
                let y = (target_h.saturating_sub(img.height())) / 2;
                image::imageops::overlay(&mut bg, &img.to_rgba8(), x as i64, y as i64);
                bg.into_raw()
            }
        };

        // RGBA → BGRA.
        let mut bgra = scaled;
        for px in bgra.chunks_exact_mut(4) { px.swap(0, 2); }
        Some(Self { data: bgra, w: target_w, h: target_h })
    }

    fn next_frame_bgra(&mut self, dst: &mut [u8]) -> anyhow::Result<bool> {
        if dst.len() < self.data.len() {
            anyhow::bail!("StaticDecoder: dst too small ({} < {})", dst.len(), self.data.len());
        }
        dst[..self.data.len()].copy_from_slice(&self.data);
        Ok(true)
    }

    fn frame_duration(&self) -> Duration {
        // Refresh once per second — no animation, saves CPU.
        Duration::from_secs(1)
    }

    fn dimensions(&self) -> (u32, u32) { (self.w, self.h) }
}

// ─── AnyDecoder ──────────────────────────────────────────────────────────────

enum AnyDecoder { Hw(HwDecoder), Sw(VideoDecoder), Static(StaticDecoder) }

impl AnyDecoder {
    fn next_frame_bgra(&mut self, dst: &mut [u8]) -> anyhow::Result<bool> {
        match self {
            AnyDecoder::Hw(d)     => d.next_frame_bgra(dst),
            AnyDecoder::Sw(d)     => d.next_frame_bgra(dst),
            AnyDecoder::Static(d) => d.next_frame_bgra(dst),
        }
    }
    fn seek_to_start(&mut self) -> anyhow::Result<()> {
        match self {
            AnyDecoder::Hw(d)     => d.seek_to_start(),
            AnyDecoder::Sw(d)     => d.seek_to_start(),
            AnyDecoder::Static(_) => Ok(()), // nothing to seek
        }
    }
    fn frame_duration(&self) -> Duration {
        match self {
            AnyDecoder::Hw(d)     => d.frame_duration(),
            AnyDecoder::Sw(d)     => d.frame_duration(),
            AnyDecoder::Static(d) => d.frame_duration(),
        }
    }
    fn is_hw(&self) -> bool { matches!(self, AnyDecoder::Hw(_)) }
}

// ─── Per-output surface state ─────────────────────────────────────────────────

struct OutputSurface {
    /// Index into `RendererCtx::wls.surf_ev` — routes Dispatch events here.
    ev_idx:        usize,
    /// Index into `RendererCtx::wls.outputs` — used when recreating the surface.
    output_idx:    usize,
    output_name:   String,
    wl_surface:    wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    pool:          Option<ShmPool>,
    decoder:       Option<AnyDecoder>,
    surf_w:        u32,
    surf_h:        u32,
    next_frame:    Instant,
    canvas_ready:  bool,
    in_fullscreen: bool,
    closed_in_fs:  bool,
    nudge_at:      Option<Instant>,
    /// Set on fullscreen exit; after this delay we recreate the surface so it
    /// becomes the newest (highest z-order) on the Bottom layer, landing above
    /// any competitor surface that the shell restarted while we were paused.
    recreate_after: Option<Instant>,
    /// Timestamp of the last committed frame.  Used as a fallback when the
    /// compositor stops sending frame callbacks (e.g. surface hidden behind a
    /// fullscreen window): if no callback arrives within 300 ms we resume
    /// rendering without waiting.
    last_commit_at: Option<Instant>,
    /// When Some, this surface is pinned to a specific wallpaper from `[monitors]` config
    /// and ignores global `Set` commands.
    pinned_wp:     Option<WallpaperInfo>,
    /// Active fit mode for this monitor — settable at runtime via SetFit.
    fit:           FitMode,
    /// Most-recent successfully decoded frame; copied here every frame for crossfade.
    /// Set to false after a HW decode failure so we skip HW and go straight to SW.
    hw_ok:             bool,
    last_frame:        Vec<u8>,
    last_frame_tick:   u32,      // counts Ok(true) frames, used to throttle last_frame updates
    /// Snapshot of last_frame taken when a wallpaper change starts.
    fade_old:          Vec<u8>,
    fade_frames_left:  u32,
    fade_frames_total: u32,
}

// ─── Renderer context ─────────────────────────────────────────────────────────

// SAFETY: RendererCtx is created in one spawn_blocking task and immediately
// moved into another.  All non-Send fields (ffmpeg SwsContext raw pointers,
// wl_* Wayland objects) are used exclusively from the render thread.
unsafe impl Send for RendererCtx {}

struct RendererCtx {
    conn:        Connection,
    evq:         wayland_client::EventQueue<WaylandState>,
    wls:         WaylandState,
    compositor:  wl_compositor::WlCompositor,
    layer_shell: ZwlrLayerShellV1,
    shm:         wl_shm::WlShm,
    /// One entry per discovered wl_output.
    surfaces:    Vec<OutputSurface>,
}

// ─── Surface helpers ──────────────────────────────────────────────────────────

/// Create a layer surface pinned to a specific wl_output.
/// `ev_idx` is stored as user-data on the ZwlrLayerSurfaceV1 so Dispatch
/// events can route to the correct SurfaceEvent slot.
fn make_output_surface(
    compositor:  &wl_compositor::WlCompositor,
    layer_shell: &ZwlrLayerShellV1,
    output:      &wl_output::WlOutput,
    qh:          &QueueHandle<WaylandState>,
    ev_idx:      usize,
) -> (wl_surface::WlSurface, ZwlrLayerSurfaceV1) {
    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
    let wl = compositor.create_surface(qh, ());
    let ls = layer_shell.get_layer_surface(&wl, Some(output), Layer::Bottom, "wpick".into(), qh, ev_idx);
    ls.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    ls.set_size(0, 0);
    ls.set_exclusive_zone(-1);
    // Empty input region: compositor will not route pointer events to this surface.
    // Fixes cursor shape staying stuck as the last window's cursor (e.g. I-beam)
    // when the mouse moves over the wallpaper, and eliminates compositor hit-test
    // overhead on every pointer motion event.
    let empty_region = compositor.create_region(qh, ());
    wl.set_input_region(Some(&empty_region));
    wl.commit();
    (wl, ls)
}

fn recreate_surface(ctx: &mut RendererCtx, i: usize) {
    let ev_idx  = ctx.surfaces[i].ev_idx;
    let out_idx = ctx.surfaces[i].output_idx;
    let qh      = ctx.evq.handle();
    let output  = &ctx.wls.outputs[out_idx]._wl_output;
    let (new_wl, new_ls) = make_output_surface(&ctx.compositor, &ctx.layer_shell, output, &qh, ev_idx);
    let old_ls = std::mem::replace(&mut ctx.surfaces[i].layer_surface, new_ls);
    let old_wl = std::mem::replace(&mut ctx.surfaces[i].wl_surface, new_wl);
    drop(old_ls);
    drop(old_wl);
    // Drop the old ShmPool: its wl_buffer objects referenced the destroyed wl_surface,
    // so the compositor will never send Release events for them.  Without this drop the
    // in_use AtomicBool flags stay true forever and free_idx() returns None, causing the
    // render loop to stall on this surface (no frames committed, decoder idles).
    ctx.surfaces[i].pool           = None;
    ctx.surfaces[i].canvas_ready   = false;
    ctx.surfaces[i].fade_frames_left = 0;
    ctx.surfaces[i].fade_old       = Vec::new();
    ctx.surfaces[i].last_frame.clear();
    ctx.surfaces[i].last_frame.shrink_to_fit();
    ctx.surfaces[i].nudge_at       = None;
    ctx.surfaces[i].last_commit_at = None;
    ctx.wls.surf_ev[ev_idx] = SurfaceEvent { frame_ready: true, ..Default::default() };
    let _ = ctx.evq.flush();
    tracing::info!("surface[{}] ({}) recreated", i, ctx.surfaces[i].output_name);
}

/// Attach `slot_i` from surface `surf_i`'s pool and request a frame callback.
/// `ev_idx` is passed as callback user-data so Done routes to the right SurfaceEvent.
fn commit_slot(ctx: &mut RendererCtx, surf_i: usize, slot_i: usize) {
    let ev_idx = ctx.surfaces[surf_i].ev_idx;
    let sw     = ctx.surfaces[surf_i].surf_w as i32;
    let sh     = ctx.surfaces[surf_i].surf_h as i32;
    let qh     = ctx.evq.handle();  // owned QueueHandle — no borrow on ctx after this line
    ctx.surfaces[surf_i].pool.as_ref().unwrap().slots[slot_i].in_use.store(true, Ordering::Release);
    let buffer = &ctx.surfaces[surf_i].pool.as_ref().unwrap().slots[slot_i].canvas.buffer;
    ctx.surfaces[surf_i].wl_surface.attach(Some(buffer), 0, 0);
    ctx.surfaces[surf_i].wl_surface.damage_buffer(0, 0, sw, sh);
    ctx.surfaces[surf_i].wl_surface.frame(&qh, ev_idx);
    ctx.surfaces[surf_i].wl_surface.commit();
    ctx.surfaces[surf_i].last_commit_at = Some(Instant::now());
}

// ─── Init ─────────────────────────────────────────────────────────────────────

fn init_renderer() -> anyhow::Result<RendererCtx> {
    let conn    = Connection::connect_to_env().context("wayland connect")?;
    let mut evq = conn.new_event_queue::<WaylandState>();
    let qh      = evq.handle();
    let mut wls = WaylandState::default();

    conn.display().get_registry(&qh, ());
    // Two roundtrips: first collects globals, second drains wl_output Mode/Name events.
    evq.roundtrip(&mut wls).context("globals roundtrip")?;
    evq.roundtrip(&mut wls).context("output events roundtrip")?;

    let compositor  = wls.compositor.take().ok_or_else(|| anyhow::anyhow!("no wl_compositor"))?;
    let layer_shell = wls.layer_shell.take().ok_or_else(|| anyhow::anyhow!("no zwlr_layer_shell_v1"))?;
    let shm         = wls.shm.take().ok_or_else(|| anyhow::anyhow!("no wl_shm"))?;
    anyhow::ensure!(!wls.outputs.is_empty(), "no wl_output found");

    // Create one layer surface per output.  surf_ev is pre-populated so that
    // Dispatch handlers can write into the correct slot as Configure events arrive.
    let n = wls.outputs.len();
    let mut raw: Vec<(wl_surface::WlSurface, ZwlrLayerSurfaceV1)> = Vec::with_capacity(n);
    for i in 0..n {
        wls.surf_ev.push(SurfaceEvent::default());
        raw.push(make_output_surface(&compositor, &layer_shell, &wls.outputs[i]._wl_output, &qh, i));
    }

    // Collect Configure events for every surface we just committed.
    evq.roundtrip(&mut wls).context("configure roundtrip")?;

    let mut surfaces = Vec::with_capacity(n);
    for (i, (wl_surf, layer_surf)) in raw.into_iter().enumerate() {
        anyhow::ensure!(wls.surf_ev[i].configured, "surface[{}] ({}) not configured", i, wls.outputs[i].name);

        let ev = &wls.surf_ev[i];
        let sw = if ev.surf_width  > 0 { ev.surf_width  } else { wls.outputs[i].width  };
        let sh = if ev.surf_height > 0 { ev.surf_height } else { wls.outputs[i].height };

        let mut pool = ShmPool::create(&shm, &qh, sw, sh).context("shm pool")?;
        pool.fill_all_black();

        // Ack + initial black commit + request first frame callback.
        layer_surf.ack_configure(ev.configure_serial);
        pool.slots[0].in_use.store(true, Ordering::Release);
        wl_surf.attach(Some(&pool.slots[0].canvas.buffer), 0, 0);
        wl_surf.damage_buffer(0, 0, sw as i32, sh as i32);
        wl_surf.frame(&qh, i);
        wl_surf.commit();

        tracing::info!("surface[{}] {} {}x{}", i, wls.outputs[i].name, sw, sh);

        surfaces.push(OutputSurface {
            ev_idx:       i,
            output_idx:   i,
            output_name:  wls.outputs[i].name.clone(),
            wl_surface:   wl_surf,
            layer_surface: layer_surf,
            pool:          Some(pool),
            decoder:       None,
            surf_w:        sw,
            surf_h:        sh,
            next_frame:    Instant::now(),
            canvas_ready:  false,
            in_fullscreen:  false,
            closed_in_fs:   false,
            nudge_at:       None,
            recreate_after: None,
            // Set to now so the 300ms fallback timer fires correctly from the first frame.
            // None would permanently disable the fallback until the first commit_slot call.
            last_commit_at: Some(Instant::now()),
            pinned_wp:         None,
            fit:               FitMode::default(),
            hw_ok:             true,
            last_frame:        Vec::new(),
            last_frame_tick:   0,
            fade_old:          Vec::new(),
            fade_frames_left:  0,
            fade_frames_total: 0,
        });

        wls.surf_ev[i].needs_ack   = false;
        wls.surf_ev[i].frame_ready = false; // wait for first callback
    }

    evq.flush().context("flush")?;
    tracing::info!("renderer ready — {} output(s)", surfaces.len());
    Ok(RendererCtx { conn, evq, wls, compositor, layer_shell, shm, surfaces })
}

// ─── Async wrapper ────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run(
    mut wallpaper_rx:        watch::Receiver<Option<WallpaperInfo>>,
    shutdown_rx:             broadcast::Receiver<()>,
    pause_cfg:               PauseConfig,
    monitors:                HashMap<String, MonitorConfig>,
    cache:                   Arc<tokio::sync::Mutex<Cache>>,
    on_fullscreen_exit:      Option<Arc<dyn Fn() + Send + Sync>>,
    mut per_monitor_rx:      watch::Receiver<HashMap<String, Option<WallpaperInfo>>>,
    fit_rx:                  watch::Receiver<(String, FitMode)>,
    outputs_out:             Arc<std::sync::Mutex<Vec<crate::state::OutputInfo>>>,
    reassert_flag:           Arc<std::sync::atomic::AtomicBool>,
    mut workspace_wallpaper_rx: watch::Receiver<HashMap<String, u64>>,
    max_fps:                 u32,
) -> anyhow::Result<()> {
    let mut shutdown_rx   = shutdown_rx;
    let mut init_failures = 0u32;

    // Shared sender slot: the fs-mon thread always sends to the current per-iteration
    // SyncSender.  On each reinit we swap in a fresh sender and pass the matching
    // receiver to render_loop.  The slot starts empty (None) — populated below.
    let workspace_sender_slot: WorkspaceSenderSlot =
        Arc::new(std::sync::Mutex::new(None));

    let fullscreen = start_fullscreen_monitor(
        on_fullscreen_exit,
        Arc::clone(&reassert_flag),
        Arc::clone(&workspace_sender_slot),
    );

    // Start pause monitors — only for sources enabled in config.
    let battery = if pause_cfg.on_battery   { start_battery_monitor() } else { Arc::new(AtomicBool::new(false)) };
    let lid     = if pause_cfg.on_lid_close { start_lid_monitor()     } else { Arc::new(AtomicBool::new(false)) };
    let pause   = PauseMonitors {
        on_fullscreen: pause_cfg.on_fullscreen,
        fullscreen:    Arc::clone(&fullscreen),
        battery,
        lid,
    };

    loop {
        let ctx = match tokio::task::spawn_blocking(init_renderer).await.context("init panic")? {
            Ok(ctx) => { init_failures = 0; ctx }
            Err(e)  => {
                init_failures += 1;
                tracing::warn!("renderer init failed ({}): {}", init_failures, e);
                if init_failures >= 10 { return Err(e.context("renderer: 10 failures")); }
                if shutdown_rx.try_recv().is_ok() { break; }
                tokio::time::sleep(Duration::from_millis(500 * init_failures.min(4) as u64)).await;
                if shutdown_rx.try_recv().is_ok() { break; }
                continue;
            }
        };

        // Publish connected output names and resolutions so IPC can serve ListOutputs.
        if let Ok(mut g) = outputs_out.lock() {
            *g = ctx.surfaces.iter()
                .map(|s| crate::state::OutputInfo {
                    name:   s.output_name.clone(),
                    width:  s.surf_w,
                    height: s.surf_h,
                })
                .collect();
        }

        let (wp_tx, wp_rx)   = std::sync::mpsc::channel::<Option<WallpaperInfo>>();
        let (sd_tx, sd_rx)   = std::sync::mpsc::channel::<()>();
        let (pin_tx, pin_rx) = std::sync::mpsc::channel::<HashMap<String, Option<WallpaperInfo>>>();
        let (fit_fwd_tx, fit_fwd_rx) = std::sync::mpsc::channel::<(String, FitMode)>();
        let (wwmap_tx, wwmap_rx)     = std::sync::mpsc::channel::<HashMap<String, u64>>();
        let _ = wp_tx.send(wallpaper_rx.borrow_and_update().clone());
        let _ = pin_tx.send(per_monitor_rx.borrow_and_update().clone());
        let _ = wwmap_tx.send(workspace_wallpaper_rx.borrow_and_update().clone());

        // Per-reinit workspace change channel.  We install the fresh sender into the
        // shared slot so the fs-mon thread routes events to this iteration's receiver.
        let (ws_change_tx, ws_change_rx) = std::sync::mpsc::sync_channel::<String>(64);
        if let Ok(mut slot) = workspace_sender_slot.lock() {
            *slot = Some(ws_change_tx);
        }

        let mut rx2 = wallpaper_rx.clone();
        let wp_fwd = tokio::spawn(async move {
            loop {
                if rx2.changed().await.is_err() { break; }
                if wp_tx.send(rx2.borrow_and_update().clone()).is_err() { break; }
            }
        });
        let mut sd_sub = shutdown_rx.resubscribe();
        let sd_fwd = tokio::spawn(async move {
            let _ = sd_sub.recv().await;
            let _ = sd_tx.send(());
        });
        let mut pm_rx2 = per_monitor_rx.clone();
        let pin_fwd = tokio::spawn(async move {
            loop {
                if pm_rx2.changed().await.is_err() { break; }
                if pin_tx.send(pm_rx2.borrow_and_update().clone()).is_err() { break; }
            }
        });
        let mut fit_rx2 = fit_rx.clone();
        let fit_fwd = tokio::spawn(async move {
            loop {
                if fit_rx2.changed().await.is_err() { break; }
                if fit_fwd_tx.send(fit_rx2.borrow_and_update().clone()).is_err() { break; }
            }
        });
        let mut wwmap_rx2 = workspace_wallpaper_rx.clone();
        let wwmap_fwd = tokio::spawn(async move {
            loop {
                if wwmap_rx2.changed().await.is_err() { break; }
                if wwmap_tx.send(wwmap_rx2.borrow_and_update().clone()).is_err() { break; }
            }
        });

        // Resolve per-monitor wallpapers from cache (async, before entering blocking task).
        // Re-resolved each reinit so a re-scan between reinits picks up new entries.
        let monitor_wallpapers = resolve_monitor_wallpapers(&monitors, &cache).await;

        // Resolve workspace wallpapers from the current id-map into WallpaperInfo objects.
        let initial_ws_map = workspace_wallpaper_rx.borrow().clone();
        let workspace_wallpaper_map = resolve_workspace_wallpapers(&initial_ws_map, &cache).await;
        let workspace_wallpaper_map = Arc::new(std::sync::RwLock::new(workspace_wallpaper_map));
        let ww_map_arc = Arc::clone(&workspace_wallpaper_map);

        let fs = Arc::clone(&fullscreen);
        let pm = PauseMonitors {
            on_fullscreen: pause.on_fullscreen,
            fullscreen:    Arc::clone(&pause.fullscreen),
            battery:       Arc::clone(&pause.battery),
            lid:           Arc::clone(&pause.lid),
        };
        let monitors_cfg = monitors.clone();
        let reassert     = Arc::clone(&reassert_flag);
        let cache_arc    = Arc::clone(&cache);

        let result = tokio::task::spawn_blocking(move || {
            render_loop(ctx, wp_rx, pin_rx, sd_rx, fit_fwd_rx, fs, pm, monitors_cfg,
                        monitor_wallpapers, reassert, ws_change_rx, ww_map_arc, wwmap_rx,
                        cache_arc, max_fps)
        }).await.context("render loop panic")?;

        // Clear the workspace sender slot so the fs-mon thread's sends fail cleanly
        // during the reinit gap (receiver dropped, new sender not installed yet).
        if let Ok(mut slot) = workspace_sender_slot.lock() { *slot = None; }

        wp_fwd.abort();
        sd_fwd.abort();
        pin_fwd.abort();
        fit_fwd.abort();
        wwmap_fwd.abort();

        match result {
            Ok(()) => break,
            Err(ref e) if e.to_string().contains("__reassert__") => {
                tracing::info!("renderer: reasserting surfaces (session startup z-order fix)");
                continue; // reinit — new surfaces end up on top of QuickShell background
            }
            Err(ref e) if e.to_string().contains("__fatal__") => {
                if shutdown_rx.try_recv().is_ok() { break; }
                tracing::warn!("renderer fatal — full reinit");
                continue;
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

// ─── Per-monitor wallpaper resolution ────────────────────────────────────────

/// Look up WallpaperInfo for every monitor that has a `wallpaper_id` configured.
/// Missing or invalid IDs are skipped — those surfaces will fall back to the global wallpaper.
async fn resolve_monitor_wallpapers(
    monitors: &HashMap<String, MonitorConfig>,
    cache:    &Arc<tokio::sync::Mutex<Cache>>,
) -> HashMap<String, WallpaperInfo> {
    let mut out = HashMap::new();
    for (name, cfg) in monitors {
        let Some(id) = cfg.wallpaper_id else { continue };
        let guard = cache.lock().await;
        match guard.get_by_id(id) {
            Ok(Some(info)) => { out.insert(name.clone(), info); }
            Ok(None) => tracing::warn!("monitor '{}': wallpaper_id {} not in cache", name, id),
            Err(e)   => tracing::warn!("monitor '{}': cache lookup failed: {}", name, e),
        }
    }
    out
}

/// Resolve a workspace-name→wallpaper-id map into workspace-name→WallpaperInfo.
/// Missing / invalid IDs are silently skipped.
async fn resolve_workspace_wallpapers(
    ws_map: &HashMap<String, u64>,
    cache:  &Arc<tokio::sync::Mutex<Cache>>,
) -> HashMap<String, WallpaperInfo> {
    let mut out = HashMap::new();
    for (ws, &id) in ws_map {
        let guard = cache.lock().await;
        match guard.get_by_id(id) {
            Ok(Some(info)) => { out.insert(ws.clone(), info); }
            Ok(None)       => tracing::warn!("workspace '{}': wallpaper_id {} not in cache", ws, id),
            Err(e)         => tracing::warn!("workspace '{}': cache lookup failed: {}", ws, e),
        }
    }
    out
}

// ─── Render loop ──────────────────────────────────────────────────────────────

const NUDGE_TIMEOUT: Duration = Duration::from_secs(2);
/// Number of decoded frames to blend during a wallpaper crossfade (~4 s at 30 fps).
const FADE_FRAMES: u32 = 120;
/// Update last_frame snapshot only every N successful frames to save memory bandwidth.
const LAST_FRAME_UPDATE_INTERVAL: u32 = 10;

#[allow(clippy::too_many_arguments)]
fn render_loop(
    mut ctx:                RendererCtx,
    wp_rx:                  std::sync::mpsc::Receiver<Option<WallpaperInfo>>,
    pin_rx:                 std::sync::mpsc::Receiver<HashMap<String, Option<WallpaperInfo>>>,
    sd_rx:                  std::sync::mpsc::Receiver<()>,
    fit_rx:                 std::sync::mpsc::Receiver<(String, FitMode)>,
    fullscreen:             Arc<AtomicBool>,
    pause:                  PauseMonitors,
    monitors_cfg:           HashMap<String, MonitorConfig>,
    monitor_wallpapers:     HashMap<String, WallpaperInfo>,
    reassert_flag:          Arc<AtomicBool>,
    workspace_change_rx:    std::sync::mpsc::Receiver<String>,
    workspace_wallpaper_map: Arc<std::sync::RwLock<HashMap<String, WallpaperInfo>>>,
    workspace_map_update_rx: std::sync::mpsc::Receiver<HashMap<String, u64>>,
    cache:                  Arc<tokio::sync::Mutex<Cache>>,
    max_fps:                u32,
) -> anyhow::Result<()> {
    // Minimum interval between committed frames (1 / max_fps).
    // Caps wl_shm upload frequency, reducing compositor CPU load and cursor jitter.
    let fps_cap_dur: Option<Duration> = if max_fps > 0 {
        Some(Duration::from_micros(1_000_000 / max_fps.max(1) as u64))
    } else {
        None
    };
    let mut current_wp: Option<WallpaperInfo> = None;

    // Apply per-monitor fit modes from config.
    for surf in &mut ctx.surfaces {
        if let Some(cfg) = monitors_cfg.get(&surf.output_name) {
            surf.fit = cfg.fit;
        }
    }

    // Apply per-monitor pinned wallpapers from config before the first frame.
    for surf in &mut ctx.surfaces {
        if let Some(info) = monitor_wallpapers.get(&surf.output_name) {
            tracing::info!("surface[{}] ({}) pinned to wallpaper '{}'",
                surf.ev_idx, surf.output_name, info.title);
            surf.decoder   = open_decoder(info, surf.surf_w, surf.surf_h, surf.fit, surf.hw_ok);
            surf.next_frame = Instant::now();
            surf.pinned_wp  = Some(info.clone());
        }
    }

    loop {
        // ── shutdown ──────────────────────────────────────────────────────────
        if sd_rx.try_recv().is_ok() { break; }

        // ── surface reassert (session startup z-order fix) ────────────────────
        if reassert_flag.swap(false, Ordering::Relaxed) {
            anyhow::bail!("__reassert__");
        }

        // ── wayland events ────────────────────────────────────────────────────
        // dispatch_pending does NOT read from the socket in wayland-client 0.31.
        // We must call prepare_read()+read() first so frame callbacks and other
        // compositor events actually arrive (without this the first frame renders
        // but no subsequent frames do because frame_ready never becomes true again).
        if let Some(guard) = ctx.conn.prepare_read() {
            let _ = guard.read();
        }
        if let Err(e) = ctx.evq.dispatch_pending(&mut ctx.wls) {
            tracing::warn!("wayland connection lost: {} — fatal", e);
            anyhow::bail!("__fatal__");
        }
        let _ = ctx.evq.flush();

        // ── fit mode changes (IPC SetFit) ─────────────────────────────────────
        while let Ok((monitor_key, new_fit)) = fit_rx.try_recv() {
            for surf in &mut ctx.surfaces {
                if monitor_key == "*" || monitor_key == surf.output_name {
                    surf.fit = new_fit;
                    // Recreate decoder with new fit mode.
                    let active_wp = surf.pinned_wp.clone().or_else(|| current_wp.clone());
                    if let Some(ref info) = active_wp {
                        surf.decoder      = open_decoder(info, surf.surf_w, surf.surf_h, surf.fit, surf.hw_ok);
                        surf.next_frame   = Instant::now();
                        surf.canvas_ready = false;
                    }
                }
            }
        }

        // ── dynamic per-monitor pins (IPC Set with monitor=) ─────────────────
        // Consume only the most-recent snapshot — watch semantics mean older
        // intermediate values don't matter.
        let mut latest_pins: Option<HashMap<String, Option<WallpaperInfo>>> = None;
        while let Ok(pins) = pin_rx.try_recv() { latest_pins = Some(pins); }
        if let Some(pins) = latest_pins {
            for surf in &mut ctx.surfaces {
                if let Some(wp_opt) = pins.get(&surf.output_name) {
                    match wp_opt {
                        Some(info) => {
                            tracing::info!("pin update: surface '{}' → '{}'",
                                surf.output_name, info.title);
                            surf.decoder    = open_decoder(info, surf.surf_w, surf.surf_h, surf.fit, surf.hw_ok);
                            surf.next_frame = Instant::now();
                            surf.pinned_wp  = Some(info.clone());
                            surf.canvas_ready = false;
                        }
                        None => {
                            tracing::info!("pin update: surface '{}' unpinned", surf.output_name);
                            surf.pinned_wp    = None;
                            surf.canvas_ready = false;
                            surf.decoder = current_wp.as_ref()
                                .and_then(|info| open_decoder(info, surf.surf_w, surf.surf_h, surf.fit, surf.hw_ok));
                            if surf.decoder.is_some() { surf.next_frame = Instant::now(); }
                        }
                    }
                }
            }
        }

        // ── wallpaper change → recreate decoders for non-pinned surfaces ──────
        while let Ok(new_wp) = wp_rx.try_recv() {
            if let Some(ref info) = new_wp {
                tracing::info!("wallpaper: {}", info.title);
            } else {
                tracing::info!("wallpaper cleared — blanking shm surfaces");
            }
            let clearing = new_wp.is_none();
            current_wp = new_wp.clone();
            for surf in &mut ctx.surfaces {
                // Surfaces with a per-monitor override ignore global wallpaper changes.
                if surf.pinned_wp.is_some() { continue; }
                // Capture old frame for crossfade before switching decoder.
                surf.hw_ok = true; // reset per-wallpaper HW flag for new video
                if surf.canvas_ready && new_wp.is_some() && !surf.last_frame.is_empty() {
                    tracing::info!("crossfade: starting {} frames on '{}'",
                        FADE_FRAMES, surf.output_name);
                    surf.fade_old          = std::mem::take(&mut surf.last_frame);
                    surf.last_frame_tick   = 0;
                    surf.fade_frames_left  = FADE_FRAMES;
                    surf.fade_frames_total = FADE_FRAMES;
                } else {
                    tracing::info!("crossfade: skipped (canvas_ready={} last_frame_empty={})",
                        surf.canvas_ready, surf.last_frame.is_empty());
                    surf.fade_frames_left = 0;
                    // Free any in-progress fade buffer from a previous wallpaper change
                    // that hasn't finished yet; prevents 8–32 MB leak on rapid changes.
                    surf.fade_old = Vec::new();
                }
                surf.decoder      = None;
                surf.canvas_ready = false;
                if let Some(ref info) = new_wp {
                    surf.decoder    = open_decoder(info, surf.surf_w, surf.surf_h, surf.fit, surf.hw_ok);
                    surf.next_frame = Instant::now();
                }
            }
            // When the wallpaper is cleared (e.g. a web wallpaper was just set),
            // commit a fully-transparent/black frame on every non-pinned surface so
            // the wl_shm layer no longer occludes the webview's layer-shell surface.
            if clearing {
                for i in 0..ctx.surfaces.len() {
                    if ctx.surfaces[i].pinned_wp.is_some() { continue; }
                    if let Some(slot_i) = ctx.surfaces[i].pool.as_ref().and_then(|p| p.free_idx()) {
                        if let Some(pool) = ctx.surfaces[i].pool.as_mut() {
                            pool.slots[slot_i].canvas.pixels_mut().fill(0);
                        }
                        commit_slot(&mut ctx, i, slot_i);
                    }
                }
                let _ = ctx.evq.flush();
            }
        }

        // ── workspace wallpaper map updates (IPC SetWorkspaceWallpaper) ──────────
        // Re-resolve IDs to WallpaperInfo when the map changes via IPC.
        while let Ok(new_id_map) = workspace_map_update_rx.try_recv() {
            // Resolve synchronously using blocking_lock — we are in a blocking thread.
            let mut resolved: HashMap<String, WallpaperInfo> = HashMap::new();
            for (ws, id) in &new_id_map {
                match cache.blocking_lock().get_by_id(*id) {
                    Ok(Some(info)) => { resolved.insert(ws.clone(), info); }
                    Ok(None)       => tracing::warn!("workspace '{}': id {} not in cache", ws, id),
                    Err(e)         => tracing::warn!("workspace '{}': lookup failed: {}", ws, e),
                }
            }
            if let Ok(mut map) = workspace_wallpaper_map.write() {
                *map = resolved;
            }
        }

        // ── workspace change → apply per-workspace wallpaper ──────────────────
        while let Ok(workspace_name) = workspace_change_rx.try_recv() {
            let map = workspace_wallpaper_map.read()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(info) = map.get(&workspace_name) {
                tracing::info!("workspace '{}' → wallpaper '{}'", workspace_name, info.title);
                let info = info.clone();
                drop(map); // release the read lock before mutating surfaces
                for surf in &mut ctx.surfaces {
                    // Don't override per-monitor pins.
                    if surf.pinned_wp.is_none() {
                        surf.decoder      = open_decoder(&info, surf.surf_w, surf.surf_h, surf.fit, surf.hw_ok);
                        surf.next_frame   = Instant::now();
                        surf.canvas_ready = false;
                    }
                }
                current_wp = Some(info);
            }
        }

        // ── per-surface processing ────────────────────────────────────────────
        let is_fs    = fullscreen.load(Ordering::Relaxed);
        let is_paused = pause.is_paused();
        let mut any_work = false;
        for i in 0..ctx.surfaces.len() {
            if process_surface(&mut ctx, i, is_fs, is_paused, &current_wp, fps_cap_dur) {
                any_work = true;
            }
        }

        // When no frame was decoded this iteration, block on the Wayland socket until a
        // frame callback arrives or until the next video frame is due — whichever comes
        // first.  Replaces the blind 1 ms sleep: the old sleep caused the render thread
        // to spin ~1000 times/second between frames, stealing CPU from the compositor
        // and producing cursor jitter on high-refresh-rate displays when two wallpapers
        // play simultaneously.
        if !any_work {
            let now = Instant::now();
            let timeout_ms: i32 = ctx.surfaces.iter()
                .filter(|s| s.decoder.is_some())
                .filter_map(|s| s.next_frame.checked_duration_since(now))
                .min()
                .map(|d| d.as_millis().clamp(1, 16) as i32)
                .unwrap_or(16);
            if let Some(guard) = ctx.conn.prepare_read() {
                let fd = ctx.conn.as_fd().as_raw_fd();
                let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                // Check poll() return: EINTR (-1) or spurious (0) must not call read().
                let ret = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
                if ret > 0 { let _ = guard.read(); } else { drop(guard); }
            } else {
                // Events already buffered in the display queue.  yield_now() is a
                // spin on spawn_blocking threads; sleep 1ms instead to give the
                // compositor CPU time without burning a full core.
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    for surf in ctx.surfaces.drain(..) {
        drop(surf.layer_surface);
        drop(surf.wl_surface);
    }
    let _ = ctx.evq.flush();
    Ok(())
}

// ─── Per-surface processing ───────────────────────────────────────────────────

/// Process one output surface for the current loop iteration.
/// Returns `true` if a video frame was decoded and committed (work was done).
fn process_surface(
    ctx:         &mut RendererCtx,
    i:           usize,
    is_fs:       bool,
    is_paused:   bool,
    current_wp:  &Option<WallpaperInfo>,
    fps_cap_dur: Option<Duration>,
) -> bool {
    let ev_idx = ctx.surfaces[i].ev_idx;

    // ── surface closed ────────────────────────────────────────────────────────
    if ctx.wls.surf_ev[ev_idx].closed {
        ctx.wls.surf_ev[ev_idx].closed     = false;
        ctx.wls.surf_ev[ev_idx].configured = false;
        ctx.surfaces[i].canvas_ready       = false;
        if ctx.surfaces[i].in_fullscreen {
            ctx.surfaces[i].closed_in_fs = true;
            tracing::info!("surface[{}] closed during fullscreen — deferring", i);
        } else {
            recreate_surface(ctx, i);
        }
        return false;
    }

    // ── fullscreen transition ─────────────────────────────────────────────────
    if ctx.surfaces[i].in_fullscreen && !is_fs {
        ctx.surfaces[i].in_fullscreen = false;
        if ctx.surfaces[i].closed_in_fs {
            // Compositor closed the surface during fullscreen — must recreate.
            ctx.surfaces[i].closed_in_fs = false;
            tracing::info!("fullscreen exit: surface[{}] was closed — recreating", i);
            recreate_surface(ctx, i);
        } else {
            // Schedule a delayed recreate. We wait 800 ms so that any shell
            // (e.g. QuickShell) that reacts to the fullscreen exit by starting
            // a new wallpaper process gets its surface registered first; our
            // newer surface will then sit on top of it in Hyprland's z-order.
            tracing::info!("fullscreen exit: surface[{}] scheduling recreate in 800 ms", i);
            ctx.wls.surf_ev[ev_idx].frame_ready = true;
            ctx.surfaces[i].recreate_after = Some(Instant::now() + Duration::from_millis(300));
        }
        return false;
    }
    ctx.surfaces[i].in_fullscreen = is_fs;

    // ── nudge timeout fallback ────────────────────────────────────────────────
    if let Some(t) = ctx.surfaces[i].nudge_at {
        if !ctx.wls.surf_ev[ev_idx].configured && t.elapsed() > NUDGE_TIMEOUT {
            tracing::warn!("nudge timed out on surface[{}] — recreating", i);
            ctx.surfaces[i].nudge_at = None;
            recreate_surface(ctx, i);
        }
    }

    // ── delayed post-fullscreen recreate for z-order ──────────────────────────
    if let Some(t) = ctx.surfaces[i].recreate_after {
        if Instant::now() >= t {
            ctx.surfaces[i].recreate_after = None;
            tracing::info!("post-fullscreen recreate: surface[{}] (z-order)", i);
            recreate_surface(ctx, i);
            // Arm the nudge fallback so the surface recovers if configure never arrives.
            ctx.surfaces[i].nudge_at = Some(Instant::now());
            return false;
        }
    }

    // ── configure ack ─────────────────────────────────────────────────────────
    if ctx.wls.surf_ev[ev_idx].needs_ack {
        ctx.wls.surf_ev[ev_idx].needs_ack = false;
        ctx.surfaces[i].nudge_at          = None;
        let serial = ctx.wls.surf_ev[ev_idx].configure_serial;
        ctx.surfaces[i].layer_surface.ack_configure(serial);

        let ew    = ctx.wls.surf_ev[ev_idx].surf_width;
        let eh    = ctx.wls.surf_ev[ev_idx].surf_height;
        let new_w = if ew > 0 { ew } else { ctx.surfaces[i].surf_w };
        let new_h = if eh > 0 { eh } else { ctx.surfaces[i].surf_h };

        if ctx.surfaces[i].pool.is_none() || new_w != ctx.surfaces[i].surf_w || new_h != ctx.surfaces[i].surf_h {
            let qh = ctx.evq.handle();
            match ShmPool::create(&ctx.shm, &qh, new_w, new_h) {
                Ok(mut p) => {
                    p.fill_all_black();
                    ctx.surfaces[i].pool         = Some(p);
                    ctx.surfaces[i].surf_w        = new_w;
                    ctx.surfaces[i].surf_h        = new_h;
                    ctx.surfaces[i].canvas_ready  = false;
                    // Discard stale last_frame: old resolution would corrupt crossfade
                    // alpha blending if a wallpaper change arrives right after resize.
                    ctx.surfaces[i].last_frame.clear();
                    ctx.surfaces[i].last_frame.shrink_to_fit();
                    let active_wp = ctx.surfaces[i].pinned_wp.clone().or_else(|| current_wp.clone());
                    if let Some(ref info) = active_wp {
                        ctx.surfaces[i].decoder = open_decoder(info, new_w, new_h, ctx.surfaces[i].fit, ctx.surfaces[i].hw_ok);
                    }
                }
                Err(e) => tracing::warn!("pool resize surface[{}]: {}", i, e),
            }
        } else if !ctx.surfaces[i].canvas_ready {
            if let Some(p) = ctx.surfaces[i].pool.as_mut() { p.fill_all_black(); }
        }

        let free = ctx.surfaces[i].pool.as_ref().and_then(|p| p.free_idx());
        if let Some(slot) = free {
            commit_slot(ctx, i, slot);
            ctx.wls.surf_ev[ev_idx].frame_ready = false;
        }
        let _ = ctx.evq.flush();
        ctx.surfaces[i].next_frame = Instant::now();
        tracing::debug!("acked surface[{}] {}x{}", i, ctx.surfaces[i].surf_w, ctx.surfaces[i].surf_h);
        return false;
    }

    // ── wait for configure ────────────────────────────────────────────────────
    if !ctx.wls.surf_ev[ev_idx].configured { return false; }

    // ── wait for frame callback + video timing ────────────────────────────────
    let now = Instant::now();
    // Fallback: if no frame callback arrives within 300 ms (e.g. compositor
    // stopped sending them while surface was hidden by a fullscreen window),
    // force frame_ready so rendering resumes when the surface becomes visible.
    // Guard with !is_paused: when paused the surface is intentionally idle;
    // without this guard the 300ms timer fires 3x/second creating useless commits.
    if !ctx.wls.surf_ev[ev_idx].frame_ready && !is_paused {
        let timed_out = ctx.surfaces[i].last_commit_at
            .map(|t| t.elapsed() > Duration::from_millis(300))
            .unwrap_or(false);
        if timed_out {
            ctx.wls.surf_ev[ev_idx].frame_ready = true;
        }
    }
    if !ctx.wls.surf_ev[ev_idx].frame_ready || now < ctx.surfaces[i].next_frame {
        return false;
    }

    // Adaptive frame skip: if we are more than 2 frame-intervals behind wall clock
    // (e.g. system woke from suspend, decoder stalled, or compositor was very slow),
    // skip to now instead of trying to catch up frame-by-frame.  This prevents the
    // "decode storm" where the render loop decodes back-to-back until caught up,
    // starving the compositor and producing visible jitter.
    if ctx.surfaces[i].decoder.is_some() {
        let dur = ctx.surfaces[i].decoder.as_ref().unwrap().frame_duration();
        let two_intervals = dur * 2;
        if ctx.surfaces[i].next_frame + two_intervals < now {
            ctx.surfaces[i].next_frame = now;
            return false; // skip this decode cycle, pick up on the next callback
        }
    }

    // ── pause (fullscreen / battery / lid) ───────────────────────────────────
    if is_paused {
        ctx.wls.surf_ev[ev_idx].frame_ready = true;
        return false;
    }

    // ── idle when no decoder ──────────────────────────────────────────────────
    if ctx.surfaces[i].decoder.is_none() { return false; }

    // ── acquire free buffer slot ──────────────────────────────────────────────
    let Some(slot_i) = ctx.surfaces[i].pool.as_ref().and_then(|p| p.free_idx()) else {
        // Release event for an in-use slot may be sitting in the socket buffer.
        // Drain it inline so the slot is freed before the next loop iteration
        // instead of waiting up to 16 ms for the outer poll.
        if let Some(guard) = ctx.conn.prepare_read() { let _ = guard.read(); }
        let _ = ctx.evq.dispatch_pending(&mut ctx.wls);
        ctx.wls.surf_ev[ev_idx].frame_ready = true;
        return false;
    };
    ctx.wls.surf_ev[ev_idx].frame_ready = false;

    // ── decode into SHM slot ──────────────────────────────────────────────────
    let dur   = ctx.surfaces[i].decoder.as_ref().unwrap().frame_duration();
    let is_hw = ctx.surfaces[i].decoder.as_ref().unwrap().is_hw();
    let (sw, sh) = (ctx.surfaces[i].surf_w, ctx.surfaces[i].surf_h);

    let result = {
        // Single index → one &mut OutputSurface; then split into field borrows.
        let surf = &mut ctx.surfaces[i];
        let dec  = surf.decoder.as_mut().unwrap();
        let pool = surf.pool.as_mut().unwrap();
        dec.next_frame_bgra(pool.slots[slot_i].canvas.pixels_mut())
    };

    match result {
        Ok(true) => {
            ctx.surfaces[i].canvas_ready = true;
            // Extract raw mmap pointer before any field borrows (primitive, no lifetime).
            let (frame_ptr, frame_size) = {
                let c = &ctx.surfaces[i].pool.as_ref().unwrap().slots[slot_i].canvas;
                (c.ptr, c.size)
            };
            // Crossfade: blend old frame into newly decoded frame.
            let fade_left = ctx.surfaces[i].fade_frames_left;
            if fade_left > 0 {
                let fade_total = ctx.surfaces[i].fade_frames_total;
                ctx.surfaces[i].fade_frames_left -= 1;
                // Integer blend: alpha = fade_left/fade_total in 0..=255 range.
                let alpha  = ((fade_left * 255) / fade_total.max(1)) as u16; // old weight
                let ialpha = 255u16 - alpha;                                  // new weight
                let blend_len = frame_size.min(ctx.surfaces[i].fade_old.len());
                let old_ptr   = ctx.surfaces[i].fade_old.as_ptr();
                // SAFETY: frame_ptr is mmap (memfd); old_ptr is heap Vec — no overlap.
                unsafe {
                    let new_sl = std::slice::from_raw_parts_mut(frame_ptr, blend_len);
                    for j in 0..blend_len {
                        let o = *old_ptr.add(j);
                        new_sl[j] = ((o as u16 * alpha + new_sl[j] as u16 * ialpha) / 255) as u8;
                    }
                }
                // Mirror blended output into last_frame every frame during crossfade.
                // Without this, last_frame remains empty (it was moved into fade_old at
                // crossfade start), so a rapid wallpaper switch mid-crossfade finds no
                // snapshot and falls through to a hard cut.  With this copy in place,
                // the new change picks up the current blended frame and continues
                // smoothly into the next crossfade.
                let lf = &mut ctx.surfaces[i].last_frame;
                if lf.len() != blend_len { lf.resize(blend_len, 0); lf.shrink_to_fit(); }
                // SAFETY: frame_ptr (mmap) and lf.as_mut_ptr() (heap Vec) do not overlap.
                unsafe { std::ptr::copy_nonoverlapping(frame_ptr, lf.as_mut_ptr(), blend_len); }
                if ctx.surfaces[i].fade_frames_left == 0 {
                    ctx.surfaces[i].fade_old = Vec::new();
                }
            } else {
                // Update last_frame every LAST_FRAME_UPDATE_INTERVAL frames to save bandwidth.
                ctx.surfaces[i].last_frame_tick += 1;
                if ctx.surfaces[i].last_frame_tick >= LAST_FRAME_UPDATE_INTERVAL {
                    ctx.surfaces[i].last_frame_tick = 0;
                    let lf = &mut ctx.surfaces[i].last_frame;
                    if lf.len() != frame_size {
                        lf.resize(frame_size, 0);
                        // Release excess capacity when resolution decreased (e.g. 4K → 1080p).
                        // Without this, Vec retains the larger allocation permanently.
                        lf.shrink_to_fit();
                    }
                    // SAFETY: frame_ptr is mmap (memfd); lf.as_mut_ptr() is heap Vec.
                    unsafe { std::ptr::copy_nonoverlapping(frame_ptr, lf.as_mut_ptr(), frame_size); }
                }
            }
            commit_slot(ctx, i, slot_i);
            let _ = ctx.evq.flush();
            // Advance by the larger of video frame duration and FPS cap interval.
            // fps_cap_dur reduces wl_shm upload frequency → less compositor CPU
            // load → lower cursor jitter on high-resolution displays.
            let effective_dur = match fps_cap_dur {
                Some(cap) => dur.max(cap),
                None      => dur,
            };
            ctx.surfaces[i].next_frame += effective_dur;
            // Drift clamp: if scheduling jitter pushed next_frame into the past,
            // re-anchor to wall + effective_dur (NOT just wall) so the next frame
            // is always at least one interval away.  Without the +effective_dur
            // the render loop immediately passes the next_frame check on the very
            // next iteration (now ≈ wall), causing paired back-to-back commits
            // that spike compositor load and produce short cursor-jitter bursts.
            let wall = Instant::now();
            if ctx.surfaces[i].next_frame < wall {
                ctx.surfaces[i].next_frame = wall + effective_dur;
            }
            true
        }
        Ok(false) => {
            ctx.wls.surf_ev[ev_idx].frame_ready = true;
            if let Some(dec) = ctx.surfaces[i].decoder.as_mut() {
                if let Err(e) = dec.seek_to_start() {
                    tracing::warn!("seek surface[{}]: {}", i, e);
                    ctx.surfaces[i].decoder = None;
                } else {
                    // Reset timing after seek so the next decode attempt is not
                    // immediate — avoids a spin if seek succeeds but the decoder
                    // immediately returns Ok(false) again (e.g. broken demuxer state).
                    ctx.surfaces[i].next_frame = Instant::now();
                }
            }
            false
        }
        Err(e) => {
            ctx.wls.surf_ev[ev_idx].frame_ready = true;
            if is_hw {
                tracing::warn!("hw surface[{}]: {} — sw fallback (hw disabled for this wallpaper)", i, e);
                ctx.surfaces[i].hw_ok = false; // skip HW on future frames of this wallpaper
                let active_wp = ctx.surfaces[i].pinned_wp.clone().or_else(|| current_wp.clone());
                if let Some(ref info) = active_wp {
                    let fit = ctx.surfaces[i].fit;
                    ctx.surfaces[i].decoder = match VideoDecoder::open(&info.file_path, sw, sh, fit) {
                        Ok(d)  => { tracing::info!("sw fallback surface[{}]", i); Some(AnyDecoder::Sw(d)) }
                        Err(e2) => { tracing::warn!("sw fallback surface[{}]: {}", i, e2); None }
                    };
                }
            } else {
                tracing::warn!("sw surface[{}]: {}", i, e);
                ctx.surfaces[i].decoder = None;
            }
            false
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Detect whether a file path is a static image (not a video or animated GIF).
fn is_static_image(path: &str) -> bool {
    let lower = path.to_lowercase();
    matches!(
        std::path::Path::new(&lower).extension().and_then(|e| e.to_str()),
        Some("jpg" | "jpeg" | "png" | "webp" | "bmp" | "tiff" | "tif")
    )
}

fn open_decoder(info: &WallpaperInfo, target_w: u32, target_h: u32, fit: FitMode, hw_ok: bool) -> Option<AnyDecoder> {
    if is_static_image(&info.file_path) {
        match StaticDecoder::try_open(&info.file_path, target_w, target_h, fit) {
            Some(d) => {
                tracing::info!("static image {}x{}", d.dimensions().0, d.dimensions().1);
                return Some(AnyDecoder::Static(d));
            }
            None => {
                tracing::warn!("static image decode failed: {}", info.file_path);
                return None;
            }
        }
    }

    if hw_ok {
        if let Some(hw) = HwDecoder::try_open(&info.file_path, target_w, target_h) {
            tracing::info!(
                "hw decode (va-api) {}x{} → {}x{} bgra",
                hw.dimensions().0, hw.dimensions().1, target_w, target_h
            );
            return Some(AnyDecoder::Hw(hw));
        }
    }
    match VideoDecoder::open(&info.file_path, target_w, target_h, fit) {
        Ok(sw) => {
            tracing::info!(
                "sw decode {}x{} → {}x{} bgra fit={:?}",
                sw.dimensions().0, sw.dimensions().1, target_w, target_h, fit
            );
            Some(AnyDecoder::Sw(sw))
        }
        Err(e) => { tracing::warn!("decoder open: {}", e); None }
    }
}
