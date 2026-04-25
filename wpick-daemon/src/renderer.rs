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

use crate::hw_decode::{HwDecoder, Nv12Frame};
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
    needs_ack:        bool,
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
            needs_ack:        false,
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
                                name, version.min(4), qh, (),
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
            state.needs_ack        = true;
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
        // E-31: only apply the CURRENT mode — compositors send multiple Mode events
        // (one per supported resolution) before the active one; acting on any of them
        // would set the wrong resolution.
        if let wl_output::Event::Mode { flags, width, height, .. } = event {
            use wayland_client::WEnum;
            let is_current = matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current));
            if is_current && width > 0 && height > 0 {
                state.output_width  = width  as u32;
                state.output_height = height as u32;
            }
        }
    }
}

// ─── Decoder abstraction ──────────────────────────────────────────────────────

/// Wraps either a hardware (VA-API → NV12) or software (swscale → RGBA) decoder.
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
}

// ─── Render state (per active wallpaper) ─────────────────────────────────────

/// Per-frame GPU state — either RGBA (SW path) or NV12 (HW path).
/// Recreated when the wallpaper or its dimensions change.
enum RenderState {
    Rgba {
        texture:    wgpu::Texture,
        // Kept alive so wgpu can track the view's lifetime in the bind group.
        _view:      wgpu::TextureView,
        bind_group: wgpu::BindGroup,
    },
    Nv12 {
        y_tex:      wgpu::Texture,
        uv_tex:     wgpu::Texture,
        // Kept alive so wgpu can track view lifetimes in the bind group.
        _y_view:    wgpu::TextureView,
        _uv_view:   wgpu::TextureView,
        bind_group: wgpu::BindGroup,
    },
}

// ─── wgpu helpers ─────────────────────────────────────────────────────────────

fn make_rgba_texture(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    // FIX 1: Rgba8UnormSrgb matches ffmpeg RGBA output. Surface is Bgra8UnormSrgb;
    // the GPU handles format conversion during the render pass write.
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label:           None,
        size:            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
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

fn make_y_texture(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label:           None,
        size:            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count:    1,
        dimension:       wgpu::TextureDimension::D2,
        format:          wgpu::TextureFormat::R8Unorm,
        usage:           wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats:    &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn make_uv_texture(device: &wgpu::Device, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    // UV texture: half width, half height, Rg8Unorm (R=Cb G=Cr interleaved)
    let (uw, uh) = (w / 2, h / 2);
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label:           None,
        size:            wgpu::Extent3d { width: uw, height: uh, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count:    1,
        dimension:       wgpu::TextureDimension::D2,
        format:          wgpu::TextureFormat::Rg8Unorm,
        usage:           wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats:    &[],
    });
    let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
    (tex, view)
}

fn make_rgba_bind_group(
    device:  &wgpu::Device,
    layout:  &wgpu::BindGroupLayout,
    view:    &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label:   None,
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

fn make_nv12_bind_group(
    device:  &wgpu::Device,
    layout:  &wgpu::BindGroupLayout,
    y_view:  &wgpu::TextureView,
    uv_view: &wgpu::TextureView,
    sampler: &wgpu::Sampler,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label:   None,
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(y_view)  },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(uv_view) },
            wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(sampler)     },
        ],
    })
}

fn upload_rgba(queue: &wgpu::Queue, tex: &wgpu::Texture, rgba: &[u8], w: u32, h: u32) {
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: tex, mip_level: 0,
            origin:  wgpu::Origin3d::ZERO,
            aspect:  wgpu::TextureAspect::All,
        },
        rgba,
        wgpu::ImageDataLayout {
            offset:         0,
            bytes_per_row:  Some(4 * w),
            rows_per_image: Some(h),
        },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
}

fn upload_nv12(queue: &wgpu::Queue, y_tex: &wgpu::Texture, uv_tex: &wgpu::Texture, frame: &Nv12Frame) {
    // Y plane: R8Unorm, width × height, 1 byte per texel
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: y_tex, mip_level: 0,
            origin:  wgpu::Origin3d::ZERO,
            aspect:  wgpu::TextureAspect::All,
        },
        &frame.y,
        wgpu::ImageDataLayout {
            offset:         0,
            bytes_per_row:  Some(frame.width),
            rows_per_image: Some(frame.height),
        },
        wgpu::Extent3d { width: frame.width, height: frame.height, depth_or_array_layers: 1 },
    );

    // UV plane: Rg8Unorm, width/2 × height/2, 2 bytes per texel (Cb, Cr)
    let (uw, uh) = frame.uv_dims();
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: uv_tex, mip_level: 0,
            origin:  wgpu::Origin3d::ZERO,
            aspect:  wgpu::TextureAspect::All,
        },
        &frame.uv,
        wgpu::ImageDataLayout {
            offset:         0,
            bytes_per_row:  Some(uw * 2), // 2 bytes per Rg8Unorm texel
            rows_per_image: Some(uh),
        },
        wgpu::Extent3d { width: uw, height: uh, depth_or_array_layers: 1 },
    );
}

fn render_frame(
    surface:        &wgpu::Surface,
    device:         &wgpu::Device,
    queue:          &wgpu::Queue,
    pipeline:       &wgpu::RenderPipeline,
    bind_group:     &wgpu::BindGroup,
    surface_config: &wgpu::SurfaceConfiguration,
) -> anyhow::Result<bool> {
    let frame = match surface.get_current_texture() {
        Ok(f) => f,
        // Lost and Outdated both mean the swap chain is invalid on Wayland/Vulkan.
        // reconfigure rebuilds only the swap chain — Device/Queue/Pipeline untouched.
        // See E-37, E-39.
        Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
            surface.configure(device, surface_config);
            return Ok(false);
        }
        // Timeout: compositor temporarily busy. Retry next frame without reconfigure.
        Err(wgpu::SurfaceError::Timeout) => return Ok(false),
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
    // FIX 2: wgpu Vulkan WSI (VK_KHR_wayland_surface) manages buffer attachment internally.
    // Do not call wl_surface.commit() here — it would double-commit and blank the surface.
    frame.present();
    Ok(true)
}

// ─── Renderer context ─────────────────────────────────────────────────────────

struct RendererCtx {
    // FIX 3: conn keeps the Wayland socket fd alive for the life of the renderer.
    _conn:          Connection,
    evq:            wayland_client::EventQueue<WaylandState>,
    wls:            WaylandState,
    wl_surface:     wl_surface::WlSurface,
    layer_surface:  ZwlrLayerSurfaceV1,
    wgpu_surface:   wgpu::Surface<'static>,
    device:         wgpu::Device,
    queue:          wgpu::Queue,
    // RGBA pipeline (SW path: swscale → Rgba8UnormSrgb)
    pipeline_rgba:  wgpu::RenderPipeline,
    bg_layout_rgba: wgpu::BindGroupLayout,
    // NV12 pipeline (HW path: VA-API → R8Unorm Y + Rg8Unorm UV → BT.709 shader)
    pipeline_nv12:  wgpu::RenderPipeline,
    bg_layout_nv12: wgpu::BindGroupLayout,
    sampler:        wgpu::Sampler,
    surf_w:         u32,
    surf_h:         u32,
    surface_config: wgpu::SurfaceConfiguration,
}

// ─── Blocking init ────────────────────────────────────────────────────────────

fn init_renderer() -> anyhow::Result<RendererCtx> {
    use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::Layer;
    use raw_window_handle::{
        RawDisplayHandle, RawWindowHandle,
        WaylandDisplayHandle, WaylandWindowHandle,
    };

    tracing::debug!("init_renderer — connecting to Wayland");

    let conn    = Connection::connect_to_env().context("Connect to Wayland display")?;
    let mut evq = conn.new_event_queue::<WaylandState>();
    let qh      = evq.handle();
    let mut wls = WaylandState::default();

    conn.display().get_registry(&qh, ());
    evq.roundtrip(&mut wls).context("globals roundtrip")?;
    tracing::debug!("globals — compositor={} layer_shell={}",
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

    // FIX 4: roundtrip triggers Configure. Init acks manually here; render loop
    // must not re-ack this startup serial (needs_ack cleared below).
    evq.roundtrip(&mut wls).context("configure roundtrip")?;
    tracing::debug!("configure — configured={} surf={}x{}",
        wls.configured, wls.surf_width, wls.surf_height);

    anyhow::ensure!(wls.configured,
        "Layer surface not configured — compositor didn't send Configure event");

    let surf_w = if wls.surf_width  > 0 { wls.surf_width  } else { wls.output_width  };
    let surf_h = if wls.surf_height > 0 { wls.surf_height } else { wls.output_height };

    layer_surface.ack_configure(wls.configure_serial);
    wl_surface.commit();
    evq.flush().context("flush after ack_configure")?;
    // Prevent render loop from re-acking the startup serial (double-ack = protocol error).
    wls.needs_ack = false;

    tracing::debug!("Wayland surface ready: {}x{}", surf_w, surf_h);

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

    tracing::debug!("requesting Vulkan adapter");
    let adapter = pollster::block_on(instance.request_adapter(
        &wgpu::RequestAdapterOptions {
            power_preference:       wgpu::PowerPreference::HighPerformance,
            compatible_surface:     Some(&wgpu_surface),
            force_fallback_adapter: false,
        },
    )).ok_or_else(|| anyhow::anyhow!(
        "No Vulkan adapter — install vulkan-radeon / nvidia-utils / vulkan-intel"
    ))?;

    tracing::info!("Vulkan adapter: {}", adapter.get_info().name);

    let (device, queue) = pollster::block_on(
        adapter.request_device(&wgpu::DeviceDescriptor::default(), None)
    ).context("request_device")?;

    let caps   = wgpu_surface.get_capabilities(&adapter);
    // M-3: caps.formats can be empty on some drivers — avoid panic.
    let format = caps.formats.first().copied()
        .ok_or_else(|| anyhow::anyhow!("GPU reports no surface formats — check Vulkan driver"))?;
    tracing::debug!("surface format = {:?}", format);

    let surface_config = wgpu::SurfaceConfiguration {
        usage:        wgpu::TextureUsages::RENDER_ATTACHMENT,
        format,
        width:        surf_w,
        height:       surf_h,
        present_mode: wgpu::PresentMode::Fifo,
        // B-2: alpha_modes can be empty on some drivers — avoid panic.
        alpha_mode:   caps.alpha_modes.first().copied()
                          .unwrap_or(wgpu::CompositeAlphaMode::Auto),
        view_formats: vec![],
        desired_maximum_frame_latency: 2,
    };
    wgpu_surface.configure(&device, &surface_config);

    // ── Shared vertex shader ──────────────────────────────────────────────────
    let shader_vert = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label:  None,
        source: wgpu::ShaderSource::Wgsl(include_str!("../../assets/vertex.wgsl").into()),
    });

    let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
        address_mode_u: wgpu::AddressMode::ClampToEdge,
        address_mode_v: wgpu::AddressMode::ClampToEdge,
        mag_filter:     wgpu::FilterMode::Linear,
        min_filter:     wgpu::FilterMode::Linear,
        ..Default::default()
    });

    // ── RGBA pipeline (SW fallback) ───────────────────────────────────────────
    let bg_layout_rgba = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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

    let shader_frag_rgba = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label:  None,
        source: wgpu::ShaderSource::Wgsl(include_str!("../../assets/fragment.wgsl").into()),
    });

    let pipeline_rgba = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label:  None,
        layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:                None,
            bind_group_layouts:   &[&bg_layout_rgba],
            push_constant_ranges: &[],
        })),
        vertex: wgpu::VertexState {
            module:              &shader_vert,
            entry_point:         "vs_main",
            buffers:             &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module:              &shader_frag_rgba,
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

    // ── NV12 pipeline (HW path) ───────────────────────────────────────────────
    // Three bindings: Y texture (R8), UV texture (Rg8), shared sampler.
    let bg_layout_nv12 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
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
                ty: wgpu::BindingType::Texture {
                    sample_type:    wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled:   false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding:    2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty:         wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count:      None,
            },
        ],
    });

    let shader_frag_yuv = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label:  None,
        source: wgpu::ShaderSource::Wgsl(include_str!("../../assets/fragment_yuv.wgsl").into()),
    });

    let pipeline_nv12 = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label:  None,
        layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:                None,
            bind_group_layouts:   &[&bg_layout_nv12],
            push_constant_ranges: &[],
        })),
        vertex: wgpu::VertexState {
            module:              &shader_vert,
            entry_point:         "vs_main",
            buffers:             &[],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module:              &shader_frag_yuv,
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

    tracing::debug!("render pipelines ready (RGBA + NV12)");

    Ok(RendererCtx {
        _conn: conn,
        evq,
        wls,
        wl_surface,
        layer_surface,
        wgpu_surface,
        device,
        queue,
        pipeline_rgba,
        bg_layout_rgba,
        pipeline_nv12,
        bg_layout_nv12,
        sampler,
        surf_w,
        surf_h,
        surface_config,
    })
}

// ─── Public async entry point ─────────────────────────────────────────────────

pub async fn run(
    mut wallpaper_rx: watch::Receiver<Option<WallpaperInfo>>,
    mut shutdown_rx:  broadcast::Receiver<()>,
) -> anyhow::Result<()> {
    tracing::debug!("renderer::run started");

    let ctx = tokio::task::spawn_blocking(init_renderer)
        .await
        .context("renderer init thread panicked")?
        .context("renderer init failed")?;

    tracing::debug!("renderer init complete");
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

// ─── Frame action — returned by decode_upload_render, no lifetime deps ───────

/// What the render loop should do after decode_upload_render returns.
/// No references into decoder/render_state — safe to act on after borrow ends.
enum FrameAction {
    /// Frame decoded, uploaded, and rendered. Timing was updated inside.
    Ok,
    /// EOF — call seek_to_start on the current decoder.
    Eof,
    /// HW decode error — try SW fallback for current_wp.
    HwError,
    /// Unrecoverable error — clear decoder and render_state.
    Clear,
    /// Not time to decode yet, or no active decoder.
    Skip,
}

/// Decode one frame, upload to GPU, render. Returns a `FrameAction` that
/// callers act on AFTER this borrow scope ends (so decoder/render_state can
/// be mutated without conflicting with any active borrow).
fn decode_upload_render(
    ctx:        &RendererCtx,
    dec:        &mut AnyDecoder,
    rs:         &RenderState,
    next_frame: &mut Instant,
) -> FrameAction {
    if Instant::now() < *next_frame { return FrameAction::Skip; }

    match (dec, rs) {
        // HW path: VA-API → NV12 → Y+UV upload → BT.709 shader
        (AnyDecoder::Hw(hw), RenderState::Nv12 { y_tex, uv_tex, bind_group, .. }) => {
            match hw.next_nv12_frame() {
                Ok(Some(frame)) => {
                    upload_nv12(&ctx.queue, y_tex, uv_tex, &frame);
                    advance_timer_from_render(
                        ctx, &ctx.pipeline_nv12, bind_group, hw.frame_duration(), next_frame,
                    );
                    FrameAction::Ok
                }
                Ok(None) => FrameAction::Eof,
                Err(e) => {
                    tracing::warn!("HW: decode error: {}", e);
                    FrameAction::HwError
                }
            }
        }
        // SW path: swscale → RGBA → texture → identity shader
        (AnyDecoder::Sw(sw), RenderState::Rgba { texture, bind_group, .. }) => {
            match sw.next_frame_rgba() {
                Ok(Some((rgba, w, h))) => {
                    upload_rgba(&ctx.queue, texture, rgba, w, h);
                    advance_timer_from_render(
                        ctx, &ctx.pipeline_rgba, bind_group, sw.frame_duration(), next_frame,
                    );
                    FrameAction::Ok
                }
                Ok(None)  => FrameAction::Eof,
                Err(e)    => { tracing::warn!("SW: next_frame_rgba: {}", e); FrameAction::Clear }
            }
        }
        // Decoder/state type mismatch — should never happen
        _ => {
            tracing::warn!("render_loop: decoder/render_state type mismatch");
            FrameAction::Clear
        }
    }
}

/// Render a frame and update next_frame based on whether presentation succeeded.
fn advance_timer_from_render(
    ctx:        &RendererCtx,
    pipeline:   &wgpu::RenderPipeline,
    bind_group: &wgpu::BindGroup,
    dur:        Duration,
    next_frame: &mut Instant,
) {
    match render_frame(
        &ctx.wgpu_surface, &ctx.device, &ctx.queue,
        pipeline, bind_group, &ctx.surface_config,
    ) {
        // FIX 5: additive update avoids drift from decode+upload latency.
        Ok(true)  => { *next_frame += dur; }
        // FIX E-38: reset on Lost/Outdated/Timeout to prevent decoder
        // racing ahead while the surface is dead.
        Ok(false) => { *next_frame = Instant::now() + dur; }
        Err(e)    => { tracing::warn!("render_frame: {}", e); }
    }
}

// ─── Synchronous render loop ──────────────────────────────────────────────────

fn render_loop(
    mut ctx: RendererCtx,
    wp_rx:   std::sync::mpsc::Receiver<Option<WallpaperInfo>>,
    sd_rx:   std::sync::mpsc::Receiver<()>,
) -> anyhow::Result<()> {
    let mut decoder:      Option<AnyDecoder>    = None;
    let mut render_state: Option<RenderState>   = None;
    let mut current_wp:   Option<WallpaperInfo> = None;
    let mut next_frame:   Instant               = Instant::now();

    tracing::debug!("render_loop running");

    loop {
        // ── Shutdown ──────────────────────────────────────────────────────────
        if sd_rx.try_recv().is_ok() {
            tracing::info!("Renderer shutting down");
            break;
        }

        // ── Wallpaper change ──────────────────────────────────────────────────
        while let Ok(new_wp) = wp_rx.try_recv() {
            decoder      = None;
            render_state = None;
            if let Some(info) = new_wp {
                tracing::debug!("loading wallpaper: {}", info.title);
                open_wallpaper(&ctx, &info, &mut decoder, &mut render_state);
                current_wp = Some(info);
                next_frame = Instant::now();
            } else {
                current_wp = None;
            }
        }

        // ── Handle Wayland re-configure (e.g. after fullscreen exit) ─────────
        if ctx.wls.needs_ack {
            ctx.wls.needs_ack = false;
            ctx.layer_surface.ack_configure(ctx.wls.configure_serial);
            ctx.wl_surface.commit();
            if let Err(e) = ctx.evq.flush() {
                tracing::warn!("flush after re-configure: {}", e);
            }
            let new_w = if ctx.wls.surf_width  > 0 { ctx.wls.surf_width  } else { ctx.surface_config.width  };
            let new_h = if ctx.wls.surf_height > 0 { ctx.wls.surf_height } else { ctx.surface_config.height };
            if new_w != ctx.surface_config.width || new_h != ctx.surface_config.height {
                ctx.surface_config.width  = new_w;
                ctx.surface_config.height = new_h;
                tracing::info!("Surface resized: {}x{}", new_w, new_h);
            }
            ctx.wgpu_surface.configure(&ctx.device, &ctx.surface_config);
            tracing::info!("Re-configured after compositor event ({}x{})", new_w, new_h);
        }

        // ── Decode + upload + render ──────────────────────────────────────────
        // Phase 1: decode_upload_render borrows decoder/render_state mutably.
        // No mutations of decoder/render_state happen inside — only the action
        // enum is returned. The borrow ends before Phase 2.
        let action = match (decoder.as_mut(), render_state.as_ref()) {
            (Some(dec), Some(rs)) => decode_upload_render(&ctx, dec, rs, &mut next_frame),
            _                     => FrameAction::Skip,
        };

        // Phase 2: act on the result — borrows fully released, safe to mutate.
        match action {
            FrameAction::Eof => {
                if let Some(ref mut dec) = decoder {
                    if let Err(e) = dec.seek_to_start() {
                        tracing::warn!("seek_to_start failed: {} — clearing", e);
                        decoder = None; render_state = None;
                    }
                }
            }
            FrameAction::HwError => {
                tracing::warn!("HW decode error — falling back to SW");
                decoder = None; render_state = None;
                if let Some(ref info) = current_wp {
                    open_wallpaper(&ctx, info, &mut decoder, &mut render_state);
                }
            }
            FrameAction::Clear => { decoder = None; render_state = None; }
            FrameAction::Ok | FrameAction::Skip => {}
        }

        // ── Wayland event dispatch (non-blocking) ─────────────────────────────
        if let Err(e) = ctx.evq.dispatch_pending(&mut ctx.wls) {
            tracing::warn!("dispatch_pending: {}", e);
        }
        if let Err(e) = ctx.evq.flush() {
            tracing::warn!("evq flush: {}", e);
        }

        // ── Frame timing ──────────────────────────────────────────────────────
        let wait = if decoder.is_some() {
            let now = Instant::now();
            if next_frame > now { (next_frame - now).min(Duration::from_millis(8)) }
            else                { Duration::ZERO }
        } else {
            // No active wallpaper — poll slowly to save CPU.
            Duration::from_millis(100)
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

// ─── Wallpaper open helper ────────────────────────────────────────────────────

/// Try to open a wallpaper with HW decode (NV12 path) first, fall back to SW (RGBA path).
/// On success, populates `decoder` and `render_state`.
fn open_wallpaper(
    ctx:          &RendererCtx,
    info:         &WallpaperInfo,
    decoder:      &mut Option<AnyDecoder>,
    render_state: &mut Option<RenderState>,
) {
    // ── Try HW (VA-API → NV12) ────────────────────────────────────────────────
    if let Some(hw) = HwDecoder::try_open(&info.file_path) {
        let dims: (u32, u32) = hw.dimensions();
        let (y_tex, y_view)   = make_y_texture(&ctx.device, dims.0, dims.1);
        let (uv_tex, uv_view) = make_uv_texture(&ctx.device, dims.0, dims.1);
        let bind_group = make_nv12_bind_group(
            &ctx.device, &ctx.bg_layout_nv12, &y_view, &uv_view, &ctx.sampler,
        );
        *decoder = Some(AnyDecoder::Hw(hw));
        *render_state = Some(RenderState::Nv12 {
            y_tex, uv_tex,
            _y_view: y_view, _uv_view: uv_view,
            bind_group,
        });
        tracing::info!("HW NV12 path active: {}x{} — {}", dims.0, dims.1, info.title);
        return;
    }

    // ── SW fallback (swscale → RGBA) ──────────────────────────────────────────
    match VideoDecoder::open(&info.file_path) {
        Ok(sw) => {
            let dims = sw.dimensions();
            let (texture, view) = make_rgba_texture(&ctx.device, dims.0, dims.1);
            let bind_group = make_rgba_bind_group(
                &ctx.device, &ctx.bg_layout_rgba, &view, &ctx.sampler,
            );
            *decoder = Some(AnyDecoder::Sw(sw));
            *render_state = Some(RenderState::Rgba { texture, _view: view, bind_group });
            tracing::info!("SW RGBA path active: {}x{} — {}", dims.0, dims.1, info.title);
        }
        Err(e) => tracing::warn!("VideoDecoder::open: {}", e),
    }
}
