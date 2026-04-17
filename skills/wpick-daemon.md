---
name: wpick-daemon
description: "Use this skill when implementing wpick-daemon: Wayland layer surface setup, wgpu render pipeline, video decoding with ffmpeg-next, audio playback with rodio, IPC server, shared state management, or daemon lifecycle. Triggers: 'implement daemon', 'wayland renderer', 'wgpu pipeline', 'layer shell', 'video decode', 'ffmpeg frames', 'audio sink', 'rodio source', 'ipc server', 'daemon state', 'DaemonState'. Read PROJECT.md, CORE.md, DAEMON.md, and ERRORS.md before using this skill."
---

# wpick-daemon Implementation Skill

## Reference Files
Always read before any daemon work:
- `DAEMON.md` — complete implementation with code for all modules
- `PROJECT.md` — shared state channels, startup flow, IPC protocol
- `ERRORS.md` — log levels per situation, anyhow context patterns

## Implementation Order

```
1. state.rs        → channels, DaemonState struct, helper methods
2. ipc_server.rs   → UnixListener, dispatch, scan_and_populate_cache
3. video.rs        → VideoDecoder (ffmpeg-next, RGBA output)
4. audio.rs        → AudioDecoder, AudioSamples rodio::Source
5. renderer.rs     → Wayland init, wgpu pipeline, render loop
6. main.rs         → wire all tasks together, signal handling
```

Start with state.rs and ipc_server.rs — you can test IPC without Wayland.

## state.rs — Rules

All inter-task communication uses `tokio::sync::watch` channels:
```
renderer_tx: watch::Sender<Option<WallpaperInfo>>   → renderer task
audio_tx:    watch::Sender<Option<WallpaperInfo>>   → audio task
volume_tx:   watch::Sender<(f32, bool)>             → audio task (level, muted)
pause_tx:    watch::Sender<bool>                    → both tasks
shutdown_tx: broadcast::Sender<()>                  → all tasks
```

**Never hold `Mutex` across `.await`:**
```rust
// WRONG:
let guard = state.lock().await;
some_async_fn().await;  // lock held across await — potential deadlock
drop(guard);

// CORRECT:
let value = {
    let guard = state.lock().await;
    guard.some_field.clone()
};  // lock released before await
some_async_fn(value).await;
```

Use `tokio::sync::Mutex` (NOT `std::sync::Mutex` or `parking_lot::Mutex`) for `DaemonState`
because it's shared across async tasks.

## ipc_server.rs — Rules

- Each accepted connection gets its own `tokio::spawn` task
- Connection errors are `tracing::warn!` — never crash the server
- `ClientCommand::Kill` calls `std::process::exit(0)` after sending `Ok`
- `ClientCommand::List` with empty cache calls `scan_and_populate_cache()` with `spawn_blocking`
- All heavy I/O in `scan_and_populate_cache` runs in `spawn_blocking` — never block tokio workers

Stale socket detection in main.rs:
```rust
if socket_path.exists() {
    if tokio::net::UnixStream::connect(&socket_path).await.is_ok() {
        anyhow::bail!("Another wpick-daemon is already running at {:?}", socket_path);
    }
    // Stale socket from crash — remove it
    std::fs::remove_file(&socket_path)
        .context("Failed to remove stale socket")?;
}
```

## video.rs — Implementation Checklist

- [ ] `ffmpeg::init()` called once at start of `open()`
- [ ] Use `streams().best(Type::Video)` — handles files with multiple video streams
- [ ] Scaler converts source pixel format → `Pixel::RGBA` (NOT `RGB24` — wgpu needs 4 bytes/pixel)
- [ ] `frame_duration()` guards against 0/NaN fps: `.max(1.0)`
- [ ] `next_frame_rgba()` skips packets from non-video streams (audio, subtitle)
- [ ] `seek_to_start()` calls `decoder.flush()` after seek to clear codec buffer
- [ ] Returns `Ok(None)` on EOF — caller handles looping

ffmpeg pixel format to RGBA — scaler setup:
```rust
let scaler = ffmpeg::software::scaling::context::Context::get(
    decoder.format(),
    decoder.width(),
    decoder.height(),
    ffmpeg::format::Pixel::RGBA,   // output format
    decoder.width(),
    decoder.height(),
    ffmpeg::software::scaling::flag::Flags::BILINEAR,
)?;
```

Data extraction from RGBA frame:
```rust
// plane 0 = RGBA packed data
let data = rgba_frame.data(0).to_vec();
// stride may be larger than width*4 — use linesize
let linesize = rgba_frame.stride(0) as u32;
// If linesize == width * 4, data is contiguous and can be uploaded directly.
// If linesize > width * 4, you must strip padding per row before upload.
```

Handle non-contiguous frames:
```rust
let width  = rgba_frame.width() as usize;
let height = rgba_frame.height() as usize;
let stride = rgba_frame.stride(0);
let src    = rgba_frame.data(0);

let mut packed = Vec::with_capacity(width * height * 4);
for row in 0..height {
    let start = row * stride;
    packed.extend_from_slice(&src[start..start + width * 4]);
}
```

## audio.rs — Implementation Checklist

- [ ] `OutputStream::try_default()` must stay on same thread as `Sink` — use `spawn_blocking` or ensure the audio task doesn't migrate
- [ ] `AudioSamples` iterator loops: `self.pos = (self.pos + 1) % self.samples.len()`
- [ ] `rodio::Source::total_duration()` returns `None` — infinite loop
- [ ] Audio pre-load limit: warn if file > 500MB, load anyway for MVP
- [ ] Apply volume immediately when `Sink` is created: `sink.set_volume(if muted { 0.0 } else { vol })`
- [ ] `has_audio` may be wrong from cache — catch decode errors and log warn, don't crash

Resampler to stereo f32 at 48000Hz (rodio's preferred format):
```rust
let mut resampler = ffmpeg::software::resampling::context::Context::get(
    decoder.format(),
    decoder.channel_layout(),
    decoder.rate(),
    ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
    ffmpeg::ChannelLayout::STEREO,
    48000,
)?;
```

Watch channel polling loop (50ms sleep — acceptable latency for volume changes):
```rust
loop {
    if wallpaper_rx.has_changed() { /* reload audio */ }
    if volume_rx.has_changed()    { /* update sink volume */ }
    if pause_rx.has_changed()     { /* pause/resume sink */ }

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
}
```

## renderer.rs — Wayland + wgpu

### Wayland Initialization (Critical Sequence)

```
wl_display::connect(None)          → connect to $WAYLAND_DISPLAY
wl_display::get_registry()         → get global registry
EventQueue::roundtrip()            → receive all current globals
  ↓ globals received:
  bind wl_compositor  → create surfaces
  bind zwlr_layer_shell_v1 → create layer surfaces
  bind wl_output      → get output dimensions

wl_compositor::create_surface()    → wl_surface
zwlr_layer_shell_v1::get_layer_surface(
    surface,
    output: None,          → None = all outputs
    layer: Background,
    namespace: "wpick",
)
layer_surface::set_anchor(top | right | bottom | left)
layer_surface::set_size(0, 0)      → fullscreen
layer_surface::set_exclusive_zone(-1)   → don't affect other surfaces
wl_surface::commit()               → trigger configure event

EventQueue::roundtrip()            → receive configure event
  ↓ zwlr_layer_surface_v1.configure(serial, width, height) received
  layer_surface::ack_configure(serial)
  wl_surface::commit()             → NOW safe to render
```

### wgpu Surface from Wayland

```rust
// Get raw pointers BEFORE creating wgpu instance
// (wayland-client objects provide .id().as_ptr())
let display_ptr = display.backend().upgrade().unwrap().display_ptr() as *mut std::ffi::c_void;
let surface_ptr = wl_surface.id().as_ptr() as *mut std::ffi::c_void;

// Construct raw handles
let mut wayland_display_handle = WaylandDisplayHandle::empty();
wayland_display_handle.display = std::ptr::NonNull::new(display_ptr).unwrap();

let mut wayland_window_handle = WaylandWindowHandle::empty();
wayland_window_handle.surface = std::ptr::NonNull::new(surface_ptr).unwrap();
```

### wgpu Pipeline Structure

```
Device + Queue
  └── RenderPipeline
        ├── Vertex shader: fullscreen quad (hardcoded 6 vertices, no vertex buffer)
        ├── Fragment shader: samples video_texture via sampler
        ├── BindGroupLayout: { texture_2d<f32>, sampler }
        └── PipelineLayout

video_texture: Texture { format: Rgba8UnormSrgb, usage: TEXTURE_BINDING | COPY_DST }
sampler: linear filter, address_mode: ClampToEdge

Surface config:
  format: surface.get_capabilities(&adapter).formats[0]
  present_mode: Fifo (vsync) for MVP
  alpha_mode: Opaque
```

Vertex shader (fullscreen triangle pair, no buffer needed):
```wgsl
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    // Two triangles covering clip space
    var positions = array<vec2<f32>, 6>(
        vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 1.0),
        vec2(1.0, -1.0),  vec2(1.0,  1.0), vec2(-1.0, 1.0),
    );
    let p = positions[vi];
    return vec4<f32>(p, 0.0, 1.0);
}
```

Fragment shader:
```wgsl
@group(0) @binding(0) var t_video:  texture_2d<f32>;
@group(0) @binding(1) var s_linear: sampler;

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    // Convert from pixel position to UV (0.0 → 1.0)
    let dims = vec2<f32>(textureDimensions(t_video));
    let uv   = vec2<f32>(pos.x / dims.x, pos.y / dims.y);
    return textureSample(t_video, s_linear, uv);
}
```

### Render Loop Timing

```rust
let frame_duration = decoder.frame_duration();
let mut next_frame = std::time::Instant::now();

loop {
    let now = std::time::Instant::now();
    if now >= next_frame {
        // decode + upload + render
        next_frame = now + frame_duration;
    } else {
        // sleep until next frame is due
        let wait = next_frame - now;
        tokio::time::sleep(wait.min(std::time::Duration::from_millis(16))).await;
    }

    // Wayland non-blocking dispatch (must happen on same thread as surface creation)
    event_queue.dispatch_pending(&mut state)?;

    // Check for shutdown/wallpaper change
    if shutdown_rx.try_recv().is_ok() { break; }
    if wallpaper_rx.has_changed() { /* reload decoder */ }
}
```

### Wayland Threading Rule

`wl_surface` and `EventQueue` are NOT `Send`.  
The renderer cannot be a regular `tokio::spawn` task.

**Solution**: Run renderer on the main thread, spawn everything else on tokio:
```rust
// In main.rs:
let rt = tokio::runtime::Runtime::new()?;
rt.spawn(ipc_server::run(listener, state.clone(), cache.clone(), dirs.clone()));
rt.spawn(audio::run(audio_rx, volume_rx, pause_rx));

// Block main thread on renderer (Wayland runs here)
rt.block_on(renderer::run(renderer_rx, &config, shutdown_rx))?;
```

Or use `tokio::task::LocalSet` if you prefer keeping everything in tokio context:
```rust
let local = tokio::task::LocalSet::new();
local.spawn_local(renderer::run(...));  // renderer on local set
tokio::spawn(ipc_server::run(...));     // IPC on regular workers
local.await;
```

## main.rs — Signal Handling

Register cleanup before binding socket:
```rust
// Cleanup function — called from signal handlers
fn cleanup(socket_path: &std::path::Path) {
    let _ = std::fs::remove_file(socket_path);
}

// SIGINT (Ctrl+C)
let sp = socket_path.clone();
tokio::spawn(async move {
    tokio::signal::ctrl_c().await.ok();
    cleanup(&sp);
    std::process::exit(0);
});

// SIGTERM
#[cfg(unix)]
{
    let sp = socket_path.clone();
    tokio::spawn(async move {
        let mut sig = tokio::signal::unix::signal(
            tokio::signal::unix::SignalKind::terminate()
        ).expect("signal handler");
        sig.recv().await;
        cleanup(&sp);
        std::process::exit(0);
    });
}
```

## Common Failure Modes

| Symptom | Cause | Fix |
|---------|-------|-----|
| `wgpu::RequestDeviceError` | GPU adapter not Vulkan capable | Add `Backends::GL` fallback |
| Black screen, no error | Wayland configure not ack'd | Check ack_configure + commit sequence |
| Audio starts then stops | `AudioSamples` iterator returns `None` | Check loop: `% self.samples.len()` |
| `SIGPIPE` on IPC | TUI disconnected while write in progress | Ignore SIGPIPE: `unsafe { libc::signal(libc::SIGPIPE, libc::SIG_IGN) }` |
| `configure: size 0x0` | Output not bound in registry | Bind `wl_output` and wait for configure |
| ffmpeg link error | System ffmpeg missing or wrong version | `sudo pacman -S ffmpeg` or check `PKG_CONFIG_PATH` |

## Verification Steps

```bash
# 1. Check daemon compiles
cargo check -p wpick-daemon

# 2. Start daemon (logs to file, check for errors)
RUST_LOG=wpick_daemon=debug cargo run -p wpick-daemon
# In another terminal:
tail -f ~/.local/share/wpick/wpick.log

# 3. Test IPC manually
echo '{"type":"List"}' | nc -U ~/.wpick.sock

# 4. Full integration test with TUI
cargo run -p wpick-tui
```
