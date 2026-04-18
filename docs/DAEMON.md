# wpick-daemon — Daemon Implementation Specification

> **Context**: Read PROJECT.md and CORE.md first.  
> This file defines the background daemon: Wayland surface setup, wgpu rendering pipeline,  
> video decode loop, audio playback, IPC server, and state management.  
> This is the most complex crate. Read every section before writing any code.

---

## File Structure

```
wpick-daemon/src/
├── main.rs         Entry point: socket creation, task spawning, signal handling.
├── state.rs        DaemonState — shared mutable state across all async tasks.
├── ipc_server.rs   UnixListener — accepts connections, dispatches ClientCommands.
├── renderer.rs     Wayland surface + wgpu render loop.
├── video.rs        ffmpeg-next video decode — produces frames as raw RGBA bytes.
└── audio.rs        ffmpeg-next audio decode → custom rodio Source → Sink playback.
```

---

## `state.rs` — Shared Daemon State

All tasks share a single `Arc<tokio::sync::Mutex<DaemonState>>`.  
Never hold the lock across an `.await` point.

```rust
use parking_lot::Mutex;
use std::sync::Arc;

/// Wraps the watch channel senders for renderer and audio tasks.
/// The renderer and audio tasks each hold the Receiver end.
pub struct DaemonState {
    /// Currently active wallpaper. None = nothing playing.
    pub current: Option<WallpaperInfo>,

    /// Playback volume, 0.0–1.0.
    pub volume: f32,

    /// Whether audio is muted (volume sent to sink = 0.0).
    pub muted: bool,

    /// Whether playback is paused.
    pub paused: bool,

    /// Channel to notify renderer of wallpaper change.
    /// Renderer receives the new WallpaperInfo (or None to stop).
    pub renderer_tx: tokio::sync::watch::Sender<Option<WallpaperInfo>>,

    /// Channel to notify audio task of wallpaper change.
    pub audio_tx: tokio::sync::watch::Sender<Option<WallpaperInfo>>,

    /// Channel to set volume on audio task.
    pub volume_tx: tokio::sync::watch::Sender<(f32, bool)>,  // (level, muted)

    /// Channel to pause/resume renderer and audio.
    pub pause_tx: tokio::sync::watch::Sender<bool>,

    /// Signal to shut down all tasks.
    pub shutdown_tx: tokio::sync::broadcast::Sender<()>,
}

impl DaemonState {
    pub fn new(/* senders */) -> Self { ... }

    /// Set a new wallpaper. Notifies renderer and audio tasks.
    pub fn set_wallpaper(&mut self, info: WallpaperInfo) {
        self.current = Some(info.clone());
        let _ = self.renderer_tx.send(Some(info.clone()));
        let _ = self.audio_tx.send(Some(info));
    }

    /// Stop current wallpaper without setting a new one.
    pub fn stop(&mut self) {
        self.current = None;
        let _ = self.renderer_tx.send(None);
        let _ = self.audio_tx.send(None);
    }

    pub fn set_volume(&mut self, level: f32) {
        self.volume = level.clamp(0.0, 1.0);
        let _ = self.volume_tx.send((self.volume, self.muted));
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        let _ = self.volume_tx.send((self.volume, self.muted));
    }

    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
        let _ = self.pause_tx.send(paused);
    }
}
```

---

## `main.rs` — Entry Point

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load config and resolve paths
    let config = WpickConfig::load()?;
    let dirs = config.app_dirs()?;

    // 2. Setup file logging (daemon has no terminal)
    let file_appender = tracing_appender::rolling::daily(&dirs.log_dir, "wpick.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);
    tracing_subscriber::fmt()
        .with_writer(non_blocking)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env()
            .add_directive("wpick_daemon=info".parse()?)
            .add_directive("wpick_core=info".parse()?))
        .init();

    tracing::info!("wpick-daemon starting");

    // 3. Open database
    let cache = Arc::new(tokio::sync::Mutex::new(Cache::open(&dirs.db_path)?));

    // 4. Create watch channels
    let (renderer_tx, renderer_rx) = tokio::sync::watch::channel(None::<WallpaperInfo>);
    let (audio_tx,    audio_rx)    = tokio::sync::watch::channel(None::<WallpaperInfo>);
    let (volume_tx,   volume_rx)   = tokio::sync::watch::channel((config.general.volume, config.general.muted));
    let (pause_tx,    pause_rx)    = tokio::sync::watch::channel(false);
    let (shutdown_tx, _)           = tokio::sync::broadcast::channel(1);

    // 5. Build shared state
    let state = Arc::new(tokio::sync::Mutex::new(DaemonState::new(
        renderer_tx, audio_tx, volume_tx, pause_tx, shutdown_tx.clone(),
        config.general.volume, config.general.muted,
    )));

    // 6. Bind Unix socket — fail early if already running
    let socket_path = dirs.socket_path.clone();
    if socket_path.exists() {
        // Check if it's a stale socket (previous crash)
        if tokio::net::UnixStream::connect(&socket_path).await.is_ok() {
            anyhow::bail!("Another wpick-daemon is already running");
        }
        std::fs::remove_file(&socket_path)?;
    }
    let listener = tokio::net::UnixListener::bind(&socket_path)?;
    tracing::info!("Listening on {:?}", socket_path);

    // 7. Register cleanup (remove socket on exit)
    let socket_cleanup = socket_path.clone();
    let shutdown_for_ctrlc = shutdown_tx.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("SIGINT received — shutting down");
        let _ = shutdown_for_ctrlc.send(());
        let _ = std::fs::remove_file(&socket_cleanup);
        std::process::exit(0);
    });

    // Also handle SIGTERM
    {
        let socket_cleanup2 = socket_path.clone();
        let shutdown2 = shutdown_tx.clone();
        tokio::spawn(async move {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{signal, SignalKind};
                let mut sigterm = signal(SignalKind::terminate()).unwrap();
                sigterm.recv().await;
                tracing::info!("SIGTERM received — shutting down");
                let _ = shutdown2.send(());
                let _ = std::fs::remove_file(&socket_cleanup2);
                std::process::exit(0);
            }
        });
    }

    // 8. Spawn tasks
    let state_for_ipc = Arc::clone(&state);
    let cache_for_ipc = Arc::clone(&cache);
    let dirs_for_ipc  = dirs.clone();
    tokio::spawn(async move {
        if let Err(e) = ipc_server::run(listener, state_for_ipc, cache_for_ipc, dirs_for_ipc).await {
            tracing::error!("IPC server error: {}", e);
        }
    });

    tokio::spawn(async move {
        if let Err(e) = audio::run(audio_rx, volume_rx, pause_rx).await {
            tracing::error!("Audio task error: {}", e);
        }
    });

    // Renderer runs on current thread (Wayland requires single-threaded dispatch)
    // It blocks until shutdown.
    renderer::run(renderer_rx, &config, shutdown_tx.subscribe()).await?;

    Ok(())
}
```

---

## `ipc_server.rs` — Command Dispatcher

```rust
pub async fn run(
    listener: tokio::net::UnixListener,
    state: Arc<tokio::sync::Mutex<DaemonState>>,
    cache: Arc<tokio::sync::Mutex<Cache>>,
    dirs: AppDirs,
) -> anyhow::Result<()> {
    loop {
        let (stream, _addr) = listener.accept().await?;
        let state = Arc::clone(&state);
        let cache = Arc::clone(&cache);
        let dirs = dirs.clone();

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, state, cache, dirs).await {
                tracing::warn!("IPC connection error: {}", e);
            }
        });
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    state: Arc<tokio::sync::Mutex<DaemonState>>,
    cache: Arc<tokio::sync::Mutex<Cache>>,
    dirs: AppDirs,
) -> anyhow::Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);
    let mut writer = tokio::io::BufWriter::new(write_half);

    loop {
        let cmd = match wpick_core::ipc::recv_command(&mut reader).await {
            Ok(cmd) => cmd,
            Err(_)  => break,  // Connection closed or parse error — end session
        };

        tracing::debug!("Received command: {:?}", cmd);

        let response = dispatch(cmd, &state, &cache, &dirs).await;

        if let Err(e) = wpick_core::ipc::send_response(&mut writer, &response).await {
            tracing::warn!("Failed to send response: {}", e);
            break;
        }

        // Kill command terminates the daemon after sending Ok
        if matches!(response, DaemonResponse::Ok) {
            // Check if last command was Kill — done in dispatch via a side channel
        }
    }

    Ok(())
}

async fn dispatch(
    cmd: ClientCommand,
    state: &Arc<tokio::sync::Mutex<DaemonState>>,
    cache: &Arc<tokio::sync::Mutex<Cache>>,
    dirs: &AppDirs,
) -> DaemonResponse {
    match cmd {
        ClientCommand::List => {
            // Load from cache, trigger scan if empty
            let items = {
                let cache_guard = cache.lock().await;
                match cache_guard.get_all() {
                    Ok(items) if !items.is_empty() => items,
                    _ => {
                        // Trigger background scan — this may take seconds
                        drop(cache_guard);
                        match scan_and_populate_cache(cache, dirs).await {
                            Ok(items) => items,
                            Err(e) => return DaemonResponse::Error { message: e.to_string() },
                        }
                    }
                }
            };
            DaemonResponse::WallpaperList { items }
        }

        ClientCommand::Set { id } => {
            let info = {
                let cache_guard = cache.lock().await;
                match cache_guard.get_by_id(id) {
                    Ok(Some(info)) => info,
                    Ok(None) => return DaemonResponse::Error {
                        message: format!("Wallpaper {} not found in cache", id)
                    },
                    Err(e) => return DaemonResponse::Error { message: e.to_string() },
                }
            };

            if !info.is_supported() {
                return DaemonResponse::Error {
                    message: format!("Wallpaper type '{}' is not supported in MVP", info.wallpaper_type)
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
            match cache.lock().await.get_by_id(id) {
                Ok(Some(item)) => DaemonResponse::WallpaperInfo { item },
                Ok(None) => DaemonResponse::Error { message: format!("ID {} not found", id) },
                Err(e) => DaemonResponse::Error { message: e.to_string() },
            }
        }

        ClientCommand::Kill => {
            tracing::info!("Kill command received — shutting down");
            // Signal all tasks to stop
            let _ = state.lock().await.shutdown_tx.send(());
            // Socket cleanup happens in main via the shutdown receiver
            std::process::exit(0);  // Direct exit — cleanup registered in main()
        }
    }
}

/// Scan Steam libraries, extract PKG files, populate cache. Returns all found wallpapers.
async fn scan_and_populate_cache(
    cache: &Arc<tokio::sync::Mutex<Cache>>,
    dirs: &AppDirs,
) -> anyhow::Result<Vec<WallpaperInfo>> {
    tracing::info!("Starting full wallpaper scan");

    // Run on blocking thread pool — PKG extraction is CPU+IO intensive
    let cache = Arc::clone(cache);
    let dirs = dirs.clone();

    tokio::task::spawn_blocking(move || {
        let config = WpickConfig::load()?;
        let wallpaper_dirs = wpick_core::discovery::find_wallpaper_dirs(&config)?;
        tracing::info!("Found {} wallpaper directories", wallpaper_dirs.len());

        let mut results = Vec::new();
        let cache_guard = cache.blocking_lock();

        for wd in wallpaper_dirs {
            match wpick_core::pkg::extract_and_parse(&wd, &dirs.wallpapers_dir) {
                Ok(Some(info)) => {
                    // Get mtime for cache invalidation
                    let mtime = std::fs::metadata(&wd.path.join("scene.pkg"))
                        .and_then(|m| m.modified())
                        .map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs())
                        .unwrap_or(0);

                    if let Err(e) = cache_guard.upsert(&info, mtime) {
                        tracing::warn!("Cache upsert failed for {}: {}", wd.id, e);
                    }
                    results.push(info);
                }
                Ok(None) => {
                    tracing::debug!("Skipping unsupported wallpaper {}", wd.id);
                }
                Err(e) => {
                    tracing::warn!("Failed to process wallpaper {}: {}", wd.id, e);
                }
            }
        }

        Ok::<Vec<WallpaperInfo>, anyhow::Error>(results)
    }).await?
}
```

---

## `renderer.rs` — Wayland + wgpu

This is the most complex module. Read completely before implementing.

### Initialization Sequence

```
1. Connect to Wayland display
2. Get global registry → bind needed globals:
   - wl_compositor (for wl_surface)
   - zwlr_layer_shell_v1 (for background surface)
   - wl_output (to get screen dimensions)
3. Create wl_surface via compositor
4. Create zwlr_layer_surface_v1 via layer_shell:
   - layer: zwlr_layer_shell_v1::Layer::Background
   - anchor: all sides (top | right | bottom | left)
   - size: (0, 0) — fullscreen
   - exclusive_zone: -1 (don't push other surfaces)
5. Commit the surface → receive configure event with actual dimensions
6. Create wgpu::Surface from the Wayland raw handles
7. Build wgpu render pipeline (fullscreen quad + texture sampler)
8. Enter render loop
```

### wgpu Setup

```rust
// Raw handle construction for wgpu
// Requires: raw-window-handle = "0.6"

use raw_window_handle::{
    HasDisplayHandle, HasWindowHandle,
    RawDisplayHandle, RawWindowHandle,
    WaylandDisplayHandle, WaylandWindowHandle,
};

struct WaylandHandles {
    display: *mut std::ffi::c_void,  // wl_display pointer
    surface: *mut std::ffi::c_void,  // wl_surface pointer
}

// SAFETY: These pointers are valid as long as the Wayland connection is alive.
// They outlive the wgpu surface because Wayland cleanup happens after wgpu is dropped.
unsafe impl HasDisplayHandle for WaylandHandles {
    fn display_handle(&self) -> Result<raw_window_handle::DisplayHandle<'_>, raw_window_handle::HandleError> {
        let mut handle = WaylandDisplayHandle::empty();
        handle.display = std::ptr::NonNull::new(self.display).unwrap();
        Ok(unsafe { raw_window_handle::DisplayHandle::borrow_raw(RawDisplayHandle::Wayland(handle)) })
    }
}

unsafe impl HasWindowHandle for WaylandHandles {
    fn window_handle(&self) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let mut handle = WaylandWindowHandle::empty();
        handle.surface = std::ptr::NonNull::new(self.surface).unwrap();
        Ok(unsafe { raw_window_handle::WindowHandle::borrow_raw(RawWindowHandle::Wayland(handle)) })
    }
}

// Create wgpu surface:
let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
    backends: wgpu::Backends::VULKAN,  // Force Vulkan on Linux
    ..Default::default()
});

// SAFETY: handles must outlive the surface
let surface = unsafe {
    instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::from_window(&wayland_handles)?)?
};
```

### Render Pipeline

The renderer displays a single fullscreen textured quad. No 3D, no transforms.

```
Vertex shader:
  - 4 vertices (2 triangles = quad)
  - UV coordinates: (0,0) top-left to (1,1) bottom-right
  - Output: NDC position [-1,1] x [-1,1]

Fragment shader:
  - Sample from video_texture at UV coordinate
  - Output: RGBA color

Texture format: wgpu::TextureFormat::Rgba8UnormSrgb
  - ffmpeg output must be converted to RGBA8 before upload
  - Use ffmpeg's swscale to convert from source pixel format to AV_PIX_FMT_RGBA
```

### Frame Upload

```rust
fn upload_frame(queue: &wgpu::Queue, texture: &wgpu::Texture, rgba_data: &[u8], width: u32, height: u32) {
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        rgba_data,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(4 * width),  // 4 bytes per pixel (RGBA)
            rows_per_image: Some(height),
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
}
```

### Render Loop

```rust
pub async fn run(
    mut wallpaper_rx: tokio::sync::watch::Receiver<Option<WallpaperInfo>>,
    config: &WpickConfig,
    mut shutdown_rx: tokio::sync::broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    // Initialize Wayland and wgpu (blocking setup)
    let mut ctx = RendererContext::init(config).await?;

    let mut current_decoder: Option<VideoDecoder> = None;
    let mut next_frame_time = std::time::Instant::now();

    loop {
        // Check for shutdown
        if shutdown_rx.try_recv().is_ok() {
            tracing::info!("Renderer shutting down");
            break;
        }

        // Check for wallpaper change
        if wallpaper_rx.has_changed() {
            let new_wallpaper = wallpaper_rx.borrow_and_update().clone();
            current_decoder = match new_wallpaper {
                Some(info) => {
                    tracing::info!("Loading wallpaper: {}", info.title);
                    match VideoDecoder::open(&info.file_path) {
                        Ok(dec) => Some(dec),
                        Err(e)  => {
                            tracing::error!("Failed to open video: {}", e);
                            None
                        }
                    }
                }
                None => {
                    // Clear surface (render black)
                    ctx.render_black();
                    None
                }
            };
            next_frame_time = std::time::Instant::now();
        }

        // Decode and render next frame
        if let Some(ref mut decoder) = current_decoder {
            let now = std::time::Instant::now();
            if now >= next_frame_time {
                match decoder.next_frame_rgba() {
                    Ok(Some((rgba, width, height))) => {
                        upload_frame(&ctx.queue, &ctx.video_texture, &rgba, width, height);
                        ctx.render_frame();
                        next_frame_time = now + decoder.frame_duration();
                    }
                    Ok(None) => {
                        // EOF — seek to start and loop
                        if let Err(e) = decoder.seek_to_start() {
                            tracing::warn!("Seek failed: {} — reloading file", e);
                            // Recreate decoder as fallback
                        }
                    }
                    Err(e) => {
                        tracing::error!("Frame decode error: {}", e);
                        current_decoder = None;
                    }
                }
            }
        }

        // Wayland event dispatch (non-blocking)
        ctx.dispatch_wayland_events()?;

        // Yield to tokio runtime briefly
        tokio::task::yield_now().await;
    }

    Ok(())
}
```

---

## `video.rs` — Video Decoder

```rust
use ffmpeg_next as ffmpeg;

pub struct VideoDecoder {
    input_ctx:   ffmpeg::format::context::Input,
    video_stream_idx: usize,
    decoder:     ffmpeg::codec::decoder::Video,
    scaler:      ffmpeg::software::scaling::context::Context,  // for RGBA conversion
    fps:         f64,  // frames per second (from stream metadata)
}

impl VideoDecoder {
    /// Open a video file. Finds the best video stream and sets up an RGBA scaler.
    pub fn open(path: &str) -> anyhow::Result<Self> {
        ffmpeg::init()?;

        let input_ctx = ffmpeg::format::input(&path)?;

        let stream = input_ctx
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| anyhow::anyhow!("No video stream in {}", path))?;

        let stream_idx = stream.index();

        let context_decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = context_decoder.decoder().video()?;

        let fps = stream.avg_frame_rate();
        let fps_f64 = fps.numerator() as f64 / fps.denominator() as f64;

        // Scaler: source format → RGBA
        let scaler = ffmpeg::software::scaling::context::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::format::Pixel::RGBA,
            decoder.width(),
            decoder.height(),
            ffmpeg::software::scaling::flag::Flags::BILINEAR,
        )?;

        Ok(Self {
            input_ctx,
            video_stream_idx: stream_idx,
            decoder,
            scaler,
            fps: fps_f64.max(1.0),  // guard against 0 fps
        })
    }

    /// Decode the next video frame and return it as raw RGBA bytes.
    /// Returns Ok(None) on EOF.
    pub fn next_frame_rgba(&mut self) -> anyhow::Result<Option<(Vec<u8>, u32, u32)>> {
        loop {
            // Read next packet
            match self.input_ctx.packets().next() {
                None => return Ok(None),  // EOF
                Some((stream, packet)) => {
                    if stream.index() != self.video_stream_idx {
                        continue;  // Skip non-video packets (audio etc.)
                    }

                    self.decoder.send_packet(&packet)?;

                    let mut decoded = ffmpeg::frame::Video::empty();
                    if self.decoder.receive_frame(&mut decoded).is_ok() {
                        let mut rgba_frame = ffmpeg::frame::Video::empty();
                        self.scaler.run(&decoded, &mut rgba_frame)?;

                        let width  = rgba_frame.width();
                        let height = rgba_frame.height();
                        let data   = rgba_frame.data(0).to_vec();

                        return Ok(Some((data, width, height)));
                    }
                }
            }
        }
    }

    /// Seek to the beginning of the stream for looping.
    pub fn seek_to_start(&mut self) -> anyhow::Result<()> {
        self.input_ctx.seek(0, ..)?;
        self.decoder.flush();
        Ok(())
    }

    /// Duration between frames at the video's native FPS.
    pub fn frame_duration(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(1.0 / self.fps)
    }

    /// Actual video dimensions.
    pub fn dimensions(&self) -> (u32, u32) {
        (self.decoder.width(), self.decoder.height())
    }
}
```

---

## `audio.rs` — Audio Playback

The audio task runs independently. It receives wallpaper changes via watch channel,
decodes audio from the video file using ffmpeg, and feeds samples to a rodio Sink.

```rust
use rodio::{OutputStream, Sink, Source};
use ffmpeg_next as ffmpeg;

pub async fn run(
    mut wallpaper_rx: tokio::sync::watch::Receiver<Option<WallpaperInfo>>,
    mut volume_rx: tokio::sync::watch::Receiver<(f32, bool)>,
    mut pause_rx: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // rodio requires a non-send handle — must stay on one thread
    let (_output_stream, stream_handle) = OutputStream::try_default()?;

    let mut current_sink: Option<Sink> = None;

    loop {
        // Handle wallpaper change
        if wallpaper_rx.has_changed() {
            let new_wallpaper = wallpaper_rx.borrow_and_update().clone();

            // Stop current audio
            if let Some(sink) = current_sink.take() {
                sink.stop();
            }

            if let Some(info) = new_wallpaper {
                if info.has_audio {
                    match build_audio_source(&info.file_path) {
                        Ok(source) => {
                            let sink = Sink::try_new(&stream_handle)?;
                            let (vol, muted) = *volume_rx.borrow();
                            sink.set_volume(if muted { 0.0 } else { vol });
                            sink.append(source);
                            // Note: sink.play() is not needed — appended sources play automatically
                            current_sink = Some(sink);
                        }
                        Err(e) => {
                            tracing::warn!("No audio in {}: {}", info.file_path, e);
                        }
                    }
                }
            }
        }

        // Handle volume change
        if volume_rx.has_changed() {
            let (vol, muted) = *volume_rx.borrow_and_update();
            if let Some(ref sink) = current_sink {
                sink.set_volume(if muted { 0.0 } else { vol });
            }
        }

        // Handle pause/resume
        if pause_rx.has_changed() {
            let paused = *pause_rx.borrow_and_update();
            if let Some(ref sink) = current_sink {
                if paused { sink.pause(); } else { sink.play(); }
            }
        }

        // Yield to tokio
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Decode audio from a video file and wrap it as a rodio Source.
fn build_audio_source(path: &str) -> anyhow::Result<impl rodio::Source<Item = f32>> {
    // Find audio stream in the file via ffmpeg
    let input_ctx = ffmpeg::format::input(&path)?;

    let audio_stream = input_ctx
        .streams()
        .best(ffmpeg::media::Type::Audio)
        .ok_or_else(|| anyhow::anyhow!("No audio stream"))?;

    let sample_rate  = audio_stream.parameters().sample_rate() as u32;
    let channel_count = audio_stream.parameters().channels() as u16;

    // Decode all audio frames upfront and collect into Vec<f32>
    // (For MVP simplicity — production version would use a streaming iterator)
    let samples = decode_audio_to_f32(input_ctx, audio_stream.index())?;

    Ok(AudioSamples {
        samples,
        pos: 0,
        sample_rate,
        channels: channel_count,
    })
}

fn decode_audio_to_f32(
    mut ctx: ffmpeg::format::context::Input,
    stream_idx: usize,
) -> anyhow::Result<Vec<f32>> {
    let mut decoder = ctx.stream(stream_idx)
        .map(|s| ffmpeg::codec::context::Context::from_parameters(s.parameters()))
        .ok_or_else(|| anyhow::anyhow!("Stream not found"))??
        .decoder()
        .audio()?;

    // Use a resampler to convert to f32 planar, 48000Hz, mono/stereo
    let mut resampler = ffmpeg::software::resampling::context::Context::get(
        decoder.format(),
        decoder.channel_layout(),
        decoder.rate(),
        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
        ffmpeg::ChannelLayout::STEREO,
        48000,
    )?;

    let mut samples = Vec::new();

    for (stream, packet) in ctx.packets() {
        if stream.index() != stream_idx { continue; }
        decoder.send_packet(&packet)?;

        let mut frame = ffmpeg::frame::Audio::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            let mut resampled = ffmpeg::frame::Audio::empty();
            resampler.run(&frame, &mut resampled)?;
            // Extract f32 samples from resampled frame
            let data = resampled.data(0);
            // Each f32 is 4 bytes
            for chunk in data.chunks_exact(4) {
                samples.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
    }

    Ok(samples)
}

/// rodio Source implementation backed by a Vec<f32> (loops infinitely).
struct AudioSamples {
    samples: Vec<f32>,
    pos:     usize,
    sample_rate: u32,
    channels: u16,
}

impl Iterator for AudioSamples {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.samples.is_empty() { return None; }
        let val = self.samples[self.pos];
        self.pos = (self.pos + 1) % self.samples.len();  // loop
        Some(val)
    }
}

impl rodio::Source for AudioSamples {
    fn current_frame_len(&self) -> Option<usize> { None }
    fn channels(&self)         -> u16 { self.channels }
    fn sample_rate(&self)      -> u32 { self.sample_rate }
    fn total_duration(&self)   -> Option<std::time::Duration> { None }  // infinite loop
}
```

**Note**: Pre-loading all audio into memory is acceptable for MVP (wallpapers are typically a few minutes, ~20–100MB RAM).  
Future improvement: streaming rodio Source that decodes on the fly.

---

## Cargo.toml

```toml
[package]
name = "wpick-daemon"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "wpick-daemon"
path = "src/main.rs"

[dependencies]
wpick-core           = { path = "../wpick-core" }
serde                = { workspace = true }
serde_json           = { workspace = true }
anyhow               = { workspace = true }
tokio                = { workspace = true }
tracing              = { workspace = true }
tracing-subscriber   = { version = "0.3", features = ["env-filter"] }
tracing-appender     = "0.2"
wayland-client       = "0.31"
wayland-protocols-wlr = { version = "0.3", features = ["client"] }
wgpu                 = "22"
bytemuck             = { version = "1", features = ["derive"] }
ffmpeg-next          = "7"
rodio                = "0.19"
parking_lot          = "0.12"
raw-window-handle    = "0.6"
```

---

## Known Difficult Points

### Wayland Threading
Wayland dispatch (`event_queue.dispatch()`) must happen on the **same thread** as surface creation.  
The renderer therefore cannot be a normal tokio task. Use `tokio::task::spawn_local()` with a `LocalSet`,  
or run it on the main thread while IPC server runs on the tokio worker threads.

### wgpu on Wayland without a window manager surface
`zwlr_layer_surface_v1` provides its own configure event.  
You MUST wait for the configure event before attaching a buffer or rendering.  
Sequence: commit → roundtrip → receive configure → ack_configure → commit again → now render.

### Audio Pre-loading Memory
For wallpapers with long audio tracks (5–10 min), pre-loading may use 500MB+.  
Mitigation for MVP: check file duration before pre-loading, warn if > 10 minutes,  
and use the video file's audio stream which is typically compressed (small).  
Production fix: implement streaming `AudioSamples` that decodes chunk-by-chunk.

### ffmpeg-next Linking
`ffmpeg-next` requires system ffmpeg headers. If build fails with linking errors:  
- Check that `pkg-config` can find `libavcodec`
- `PKG_CONFIG_PATH=/usr/lib/pkgconfig cargo build`
- On Arch: `sudo pacman -S ffmpeg`
