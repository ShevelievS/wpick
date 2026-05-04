// Wallpaper renderer — wl_shm CPU path, Hyprland-aware surface recovery.
//
// Sprint-1 changes vs previous version:
//
//   Buffer pool (3 slots):
//     ShmPool replaces the single ShmCanvas.  Each slot tracks its own
//     `in_use: Arc<AtomicBool>`.  wl_buffer::Release sets the flag to false,
//     telling us the compositor has finished reading that slot and we can
//     decode the next frame into it.  This prevents writing to a buffer that
//     the compositor is still scanning out (tearing / corruption risk).
//
//   Frame callbacks:
//     After every commit we request a wl_surface::frame callback.  The render
//     loop waits for `frame_ready = true` before decoding the next frame.
//     This paces us to the compositor's refresh cycle instead of using a
//     fixed sleep, eliminating wasted decodes and avoiding frame starvation.
//
//   Decoder-side scaling (video.rs / hw_decode.rs):
//     Both decoders now accept target_w / target_h and produce BGRA at that
//     size.  The render loop copies the decoder output directly into the SHM
//     slot — no more per-pixel loops in renderer.rs.
//
// Hyprland fullscreen recovery (unchanged):
//   A) Surface still alive → nudge (re-set layer props + commit).
//      Triggers a fresh Configure; ack + commit makes it visible again.
//      Fallback: if no Configure within 2 s → path B.
//   B) Surface was Closed during fullscreen → recreate AFTER fullscreen exits.

use std::os::unix::io::{AsFd, AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::sync::{broadcast, watch};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{
        wl_buffer, wl_callback, wl_compositor, wl_output, wl_registry,
        wl_shm, wl_shm_pool, wl_surface,
    },
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};
use wpick_core::model::WallpaperInfo;

use crate::hw_decode::HwDecoder;
use crate::video::VideoDecoder;

// ─── Hyprland fullscreen monitor ─────────────────────────────────────────────

fn start_fullscreen_monitor() -> Arc<AtomicBool> {
    let flag  = Arc::new(AtomicBool::new(false));
    let flag2 = Arc::clone(&flag);
    std::thread::Builder::new().name("fs-mon".into()).spawn(move || {
        let sig = match std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
            Ok(s) => s,
            Err(_) => { tracing::info!("not Hyprland — fullscreen detection off"); return; }
        };
        let path = format!("/tmp/hypr/{}/.socket2.sock", sig);
        loop {
            if let Ok(stream) = std::os::unix::net::UnixStream::connect(&path) {
                use std::io::BufRead;
                for line in std::io::BufReader::new(stream).lines() {
                    match line {
                        Ok(l) if l.starts_with("fullscreen>>") => {
                            let p = l.trim_start_matches("fullscreen>>");
                            let active = !p.starts_with('0') && !p.contains(",0");
                            flag2.store(active, Ordering::Relaxed);
                            tracing::debug!("fullscreen → {}", active);
                        }
                        Ok(_)  => {}
                        Err(e) => { tracing::debug!("hyprland ipc: {}", e); break; }
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }).ok();
    flag
}

// ─── Wayland state ────────────────────────────────────────────────────────────

struct WaylandState {
    compositor:       Option<wl_compositor::WlCompositor>,
    layer_shell:      Option<ZwlrLayerShellV1>,
    shm:              Option<wl_shm::WlShm>,
    _output:          Option<wl_output::WlOutput>,
    output_width:     u32,
    output_height:    u32,
    configured:       bool,
    configure_serial: u32,
    surf_width:       u32,
    surf_height:      u32,
    needs_ack:        bool,
    closed:           bool,
    /// Set to true when the compositor sends wl_callback::done.
    /// The render loop clears it after each commit and waits for the next one.
    frame_ready:      bool,
}

impl Default for WaylandState {
    fn default() -> Self {
        Self {
            compositor: None, layer_shell: None, shm: None, _output: None,
            output_width: 1920, output_height: 1080,
            configured: false, configure_serial: 0,
            surf_width: 0, surf_height: 0,
            needs_ack: false, closed: false,
            frame_ready: true, // allow first frame immediately
        }
    }
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
                "wl_output" if state._output.is_none() => {
                    state._output = Some(registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_compositor::WlCompositor, _: wl_compositor::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
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

/// Frame callback: compositor signals it is ready for the next frame.
impl Dispatch<wl_callback::WlCallback, ()> for WaylandState {
    fn event(state: &mut Self, _: &wl_callback::WlCallback, event: wl_callback::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_ready = true;
        }
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for WaylandState {
    fn event(_: &mut Self, _: &ZwlrLayerShellV1, _: zwlr_layer_shell_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}
impl Dispatch<ZwlrLayerSurfaceV1, ()> for WaylandState {
    fn event(state: &mut Self, _: &ZwlrLayerSurfaceV1, event: zwlr_layer_surface_v1::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                state.configure_serial = serial;
                state.surf_width       = width;
                state.surf_height      = height;
                state.configured       = true;
                state.needs_ack        = true;
            }
            zwlr_layer_surface_v1::Event::Closed => {
                tracing::warn!("layer_surface closed");
                state.closed = true;
            }
            _ => {}
        }
    }
}
impl Dispatch<wl_output::WlOutput, ()> for WaylandState {
    fn event(state: &mut Self, _: &wl_output::WlOutput, event: wl_output::Event,
             _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_output::Event::Mode { flags, width, height, .. } = event {
            use wayland_client::WEnum;
            if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) && width > 0 && height > 0 {
                state.output_width  = width  as u32;
                state.output_height = height as u32;
            }
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
        let stride = w * 4;
        let size   = (stride * h) as usize;
        let path   = format!(
            "/tmp/.wpick-shm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let file = std::fs::OpenOptions::new()
            .read(true).write(true).create(true)
            .open(&path).context("shm open")?;
        std::fs::remove_file(&path).ok();
        file.set_len(size as u64).context("set_len")?;
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(), size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(), 0,
            )
        };
        anyhow::ensure!(ptr != libc::MAP_FAILED, "mmap: {}", std::io::Error::last_os_error());

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

// ─── AnyDecoder ──────────────────────────────────────────────────────────────

enum AnyDecoder { Hw(HwDecoder), Sw(VideoDecoder) }

impl AnyDecoder {
    fn next_frame_bgra(&mut self, dst: &mut [u8]) -> anyhow::Result<bool> {
        match self {
            AnyDecoder::Hw(d) => d.next_frame_bgra(dst),
            AnyDecoder::Sw(d) => d.next_frame_bgra(dst),
        }
    }
    fn seek_to_start(&mut self) -> anyhow::Result<()> {
        match self { AnyDecoder::Hw(d) => d.seek_to_start(), AnyDecoder::Sw(d) => d.seek_to_start() }
    }
    fn frame_duration(&self) -> Duration {
        match self { AnyDecoder::Hw(d) => d.frame_duration(), AnyDecoder::Sw(d) => d.frame_duration() }
    }
    fn is_hw(&self) -> bool { matches!(self, AnyDecoder::Hw(_)) }
}

// ─── Renderer context ─────────────────────────────────────────────────────────

struct RendererCtx {
    _conn:         Connection,
    evq:           wayland_client::EventQueue<WaylandState>,
    wls:           WaylandState,
    compositor:    wl_compositor::WlCompositor,
    layer_shell:   ZwlrLayerShellV1,
    wl_surface:    wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    shm:           wl_shm::WlShm,
    pool:          ShmPool,
    surf_w:        u32,
    surf_h:        u32,
}

// ─── Surface helpers ──────────────────────────────────────────────────────────

fn make_layer_surface(
    compositor:  &wl_compositor::WlCompositor,
    layer_shell: &ZwlrLayerShellV1,
    qh:          &QueueHandle<WaylandState>,
) -> (wl_surface::WlSurface, ZwlrLayerSurfaceV1) {
    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
    let wl = compositor.create_surface(qh, ());
    let ls = layer_shell.get_layer_surface(&wl, None, Layer::Bottom, "wpick".into(), qh, ());
    ls.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    ls.set_size(0, 0);
    ls.set_exclusive_zone(-1);
    wl.commit();
    (wl, ls)
}

fn nudge_layer_surface(ctx: &mut RendererCtx) {
    ctx.layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    ctx.layer_surface.set_size(0, 0);
    ctx.layer_surface.set_exclusive_zone(-1);
    ctx.wl_surface.commit();
    ctx.wls.configured = false;
    ctx.wls.needs_ack  = false;
    let _ = ctx.evq.flush();
}

fn recreate_surface(ctx: &mut RendererCtx) {
    let qh = ctx.evq.handle();
    let (new_wl, new_ls) = make_layer_surface(&ctx.compositor, &ctx.layer_shell, &qh);
    let old_ls = std::mem::replace(&mut ctx.layer_surface, new_ls);
    let old_wl = std::mem::replace(&mut ctx.wl_surface, new_wl);
    drop(old_ls);
    drop(old_wl);
    ctx.wls.configured  = false;
    ctx.wls.needs_ack   = false;
    ctx.wls.closed      = false;
    ctx.wls.frame_ready = true; // fresh surface — allow immediate render
    let _ = ctx.evq.flush();
    tracing::info!("layer_surface recreated");
}

/// Commit a black slot to give the surface content before the first video frame.
/// Does NOT request a frame callback (keep frame_ready = true for fast first frame).
fn commit_slot(ctx: &mut RendererCtx, idx: usize) {
    ctx.pool.slots[idx].in_use.store(true, Ordering::Release);
    let qh = ctx.evq.handle();
    ctx.wl_surface.attach(Some(&ctx.pool.slots[idx].canvas.buffer), 0, 0);
    ctx.wl_surface.damage_buffer(0, 0, ctx.surf_w as i32, ctx.surf_h as i32);
    ctx.wl_surface.frame(&qh, ());
    ctx.wl_surface.commit();
}

// ─── Init ─────────────────────────────────────────────────────────────────────

fn init_renderer() -> anyhow::Result<RendererCtx> {
    let conn    = Connection::connect_to_env().context("wayland connect")?;
    let mut evq = conn.new_event_queue::<WaylandState>();
    let qh      = evq.handle();
    let mut wls = WaylandState::default();

    conn.display().get_registry(&qh, ());
    evq.roundtrip(&mut wls).context("globals roundtrip")?;

    let compositor  = wls.compositor.take().ok_or_else(|| anyhow::anyhow!("no wl_compositor"))?;
    let layer_shell = wls.layer_shell.take().ok_or_else(|| anyhow::anyhow!("no zwlr_layer_shell_v1"))?;
    let shm         = wls.shm.take().ok_or_else(|| anyhow::anyhow!("no wl_shm"))?;

    let (wl_surface, layer_surface) = make_layer_surface(&compositor, &layer_shell, &qh);

    evq.roundtrip(&mut wls).context("configure roundtrip")?;
    anyhow::ensure!(wls.configured, "layer surface not configured");

    let surf_w = if wls.surf_width  > 0 { wls.surf_width  } else { wls.output_width  };
    let surf_h = if wls.surf_height > 0 { wls.surf_height } else { wls.output_height };

    let mut pool = ShmPool::create(&shm, &qh, surf_w, surf_h).context("shm pool")?;
    pool.fill_all_black();

    // Initial commit: show black surface immediately, then request first callback.
    layer_surface.ack_configure(wls.configure_serial);
    pool.slots[0].in_use.store(true, Ordering::Release);
    wl_surface.attach(Some(&pool.slots[0].canvas.buffer), 0, 0);
    wl_surface.damage_buffer(0, 0, surf_w as i32, surf_h as i32);
    wl_surface.frame(&qh, ());
    wl_surface.commit();
    evq.flush().context("flush")?;
    wls.needs_ack   = false;
    wls.frame_ready = false; // wait for first callback before rendering

    tracing::info!("renderer ready {}x{} (pool: 3 slots)", surf_w, surf_h);
    Ok(RendererCtx { _conn: conn, evq, wls, compositor, layer_shell, wl_surface, layer_surface, shm, pool, surf_w, surf_h })
}

// ─── Async wrapper ────────────────────────────────────────────────────────────

pub async fn run(
    mut wallpaper_rx: watch::Receiver<Option<WallpaperInfo>>,
    shutdown_rx:      broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    let mut shutdown_rx   = shutdown_rx;
    let mut init_failures = 0u32;
    let fullscreen        = start_fullscreen_monitor();

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

        let (wp_tx, wp_rx) = std::sync::mpsc::channel::<Option<WallpaperInfo>>();
        let (sd_tx, sd_rx) = std::sync::mpsc::channel::<()>();
        let _ = wp_tx.send(wallpaper_rx.borrow_and_update().clone());

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

        let fs = Arc::clone(&fullscreen);
        let result = tokio::task::spawn_blocking(move || render_loop(ctx, wp_rx, sd_rx, fs))
            .await.context("render loop panic")?;

        wp_fwd.abort();
        sd_fwd.abort();

        match result {
            Ok(()) => break,
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

// ─── Render loop ──────────────────────────────────────────────────────────────

const NUDGE_TIMEOUT: Duration = Duration::from_secs(2);

fn render_loop(
    mut ctx:    RendererCtx,
    wp_rx:      std::sync::mpsc::Receiver<Option<WallpaperInfo>>,
    sd_rx:      std::sync::mpsc::Receiver<()>,
    fullscreen: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut decoder:       Option<AnyDecoder>    = None;
    let mut current_wp:    Option<WallpaperInfo> = None;
    let mut next_frame:    Instant               = Instant::now();
    let mut canvas_ready:  bool                  = false;
    let mut in_fullscreen: bool                  = false;
    let mut closed_in_fs:  bool                  = false;
    let mut nudge_at:      Option<Instant>       = None;

    loop {
        // ── shutdown ──────────────────────────────────────────────────────────
        if sd_rx.try_recv().is_ok() { break; }

        // ── wayland events ────────────────────────────────────────────────────
        if let Err(e) = ctx.evq.dispatch_pending(&mut ctx.wls) {
            tracing::warn!("wayland connection lost: {} — fatal", e);
            anyhow::bail!("__fatal__");
        }
        let _ = ctx.evq.flush();

        // ── wallpaper change ──────────────────────────────────────────────────
        while let Ok(new_wp) = wp_rx.try_recv() {
            decoder      = None;
            canvas_ready = false;
            if let Some(info) = new_wp {
                tracing::info!("wallpaper: {}", info.title);
                decoder    = open_decoder(&info, ctx.surf_w, ctx.surf_h);
                current_wp = Some(info);
                next_frame = Instant::now();
            } else {
                current_wp = None;
            }
        }

        // ── configure ack ─────────────────────────────────────────────────────
        if ctx.wls.needs_ack {
            ctx.wls.needs_ack = false;
            ctx.layer_surface.ack_configure(ctx.wls.configure_serial);

            let new_w = if ctx.wls.surf_width  > 0 { ctx.wls.surf_width  } else { ctx.surf_w };
            let new_h = if ctx.wls.surf_height > 0 { ctx.wls.surf_height } else { ctx.surf_h };

            if new_w != ctx.surf_w || new_h != ctx.surf_h {
                let qh = ctx.evq.handle();
                match ShmPool::create(&ctx.shm, &qh, new_w, new_h) {
                    Ok(mut new_pool) => {
                        new_pool.fill_all_black();
                        ctx.pool   = new_pool;
                        ctx.surf_w = new_w;
                        ctx.surf_h = new_h;
                        canvas_ready = false;
                        // Decoder must be recreated for new target dimensions.
                        if let Some(ref info) = current_wp {
                            decoder = open_decoder(info, new_w, new_h);
                        }
                    }
                    Err(e) => tracing::warn!("pool resize: {}", e),
                }
            } else if !canvas_ready {
                ctx.pool.fill_all_black();
            }

            // Commit a free black slot so the surface always has content.
            if let Some(idx) = ctx.pool.free_idx() {
                commit_slot(&mut ctx, idx);
                ctx.wls.frame_ready = false;
            }
            let _ = ctx.evq.flush();

            nudge_at   = None;
            next_frame = Instant::now();
            tracing::debug!("configure acked {}x{}", ctx.surf_w, ctx.surf_h);
        }

        // ── surface closed ────────────────────────────────────────────────────
        if ctx.wls.closed {
            ctx.wls.closed     = false;
            ctx.wls.configured = false;
            canvas_ready       = false;
            if in_fullscreen {
                closed_in_fs = true;
                tracing::info!("Closed during fullscreen — deferring recreation");
            } else {
                recreate_surface(&mut ctx);
            }
        }

        // ── fullscreen transition ─────────────────────────────────────────────
        let is_fs = fullscreen.load(Ordering::Relaxed);
        if in_fullscreen && !is_fs {
            in_fullscreen = false;
            tracing::info!("fullscreen exited — recovering surface");
            if closed_in_fs {
                closed_in_fs = false;
                recreate_surface(&mut ctx);
            } else {
                nudge_layer_surface(&mut ctx);
                nudge_at = Some(Instant::now());
            }
            continue;
        }
        in_fullscreen = is_fs;

        // ── nudge fallback ────────────────────────────────────────────────────
        if let Some(t) = nudge_at {
            if !ctx.wls.configured && t.elapsed() > NUDGE_TIMEOUT {
                tracing::warn!("nudge timed out — recreating surface");
                nudge_at = None;
                recreate_surface(&mut ctx);
            }
        }

        // ── wait for configure ────────────────────────────────────────────────
        if !ctx.wls.configured {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }

        // ── wait for frame callback + video timing ────────────────────────────
        let now = Instant::now();
        if !ctx.wls.frame_ready || now < next_frame {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }

        // ── idle when no decoder ──────────────────────────────────────────────
        let Some(dec) = decoder.as_mut() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };

        // ── acquire free buffer slot ──────────────────────────────────────────
        let Some(idx) = ctx.pool.free_idx() else {
            // All slots held by compositor — defer until one is released.
            std::thread::sleep(Duration::from_millis(1));
            // Restore frame_ready so we don't skip the frame once a slot frees.
            ctx.wls.frame_ready = true;
            continue;
        };
        ctx.wls.frame_ready = false;

        // ── decode directly into the SHM slot ────────────────────────────────
        let dur = dec.frame_duration();
        let (sw, sh) = (ctx.surf_w, ctx.surf_h);
        let is_hw = dec.is_hw();

        let decode_result = dec.next_frame_bgra(ctx.pool.slots[idx].canvas.pixels_mut());

        match decode_result {
            Ok(true) => {
                canvas_ready = true;
                // Commit slot + request next frame callback.
                commit_slot(&mut ctx, idx);
                let _ = ctx.evq.flush();
                next_frame += dur;
            }
            Ok(false) => {
                // EOF — seek to start for seamless loop.
                ctx.wls.frame_ready = true;
                if let Err(e) = dec.seek_to_start() {
                    tracing::warn!("seek: {}", e);
                    decoder = None;
                }
            }
            Err(e) => {
                ctx.wls.frame_ready = true;
                if is_hw {
                    tracing::warn!("hw: {} — sw fallback", e);
                    if let Some(ref info) = current_wp {
                        match VideoDecoder::open(&info.file_path, sw, sh) {
                            Ok(sw_dec) => {
                                tracing::info!("sw fallback decoder opened");
                                decoder = Some(AnyDecoder::Sw(sw_dec));
                            }
                            Err(e2) => {
                                tracing::warn!("sw fallback failed: {}", e2);
                                decoder = None;
                            }
                        }
                    }
                } else {
                    tracing::warn!("sw: {}", e);
                    decoder = None;
                }
            }
        }

        // keep canvas_ready in scope (suppresses unused warning)
        let _ = canvas_ready;
    }

    drop(ctx.layer_surface);
    drop(ctx.wl_surface);
    let _ = ctx.evq.flush();
    Ok(())
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn open_decoder(info: &WallpaperInfo, target_w: u32, target_h: u32) -> Option<AnyDecoder> {
    if let Some(hw) = HwDecoder::try_open(&info.file_path, target_w, target_h) {
        tracing::info!(
            "hw decode (va-api) {}x{} → {}x{} bgra",
            hw.dimensions().0, hw.dimensions().1, target_w, target_h
        );
        return Some(AnyDecoder::Hw(hw));
    }
    match VideoDecoder::open(&info.file_path, target_w, target_h) {
        Ok(sw) => {
            tracing::info!(
                "sw decode {}x{} → {}x{} bgra",
                sw.dimensions().0, sw.dimensions().1, target_w, target_h
            );
            Some(AnyDecoder::Sw(sw))
        }
        Err(e) => { tracing::warn!("decoder open: {}", e); None }
    }
}
