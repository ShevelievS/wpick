use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use anyhow::Context;
use tokio::sync::{broadcast, watch};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle,
    protocol::{wl_compositor, wl_output, wl_registry, wl_surface},
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};
use wpick_core::model::WallpaperInfo;

use crate::video::VideoDecoder;

// ─── Wayland dispatch state ───────────────────────────────────────────────────

struct WaylandState {
    compositor:       Option<wl_compositor::WlCompositor>,
    layer_shell:      Option<ZwlrLayerShellV1>,
    // Kept alive to receive Mode events — never read directly
    _output:          Option<wl_output::WlOutput>,
    output_width:     u32,
    output_height:    u32,
    configured:       bool,
    configure_serial: u32,
    surf_width:       u32,
    surf_height:      u32,
}

impl Default for WaylandState {
    fn default() -> Self {
        Self {
            compositor:       None,
            layer_shell:      None,
            _output:          None,
            output_width:     1920,
            output_height:    1080,
            configured:       false,
            configure_serial: 0,
            surf_width:       0,
            surf_height:      0,
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
                    state.compositor =
                        Some(registry.bind(name, version.min(4), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell =
                        Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_output" => {
                    if state._output.is_none() {
                        state._output = Some(
                            registry.bind::<wl_output::WlOutput, _, _>(
                                name, version.min(3), qh, (),
                            ),
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
        if let zwlr_layer_surface_v1::Event::Configure { serial, width, height } = event {
            state.configure_serial = serial;
            state.surf_width       = width;
            state.surf_height      = height;
            state.configured       = true;
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
        if let wl_output::Event::Mode { width, height, .. } = event {
            if width > 0 && height > 0 {
                state.output_width  = width  as u32;
                state.output_height = height as u32;
            }
        }
    }
}

// ─── wgpu helpers ─────────────────────────────────────────────────────────────

fn make_video_texture(
    device: &wgpu::Device,
    width:  u32,
    height: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    // FIX 1: Texture format must be Rgba8UnormSrgb because ffmpeg outputs RGBA bytes.
    // The surface format (Bgra8UnormSrgb) is separate — the GPU handles the
    // format conversion when the shader writes from RGBA texture to BGRA surface.
    // Using Bgra8UnormSrgb here would swap R/B channels producing wrong colors.
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label:           None,
        size:            wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count:    1,
        dimension:       wgpu::TextureDimension::D2,
        format:          wgpu::TextureFormat::Rgba8UnormSrgb,
        usage:           wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats:    &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn make_bind_group(
    device:  &wgpu::Device,
    layout:  &wgpu::BindGroupLayout,
    view:    &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label:   None,
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding:  0,
                resource: wgpu::BindingResource::TextureView(view),
            },
            wgpu::BindGroupEntry {
                binding:  1,
                resource: wgpu::BindingResource::Sampler(sampler),
            },
        ],
    })
}

fn upload_frame(
    queue:   &wgpu::Queue,
    texture: &wgpu::Texture,
    rgba:    &[u8],
    width:   u32,
    height:  u32,
) {
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin:    wgpu::Origin3d::ZERO,
            aspect:    wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::ImageDataLayout {
            offset:         0,
            bytes_per_row:  Some(4 * width),
            rows_per_image: Some(height),
        },
        wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
    );
}

fn render_frame(
    surface:    &wgpu::Surface,
    device:     &wgpu::Device,
    queue:      &wgpu::Queue,
    pipeline:   &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
) -> anyhow::Result<bool> {
    let frame = match surface.get_current_texture() {
        Ok(f) => f,
        Err(wgpu::SurfaceError::Timeout | wgpu::SurfaceError::Outdated) => return Ok(false),
        Err(e) => return Err(anyhow::anyhow!("Surface error: {}", e)),
    };
    let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut rpass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view:           &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load:  wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        rpass.set_pipeline(pipeline);
        rpass.set_bind_group(0, bind_group, &[]);
        rpass.draw(0..6, 0..1);
    }
    queue.submit(std::iter::once(enc.finish()));
    // FIX 2: frame.present() is all that is needed. The wgpu Vulkan backend uses
    // VK_KHR_wayland_surface which internally manages wl_surface buffer attachment
    // and commit via the Vulkan WSI layer. Calling wl_surface.commit() separately
    // AFTER this causes a double-commit that resets the compositor's buffer state,
    // making the surface appear blank.
    frame.present();
    Ok(true)
}

// ─── Renderer context ─────────────────────────────────────────────────────────

struct RendererCtx {
    // FIX 3: conn must be kept alive — it owns the Wayland socket fd.
    // Without it the display connection closes and wgpu_surface becomes invalid.
    // Prefix with _ to suppress dead_code warning while keeping the value alive.
    _conn:         Connection,
    evq:           wayland_client::EventQueue<WaylandState>,
    wls:           WaylandState,
    wl_surface:    wl_surface::WlSurface,
    layer_surface: ZwlrLayerSurfaceV1,
    wgpu_surface:  wgpu::Surface<'static>,
    device:        wgpu::Device,
    queue:         wgpu::Queue,
    pipeline:      wgpu::RenderPipeline,
    bg_layout:     wgpu::BindGroupLayout,
    sampler:       wgpu::Sampler,
    surf_w:        u32,
    surf_h:        u32,
}

// ─── Blocking init ────────────────────────────────────────────────────────────

fn init_renderer() -> anyhow::Result<RendererCtx> {
    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
    use raw_window_handle::{
        RawDisplayHandle, RawWindowHandle,
        WaylandDisplayHandle, WaylandWindowHandle,
    };

    eprintln!("DEBUG: init_renderer — connecting to Wayland");

    let conn    = Connection::connect_to_env().context("Connect to Wayland display")?;
    let mut evq = conn.new_event_queue::<WaylandState>();
    let qh      = evq.handle();
    let mut wls = WaylandState::default();

    conn.display().get_registry(&qh, ());
    evq.roundtrip(&mut wls).context("globals roundtrip")?;
    eprintln!("DEBUG: globals — compositor={} layer_shell={}",
        wls.compositor.is_some(), wls.layer_shell.is_some());

    let compositor  = wls.compositor.take()
        .ok_or_else(|| anyhow::anyhow!("No wl_compositor global"))?;
    let layer_shell = wls.layer_shell.take()
        .ok_or_else(|| anyhow::anyhow!(
            "No zwlr_layer_shell_v1 — use Hyprland/Sway/river (not GNOME/KDE)"
        ))?;

    let wl_surface    = compositor.create_surface(&qh, ());
    let layer_surface = layer_shell.get_layer_surface(
        &wl_surface,
        None,
        Layer::Bottom,
        "wpick".to_string(),
        &qh,
        (),
    );

    layer_surface.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
    layer_surface.set_size(0, 0);
    layer_surface.set_exclusive_zone(-1);
    wl_surface.commit();

    // FIX 4: roundtrip triggers Configure event. Our Dispatch impl now calls
    // ack_configure() immediately inside the event handler (required by protocol).
    // After roundtrip we call ack_configure() again + commit() for safety.
    evq.roundtrip(&mut wls).context("configure roundtrip")?;
    eprintln!("DEBUG: configure — configured={} surf={}x{}",
        wls.configured, wls.surf_width, wls.surf_height);

    anyhow::ensure!(wls.configured,
        "Layer surface not configured — compositor didn't send Configure event");

    let surf_w = if wls.surf_width  > 0 { wls.surf_width  } else { wls.output_width  };
    let surf_h = if wls.surf_height > 0 { wls.surf_height } else { wls.output_height };

    // Explicit ack + commit after roundtrip to ensure compositor is satisfied
    layer_surface.ack_configure(wls.configure_serial);
    wl_surface.commit();
    evq.flush().context("flush after ack_configure")?;

    eprintln!("DEBUG: Wayland surface ready: {}x{}", surf_w, surf_h);

    // ── wgpu ──────────────────────────────────────────────────────────────────
    let backend     = conn.display().backend().upgrade()
        .ok_or_else(|| anyhow::anyhow!("Wayland backend unavailable"))?;
    let display_ptr = backend.display_ptr() as *mut c_void;
    let surface_ptr = wl_surface.id().as_ptr() as *mut c_void;

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..Default::default()
    });

    let raw_display = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
        NonNull::new(display_ptr).ok_or_else(|| anyhow::anyhow!("null display ptr"))?,
    ));
    let raw_window = RawWindowHandle::Wayland(WaylandWindowHandle::new(
        NonNull::new(surface_ptr).ok_or_else(|| anyhow::anyhow!("null surface ptr"))?,
    ));

    // SAFETY: _conn keeps the Wayland connection alive for the duration of RendererCtx.
    // wl_surface is stored in RendererCtx and outlives wgpu_surface.
    let wgpu_surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: raw_display,
            raw_window_handle:  raw_window,
        })
    }.context("create wgpu surface")?;

    eprintln!("DEBUG: requesting Vulkan adapter");
    let adapter = pollster::block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference:       wgpu::PowerPreference::HighPerformance,
            compatible_surface:     Some(&wgpu_surface),
            force_fallback_adapter: false,
        },
    )).ok_or_else(|| anyhow::anyhow!(
        "No Vulkan adapter — install vulkan-radeon / nvidia-utils / vulkan-intel"
    ))?;

    eprintln!("DEBUG: adapter = {}", adapter.get_info().name);

    let (device, queue) = pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default(), None)
    ).context("request_device")?;

    let caps   = wgpu_surface.get_capabilities(&adapter);
    let format = caps.formats[0];
    eprintln!("DEBUG: surface format = {:?}", format);

    wgpu_surface.configure(&device, &wgpu::SurfaceConfiguration {
        usage:        wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width:        surf_w,
        height:       surf_h,
        present_mode: wgpu::PresentMode::Fifo,
        alpha_mode:   caps.alpha_modes[0],
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    });

    // ── Pipeline ──────────────────────────────────────────────────────────────
    let shader_vert = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label:  None,
        source: wgpu::ShaderSource::Wgsl(include_str!("../../assets/vertex.wgsl").into()),
    });
    let shader_frag = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label:  None,
        source: wgpu::ShaderSource::Wgsl(include_str!("../../assets/fragment.wgsl").into()),
    });

    let bg_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label:   None,
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding:    0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type:    wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled:   false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding:    1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty:         wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count:      None,
            },
        ],
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter:     wgpu::FilterMode::Linear,
        min_filter:     wgpu::FilterMode::Linear,
        ..Default::default()
    });

    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label:  None,
        layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:                None,
            bind_group_layouts:   &[&bg_layout],
            push_constant_ranges: &[],
        })),
        vertex: wgpu::VertexState {
            module:              &shader_vert,
            entry_point:         "vs_main",
            buffers:             &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module:              &shader_frag,
            entry_point:         "fs_main",
            targets:             &[Some(wgpu::ColorTargetState {
                format,
                blend:      None,
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: Default::default(),
        }),
        primitive:     wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample:   wgpu::MultisampleState::default(),
        multiview:     None,
        cache:         None,
    });

    eprintln!("DEBUG: render pipeline ready");

    Ok(RendererCtx {
        _conn: conn,
        evq,
        wls,
        wl_surface,
        layer_surface,
        wgpu_surface,
        device,
        queue,
        pipeline,
        bg_layout,
        sampler,
        surf_w,
        surf_h,
    })
}

// ─── Public async entry point ─────────────────────────────────────────────────

pub async fn run(
    mut wallpaper_rx: watch::Receiver<Option<WallpaperInfo>>,
    mut shutdown_rx:  broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    eprintln!("DEBUG: renderer::run started");

    let ctx = tokio::task::spawn_blocking(init_renderer)
        .await
        .context("renderer init thread panicked")?
        .context("renderer init failed")?;

    eprintln!("DEBUG: renderer init complete");
    tracing::info!("Renderer ready: {}x{}", ctx.surf_w, ctx.surf_h);

    let (wp_tx, wp_rx) = std::sync::mpsc::channel::<Option<WallpaperInfo>>();
    let (sd_tx, sd_rx) = std::sync::mpsc::channel::<()>();

    let wp_forward = tokio::spawn(async move {
        loop {
            if wallpaper_rx.changed().await.is_err() { break; }
            let val = wallpaper_rx.borrow_and_update().clone();
            if wp_tx.send(val).is_err() { break; }
        }
    });

    let sd_forward = tokio::spawn(async move {
        let _ = shutdown_rx.recv().await;
        let _ = sd_tx.send(());
    });

    tokio::task::spawn_blocking(move || render_loop(ctx, wp_rx, sd_rx))
        .await
        .context("render loop thread panicked")??;

    wp_forward.abort();
    sd_forward.abort();

    Ok(())
}

// ─── Synchronous render loop ──────────────────────────────────────────────────

fn render_loop(
    mut ctx: RendererCtx,
    wp_rx:   std::sync::mpsc::Receiver<Option<WallpaperInfo>>,
    sd_rx:   std::sync::mpsc::Receiver<()>,
) -> anyhow::Result<()> {
    let mut decoder:    Option<VideoDecoder> = None;
    // FIX 5: Use a base timestamp for frame timing to avoid drift.
    // next_frame is updated by += duration, not = now() + duration.
    let mut next_frame: Instant              = Instant::now();
    let mut tex_dims = (1u32, 1u32);

    let (mut video_tex, mut tex_view) = make_video_texture(&ctx.device, 1, 1);
    let mut bind_group = make_bind_group(&ctx.device, &ctx.bg_layout, &tex_view, &ctx.sampler);

    eprintln!("DEBUG: render_loop running");

    loop {
        // ── Shutdown ──────────────────────────────────────────────────────────
        if sd_rx.try_recv().is_ok() {
            tracing::info!("Renderer shutting down");
            break;
        }

        // ── Wallpaper change ──────────────────────────────────────────────────
        while let Ok(new_wp) = wp_rx.try_recv() {
            decoder = None;
            if let Some(ref info) = new_wp {
                eprintln!("DEBUG: loading wallpaper: {}", info.title);
                match VideoDecoder::open(&info.file_path) {
                    Ok(dec) => {
                        let dims = dec.dimensions();
                        if dims != tex_dims {
                            let (t, v) = make_video_texture(&ctx.device, dims.0, dims.1);
                            video_tex  = t;
                            tex_view   = v;
                            bind_group = make_bind_group(
                                &ctx.device, &ctx.bg_layout, &tex_view, &ctx.sampler,
                            );
                            tex_dims = dims;
                        }
                        tracing::info!("Video loaded: {}x{} — {}", dims.0, dims.1, info.title);
                        eprintln!("DEBUG: video decoder ready {}x{}", dims.0, dims.1);
                        decoder    = Some(dec);
                        next_frame = Instant::now();
                    }
                    Err(e) => {
                        tracing::warn!("VideoDecoder::open: {}", e);
                        eprintln!("DEBUG: VideoDecoder error: {}", e);
                    }
                }
            }
        }

        // ── Decode + upload + render ──────────────────────────────────────────
        if let Some(ref mut dec) = decoder {
            if Instant::now() >= next_frame {
                match dec.next_frame_rgba() {
                    Ok(Some((rgba, w, h))) => {
                        upload_frame(&ctx.queue, &video_tex, &rgba, w, h);

                        match render_frame(
                            &ctx.wgpu_surface, &ctx.device, &ctx.queue,
                            &ctx.pipeline, &bind_group,
                        ) {
                            // FIX 2 applied here: NO wl_surface.commit() after present.
                            // wgpu Vulkan backend (VK_KHR_wayland_surface) handles
                            // buffer attachment and surface commit internally.
                            // A manual commit here causes a double-commit that makes
                            // the compositor discard the rendered buffer.
                            Ok(true)  => {}
                            Ok(false) => {} // Timeout/Outdated — compositor busy, retry
                            Err(e)    => tracing::warn!("render_frame: {}", e),
                        }

                        // FIX 5: Additive timing avoids drift accumulation over time.
                        // next_frame = Instant::now() + duration would drift by the
                        // time spent in decode+upload+render each frame.
                        next_frame += dec.frame_duration();
                    }
                    Ok(None) => {
                        // EOF — seek to beginning for seamless loop
                        if let Err(e) = dec.seek_to_start() {
                            tracing::warn!("seek_to_start: {}", e);
                            decoder = None;
                        }
                    }
                    Err(e) => {
                        tracing::warn!("next_frame_rgba: {}", e);
                        decoder = None;
                    }
                }
            }
        }

        // ── Wayland event dispatch (non-blocking) ─────────────────────────────
        if let Err(e) = ctx.evq.dispatch_pending(&mut ctx.wls) {
            tracing::warn!("dispatch_pending: {}", e);
        }
        if let Err(e) = ctx.evq.flush() {
            tracing::warn!("evq flush: {}", e);
        }

        // ── Frame timing: sleep only as long as needed ────────────────────────
        let wait = if decoder.is_some() {
            let now = Instant::now();
            if next_frame > now { (next_frame - now).min(Duration::from_millis(8)) }
            else                { Duration::ZERO }
        } else {
            Duration::from_millis(16)
        };

        if wait > Duration::ZERO {
            std::thread::sleep(wait);
        }
    }

    // Proper Wayland cleanup order: layer_surface before wl_surface
    drop(ctx.layer_surface);
    drop(ctx.wl_surface);
    let _ = ctx.evq.flush();

    Ok(())
}