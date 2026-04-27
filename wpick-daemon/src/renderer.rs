// Wallpaper renderer — wl_shm (CPU shared memory) path.
//
// Replaces the wgpu/Vulkan approach. Core insight: every blocking issue we
// encountered (vkAcquireNextImageKHR hanging on NVIDIA when Hyprland hides
// the surface during fullscreen) comes from the GPU synchronisation model.
// With wl_shm we write directly to shared memory and the compositor reads it
// at its own pace — no Vulkan fence, no frame callback starvation, no deadlock.
//
// Render model:
//   decode frame → write BGRA to mmap'd canvas → attach + damage + frame(cb)
//   + commit → wait for cb (compositor composited) → decode next frame …
//
// If the frame callback times out (surface hidden behind fullscreen) we stop
// rendering but keep the Wayland event loop running.  Compositor sends
// Configure when fullscreen exits; we ack it immediately (no GPU in the way)
// and resume rendering on the next callback.

use std::os::unix::io::{AsFd, AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::sync::{broadcast, watch};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
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

use crate::hw_decode::{HwDecoder, Nv12Frame};
use crate::video::VideoDecoder;

// ─── Wayland dispatch state ───────────────────────────────────────────────────

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
    /// Set by the wl_callback dispatcher when the compositor fires done.
    frame_done:       bool,
    /// Set by the wl_buffer dispatcher when the compositor releases the buffer.
    buffer_released:  bool,
}

impl Default for WaylandState {
    fn default() -> Self {
        Self {
            compositor:      None,
            layer_shell:     None,
            shm:             None,
            _output:         None,
            output_width:    1920,
            output_height:   1080,
            configured:      false,
            configure_serial: 0,
            surf_width:      0,
            surf_height:     0,
            needs_ack:       false,
            closed:          false,
            frame_done:      false,
            buffer_released: true,
        }
    }
}

// ─── Dispatch implementations ─────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<WaylandState>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" => {
                    state.shm = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "wl_output" => {
                    if state._output.is_none() {
                        state._output = Some(
                            registry.bind::<wl_output::WlOutput, _, _>(name, version.min(4), qh, ()),
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_compositor::WlCompositor, _: wl_compositor::Event,
             _: &(), _: &Connection, _: &QueueHandle<WaylandState>) {}
}

impl Dispatch<wl_surface::WlSurface, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_surface::WlSurface, _: wl_surface::Event,
             _: &(), _: &Connection, _: &QueueHandle<WaylandState>) {}
}

impl Dispatch<wl_shm::WlShm, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_shm::WlShm, _: wl_shm::Event,
             _: &(), _: &Connection, _: &QueueHandle<WaylandState>) {}
}

impl Dispatch<wl_shm_pool::WlShmPool, ()> for WaylandState {
    fn event(_: &mut Self, _: &wl_shm_pool::WlShmPool, _: wl_shm_pool::Event,
             _: &(), _: &Connection, _: &QueueHandle<WaylandState>) {}
}

impl Dispatch<wl_buffer::WlBuffer, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &wl_buffer::WlBuffer,
        event: wl_buffer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WaylandState>,
    ) {
        if matches!(event, wl_buffer::Event::Release) {
            state.buffer_released = true;
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WaylandState>,
    ) {
        if matches!(event, wl_callback::Event::Done { .. }) {
            state.frame_done = true;
        }
    }
}

impl Dispatch<ZwlrLayerShellV1, ()> for WaylandState {
    fn event(_: &mut Self, _: &ZwlrLayerShellV1, _: zwlr_layer_shell_v1::Event,
             _: &(), _: &Connection, _: &QueueHandle<WaylandState>) {}
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WaylandState>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                state.configure_serial = serial;
                state.surf_width       = width;
                state.surf_height      = height;
                state.configured       = true;
                state.needs_ack        = true;
            }
            zwlr_layer_surface_v1::Event::Closed => {
                tracing::warn!("Layer surface closed by compositor — will reinitialise");
                state.closed = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for WaylandState {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<WaylandState>,
    ) {
        if let wl_output::Event::Mode { flags, width, height, .. } = event {
            use wayland_client::WEnum;
            if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current))
                && width > 0 && height > 0
            {
                state.output_width  = width  as u32;
                state.output_height = height as u32;
            }
        }
    }
}

// ─── Shared memory canvas ─────────────────────────────────────────────────────

struct ShmCanvas {
    width:   u32,
    height:  u32,
    _fd:     OwnedFd,
    ptr:     *mut u8,
    size:    usize,
    buffer:  wl_buffer::WlBuffer,
    _pool:   wl_shm_pool::WlShmPool,
}

// SAFETY: ShmCanvas exclusively owns its fd and mmap region.
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
        width:  u32,
        height: u32,
    ) -> anyhow::Result<Self> {
        let stride = width * 4;
        let size   = (stride * height) as usize;

        // Anonymous backing file (works everywhere, no memfd_create dependency)
        let path = format!(
            "/tmp/.wpick-shm-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos(),
        );
        let file = std::fs::OpenOptions::new()
            .read(true).write(true).create(true)
            .open(&path)
            .context("create shm file")?;
        // Unlink immediately — the fd keeps the backing store alive.
        std::fs::remove_file(&path).ok();
        file.set_len(size as u64).context("set_len")?;

        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        anyhow::ensure!(ptr != libc::MAP_FAILED, "mmap failed: {}", std::io::Error::last_os_error());

        let pool   = shm.create_pool(file.as_fd(), size as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32, height as i32,
            stride as i32,
            wl_shm::Format::Argb8888,
            qh, (),
        );

        Ok(Self {
            width,
            height,
            _fd:   OwnedFd::from(file),
            ptr:   ptr as *mut u8,
            size,
            buffer,
            _pool: pool,
        })
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        // SAFETY: we own the mapping exclusively during this borrow.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.size) }
    }
}

// ─── Decoder abstraction ──────────────────────────────────────────────────────

enum AnyDecoder {
    Hw(HwDecoder),
    Sw(VideoDecoder),
}

impl AnyDecoder {
    fn seek_to_start(&mut self) -> anyhow::Result<()> {
        match self {
            AnyDecoder::Hw(d) => d.seek_to_start(),
            AnyDecoder::Sw(d) => d.seek_to_start(),
        }
    }

    fn frame_duration(&self) -> Duration {
        match self {
            AnyDecoder::Hw(d) => d.frame_duration(),
            AnyDecoder::Sw(d) => d.frame_duration(),
        }
    }
}

// ─── Renderer context ─────────────────────────────────────────────────────────

struct RendererCtx {
    _conn:         Connection,
    evq:           wayland_client::EventQueue<WaylandState>,
    wls:           WaylandState,
    wl_surface:    wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    shm:           wl_shm::WlShm,
    canvas:        ShmCanvas,
    surf_w:        u32,
    surf_h:        u32,
}

// ─── Blocking init ────────────────────────────────────────────────────────────

fn init_renderer() -> anyhow::Result<RendererCtx> {
    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;

    tracing::debug!("init_renderer — connecting to Wayland");

    let conn    = Connection::connect_to_env().context("Connect to Wayland display")?;
    let mut evq = conn.new_event_queue::<WaylandState>();
    let qh      = evq.handle();
    let mut wls = WaylandState::default();

    conn.display().get_registry(&qh, ());
    evq.roundtrip(&mut wls).context("globals roundtrip")?;
    tracing::debug!("globals — compositor={} layer_shell={} shm={}",
        wls.compositor.is_some(), wls.layer_shell.is_some(), wls.shm.is_some());

    let compositor  = wls.compositor.take()
        .ok_or_else(|| anyhow::anyhow!("No wl_compositor global"))?;
    let layer_shell = wls.layer_shell.take()
        .ok_or_else(|| anyhow::anyhow!(
            "No zwlr_layer_shell_v1 — use Hyprland/Sway/river (not GNOME/KDE)"
        ))?;
    let shm = wls.shm.take()
        .ok_or_else(|| anyhow::anyhow!("No wl_shm global"))?;

    let wl_surface    = compositor.create_surface(&qh, ());
    let layer_surface = layer_shell.get_layer_surface(
        &wl_surface, None, Layer::Bottom, "wpick".to_string(), &qh, (),
    );
    layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer_surface.set_size(0, 0);
    layer_surface.set_exclusive_zone(-1);
    wl_surface.commit();

    evq.roundtrip(&mut wls).context("configure roundtrip")?;
    anyhow::ensure!(wls.configured, "Layer surface not configured");

    let surf_w = if wls.surf_width  > 0 { wls.surf_width  } else { wls.output_width  };
    let surf_h = if wls.surf_height > 0 { wls.surf_height } else { wls.output_height };

    layer_surface.ack_configure(wls.configure_serial);
    wl_surface.commit();
    evq.flush().context("flush after ack_configure")?;
    wls.needs_ack = false;

    tracing::debug!("Wayland surface ready: {}x{}", surf_w, surf_h);

    let canvas = ShmCanvas::create(&shm, &qh, surf_w, surf_h)
        .context("create shm canvas")?;

    tracing::info!("Renderer ready (wl_shm {}x{})", surf_w, surf_h);

    Ok(RendererCtx { _conn: conn, evq, wls, wl_surface, layer_surface, shm, canvas, surf_w, surf_h })
}

// ─── Public async entry point ─────────────────────────────────────────────────

pub async fn run(
    mut wallpaper_rx: watch::Receiver<Option<WallpaperInfo>>,
    shutdown_rx:      broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    tracing::debug!("renderer::run started");

    let mut shutdown_rx    = shutdown_rx;
    let mut init_failures  = 0u32;

    loop {
        let ctx = match tokio::task::spawn_blocking(init_renderer).await
            .context("renderer init thread panicked")?
        {
            Ok(ctx) => { init_failures = 0; ctx }
            Err(e)  => {
                init_failures += 1;
                tracing::warn!("Renderer init failed (attempt {}): {}", init_failures, e);
                if init_failures >= 10 {
                    return Err(e.context("Renderer failed to initialise after 10 attempts"));
                }
                if shutdown_rx.try_recv().is_ok() { break; }
                let delay = Duration::from_millis(500 * init_failures.min(4) as u64);
                tokio::time::sleep(delay).await;
                if shutdown_rx.try_recv().is_ok() { break; }
                continue;
            }
        };

        let (wp_tx, wp_rx) = std::sync::mpsc::channel::<Option<WallpaperInfo>>();
        let (sd_tx, sd_rx) = std::sync::mpsc::channel::<()>();

        let _ = wp_tx.send(wallpaper_rx.borrow_and_update().clone());

        let mut wallpaper_rx2 = wallpaper_rx.clone();
        let wp_forward = tokio::spawn(async move {
            loop {
                if wallpaper_rx2.changed().await.is_err() { break; }
                let val = wallpaper_rx2.borrow_and_update().clone();
                if wp_tx.send(val).is_err() { break; }
            }
        });

        let mut sd_shutdown = shutdown_rx.resubscribe();
        let sd_forward = tokio::spawn(async move {
            let _ = sd_shutdown.recv().await;
            let _ = sd_tx.send(());
        });

        let result = tokio::task::spawn_blocking(move || render_loop(ctx, wp_rx, sd_rx))
            .await
            .context("render loop thread panicked")?;

        wp_forward.abort();
        sd_forward.abort();

        match result {
            Ok(()) => break,
            Err(ref e) if e.to_string().contains("__layer_surface_closed__") => {
                if shutdown_rx.try_recv().is_ok() { break; }
                tracing::info!("Renderer: layer surface closed — reinitialising");
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    Ok(())
}

// ─── Synchronous render loop ──────────────────────────────────────────────────

fn render_loop(
    mut ctx: RendererCtx,
    wp_rx:   std::sync::mpsc::Receiver<Option<WallpaperInfo>>,
    sd_rx:   std::sync::mpsc::Receiver<()>,
) -> anyhow::Result<()> {
    let mut decoder:     Option<AnyDecoder>    = None;
    let mut current_wp:  Option<WallpaperInfo> = None;
    let mut next_frame:  Instant               = Instant::now();
    // Frame callback state: pending_cb keeps the WlCallback alive until done fires.
    let mut pending_cb:  Option<wl_callback::WlCallback> = None;
    let mut cb_deadline: Option<Instant>                 = None;
    const HIDDEN_TIMEOUT: Duration = Duration::from_secs(2);

    tracing::debug!("render_loop running (wl_shm)");

    loop {
        // ── 1. Shutdown ───────────────────────────────────────────────────────
        if sd_rx.try_recv().is_ok() {
            tracing::info!("Renderer shutting down");
            break;
        }

        // ── 2. Wayland events — ALWAYS, even while waiting for callback ───────
        // This is what breaks the fullscreen deadlock: Configure acks are sent
        // here even when we're in the "waiting for frame callback" state below.
        if let Err(e) = ctx.evq.dispatch_pending(&mut ctx.wls) {
            tracing::warn!("dispatch_pending: {}", e);
        }
        if let Err(e) = ctx.evq.flush() {
            tracing::warn!("evq flush: {}", e);
        }

        // ── 3. Frame callback received ────────────────────────────────────────
        if ctx.wls.frame_done {
            ctx.wls.frame_done = false;
            pending_cb  = None;
            cb_deadline = None;
        }

        // ── 4. Wallpaper change ───────────────────────────────────────────────
        while let Ok(new_wp) = wp_rx.try_recv() {
            decoder    = None;
            pending_cb = None;
            cb_deadline = None;
            if let Some(info) = new_wp {
                tracing::debug!("loading wallpaper: {}", info.title);
                decoder    = open_decoder(&info);
                current_wp = Some(info);
                next_frame = Instant::now();
            } else {
                current_wp = None;
            }
        }

        // ── 5. Handle compositor Configure ────────────────────────────────────
        if ctx.wls.needs_ack {
            ctx.wls.needs_ack = false;
            ctx.layer_surface.ack_configure(ctx.wls.configure_serial);

            let new_w = if ctx.wls.surf_width  > 0 { ctx.wls.surf_width  } else { ctx.surf_w };
            let new_h = if ctx.wls.surf_height > 0 { ctx.wls.surf_height } else { ctx.surf_h };

            if new_w != ctx.surf_w || new_h != ctx.surf_h {
                // Resize canvas to new dimensions.
                let qh = ctx.evq.handle();
                match ShmCanvas::create(&ctx.shm, &qh, new_w, new_h) {
                    Ok(c)  => {
                        ctx.canvas  = c;
                        ctx.surf_w  = new_w;
                        ctx.surf_h  = new_h;
                        pending_cb  = None;
                        cb_deadline = None;
                        next_frame  = Instant::now();
                        tracing::info!("Canvas resized to {}x{}", new_w, new_h);
                    }
                    Err(e) => tracing::warn!("Canvas resize failed: {}", e),
                }
            } else {
                tracing::info!("Acked configure ({}x{} unchanged)", new_w, new_h);
            }

            // Commit with no buffer — the compositor just needs to see our ack.
            // The next wl_shm render will attach a real buffer.
            ctx.wl_surface.commit();
            if let Err(e) = ctx.evq.flush() {
                tracing::warn!("flush after ack: {}", e);
            }
        }

        // ── 6. Layer surface closed ───────────────────────────────────────────
        if ctx.wls.closed {
            tracing::warn!("Layer surface closed — requesting renderer restart");
            drop(ctx.layer_surface);
            drop(ctx.wl_surface);
            let _ = ctx.evq.flush();
            anyhow::bail!("__layer_surface_closed__");
        }

        // ── 7. Frame callback guard ───────────────────────────────────────────
        // While a callback is pending: sleep briefly so the event loop above
        // keeps running (picking up Configure acks, buffer releases, etc.).
        // If the deadline passes the compositor is not compositing us (surface
        // hidden behind fullscreen) — keep looping without rendering.
        if pending_cb.is_some() {
            let deadline = *cb_deadline.get_or_insert_with(|| Instant::now() + HIDDEN_TIMEOUT);
            if Instant::now() < deadline {
                // Compositor will callback soon — wait 4 ms and loop.
                std::thread::sleep(Duration::from_millis(4));
            } else {
                // Surface hidden: sleep longer, keep Wayland events flowing.
                std::thread::sleep(Duration::from_millis(50));
            }
            continue;
        }

        // ── 8. Render ─────────────────────────────────────────────────────────
        if let Some(dec) = decoder.as_mut() {
            let now = Instant::now();
            if now < next_frame {
                let wait = (next_frame - now).min(Duration::from_millis(8));
                std::thread::sleep(wait);
                continue;
            }

            let dur        = dec.frame_duration();
            let canvas_w   = ctx.canvas.width;
            let canvas_h   = ctx.canvas.height;
            let pixels     = ctx.canvas.pixels_mut();
            let eof;

            match dec {
                AnyDecoder::Hw(hw) => {
                    match hw.next_nv12_frame() {
                        Ok(Some(frame)) => {
                            write_nv12_to_bgra(pixels, canvas_w, canvas_h, &frame);
                            eof = false;
                        }
                        Ok(None) => { eof = true; }
                        Err(e) => {
                            tracing::warn!("HW decode error: {} — falling back to SW", e);
                            if let Some(ref info) = current_wp {
                                decoder = open_decoder(info);
                            }
                            continue;
                        }
                    }
                }
                AnyDecoder::Sw(sw) => {
                    match sw.next_frame_rgba() {
                        Ok(Some((rgba, fw, fh))) => {
                            write_rgba_to_bgra(pixels, canvas_w, canvas_h, rgba, fw, fh);
                            eof = false;
                        }
                        Ok(None) => { eof = true; }
                        Err(e) => {
                            tracing::warn!("SW decode error: {}", e);
                            decoder = None;
                            continue;
                        }
                    }
                }
            }

            if eof {
                if let Some(ref mut d) = decoder {
                    if let Err(e) = d.seek_to_start() {
                        tracing::warn!("seek_to_start: {} — clearing decoder", e);
                        decoder = None;
                    }
                }
                continue;
            }

            // Present: attach real buffer, mark damage, request frame callback, commit.
            // The frame callback fires when the compositor has composited this frame.
            // Only then do we decode and render the next one — this guarantees
            // get_current_texture()-equivalent blocking never occurs.
            ctx.wl_surface.attach(Some(&ctx.canvas.buffer), 0, 0);
            ctx.wl_surface.damage_buffer(0, 0, canvas_w as i32, canvas_h as i32);
            let cb = ctx.wl_surface.frame(&ctx.evq.handle(), ());
            ctx.wl_surface.commit();
            if let Err(e) = ctx.evq.flush() {
                tracing::warn!("flush after render commit: {}", e);
            }

            ctx.wls.buffer_released = false;
            pending_cb  = Some(cb);
            cb_deadline = None;
            next_frame += dur;

        } else {
            // No active wallpaper — poll slowly.
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    drop(ctx.layer_surface);
    drop(ctx.wl_surface);
    let _ = ctx.evq.flush();
    Ok(())
}

// ─── Decoder open helper ──────────────────────────────────────────────────────

fn open_decoder(info: &WallpaperInfo) -> Option<AnyDecoder> {
    if let Some(hw) = HwDecoder::try_open(&info.file_path) {
        tracing::info!("HW decode active (VA-API): {}x{} — {}",
            hw.dimensions().0, hw.dimensions().1, info.title);
        return Some(AnyDecoder::Hw(hw));
    }
    match VideoDecoder::open(&info.file_path) {
        Ok(sw) => {
            tracing::info!("SW decode active: {}x{} — {}",
                sw.dimensions().0, sw.dimensions().1, info.title);
            Some(AnyDecoder::Sw(sw))
        }
        Err(e) => {
            tracing::warn!("VideoDecoder::open failed: {}", e);
            None
        }
    }
}

// ─── Pixel conversion helpers ─────────────────────────────────────────────────

/// Convert a single NV12 sample (BT.709 limited range) to BGRA.
#[inline(always)]
fn nv12_to_bgra(y: u8, u: u8, v: u8) -> (u8, u8, u8) {
    let y = y as i32 - 16;
    let u = u as i32 - 128;
    let v = v as i32 - 128;
    let r = ((298 * y + 409 * v           + 128) >> 8).clamp(0, 255) as u8;
    let g = ((298 * y - 100 * u - 208 * v + 128) >> 8).clamp(0, 255) as u8;
    let b = ((298 * y + 516 * u           + 128) >> 8).clamp(0, 255) as u8;
    (b, g, r)
}

/// Write NV12 frame into the BGRA canvas, scaling to (canvas_w × canvas_h).
///
/// wl_shm ARGB8888 on little-endian: bytes in memory are [B, G, R, A].
fn write_nv12_to_bgra(
    pixels:   &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    frame:    &Nv12Frame,
) {
    let cw = canvas_w as usize;
    let ch = canvas_h as usize;
    let fw = frame.width  as usize;
    let fh = frame.height as usize;

    for cy in 0..ch {
        let fy = cy * fh / ch;
        for cx in 0..cw {
            let fx = cx * fw / cw;
            let y  = frame.y[fy * fw + fx];
            let uv_y = fy / 2;
            let uv_x = fx / 2;
            // UV plane: `fw` bytes per row (fw/2 pairs × 2 bytes = fw bytes)
            let u  = frame.uv[uv_y * fw + uv_x * 2];
            let v  = frame.uv[uv_y * fw + uv_x * 2 + 1];
            let (b, g, r) = nv12_to_bgra(y, u, v);
            let i = (cy * cw + cx) * 4;
            pixels[i]   = b;
            pixels[i+1] = g;
            pixels[i+2] = r;
            pixels[i+3] = 255;
        }
    }
}

/// Write an RGBA frame from VideoDecoder into the BGRA canvas, scaling to canvas size.
///
/// VideoDecoder outputs packed RGBA (R G B A per pixel).
/// wl_shm ARGB8888 wants [B, G, R, A] in memory on little-endian.
fn write_rgba_to_bgra(
    pixels:   &mut [u8],
    canvas_w: u32,
    canvas_h: u32,
    rgba:     &[u8],
    frame_w:  u32,
    frame_h:  u32,
) {
    let cw = canvas_w as usize;
    let ch = canvas_h as usize;
    let fw = frame_w  as usize;
    let fh = frame_h  as usize;

    for cy in 0..ch {
        let fy = cy * fh / ch;
        for cx in 0..cw {
            let fx = cx * fw / cw;
            let si = (fy * fw + fx) * 4;
            let di = (cy * cw + cx) * 4;
            pixels[di]   = rgba[si + 2]; // B ← R from RGBA
            pixels[di+1] = rgba[si + 1]; // G
            pixels[di+2] = rgba[si];     // R ← B from RGBA
            pixels[di+3] = rgba[si + 3]; // A
        }
    }
}
