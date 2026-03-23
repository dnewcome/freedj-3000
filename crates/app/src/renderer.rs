//! wgpu + egui renderer for the MVP waveform display.
//!
//! Two render passes per frame:
//!   Pass 1 — custom waveform shader (fullscreen quad, scrolling texture)
//!   Pass 2 — egui overlay (time counter, play state, instructions)

use anyhow::{Context, Result};
use opendeck_analysis::WaveformCache;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;

// ── Uniform struct ────────────────────────────────────────────────────────────

/// Sent to the waveform shader every frame.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WaveformParams {
    /// Index of the column at the horizontal centre of the screen (float).
    playhead_col: f32,
    /// How many columns are visible across the full screen width.
    cols_visible:  f32,
    /// Total number of valid columns in the texture.
    num_cols:     f32,
    /// Actual texture width (may be padded to satisfy alignment).
    tex_width:    f32,
    /// Surface width in pixels.
    screen_w:     f32,
    /// Surface height in pixels.
    screen_h:     f32,
    _pad:         [f32; 2],
}

// ── Renderer ──────────────────────────────────────────────────────────────────

pub struct Renderer {
    surface:        wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    device:         wgpu::Device,
    queue:          wgpu::Queue,

    // Waveform pass
    waveform_pipeline:   wgpu::RenderPipeline,
    waveform_bind_group: wgpu::BindGroup,
    params_buf:          wgpu::Buffer,
    num_cols:            u32,
    tex_width:           u32,   // padded texture width

    // egui pass
    egui_renderer:  egui_wgpu::Renderer,
    egui_screen:    egui_wgpu::ScreenDescriptor,
}

impl Renderer {
    pub async fn new(window: Arc<Window>, waveform: &WaveformCache) -> Result<Self> {
        let size = window.inner_size();

        // ── wgpu instance / surface ───────────────────────────────────────────
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends:              wgpu::Backends::all(),
            dx12_shader_compiler:  wgpu::Dx12Compiler::default(),
            gles_minor_version:    wgpu::Gles3MinorVersion::Automatic,
            flags:                 wgpu::InstanceFlags::default(),
        });

        let surface = instance
            .create_surface(Arc::clone(&window))
            .context("failed to create wgpu surface")?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference:       wgpu::PowerPreference::HighPerformance,
                compatible_surface:     Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("no compatible GPU adapter found")?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label:             Some("opendeck"),
                    required_features: wgpu::Features::empty(),
                    required_limits:   wgpu::Limits::downlevel_defaults(),
                    memory_hints:      wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .context("failed to get GPU device")?;

        // ── Surface configuration ─────────────────────────────────────────────
        let caps   = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let surface_config = wgpu::SurfaceConfiguration {
            usage:                        wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width:                        size.width.max(1),
            height:                       size.height.max(1),
            present_mode:                 wgpu::PresentMode::Fifo,
            alpha_mode:                   caps.alpha_modes[0],
            view_formats:                 vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        // ── Waveform texture ──────────────────────────────────────────────────
        // Texture is 1-pixel tall, N columns wide.
        // bytes_per_row must be a multiple of 256 = COPY_BYTES_PER_ROW_ALIGNMENT.
        // Rgba8 = 4 bytes/pixel, so width must be a multiple of 64.
        let num_cols = waveform.len() as u32;
        let tex_width = (num_cols + 63) & !63;  // round up to multiple of 64

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label:             Some("waveform"),
            size:              wgpu::Extent3d { width: tex_width, height: 1, depth_or_array_layers: 1 },
            mip_level_count:   1,
            sample_count:      1,
            dimension:         wgpu::TextureDimension::D2,
            format:            wgpu::TextureFormat::Rgba8Unorm,
            usage:             wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats:      &[],
        });

        // Build padded RGBA data (zero-pad any unused columns at the end).
        let bytes_per_row = tex_width * 4;
        let mut tex_data  = vec![0u8; (bytes_per_row) as usize];
        for (i, col) in waveform.columns.iter().enumerate() {
            let off = i * 4;
            tex_data[off..off + 4].copy_from_slice(col);
        }

        queue.write_texture(
            texture.as_image_copy(),
            &tex_data,
            wgpu::ImageDataLayout {
                offset:         0,
                bytes_per_row:  Some(bytes_per_row),
                rows_per_image: Some(1),
            },
            wgpu::Extent3d { width: tex_width, height: 1, depth_or_array_layers: 1 },
        );

        let tex_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let sampler  = device.create_sampler(&wgpu::SamplerDescriptor {
            label:        Some("waveform_sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter:   wgpu::FilterMode::Linear,
            min_filter:   wgpu::FilterMode::Linear,
            ..Default::default()
        });

        // ── Params uniform buffer ─────────────────────────────────────────────
        let initial_params = WaveformParams {
            playhead_col:  0.0,
            cols_visible:  300.0,
            num_cols:      num_cols as f32,
            tex_width:     tex_width as f32,
            screen_w:      size.width as f32,
            screen_h:      size.height as f32,
            _pad:          [0.0; 2],
        };
        let params_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    Some("waveform_params"),
            contents: bytemuck::bytes_of(&initial_params),
            usage:    wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // ── Bind group layout ─────────────────────────────────────────────────
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label:   Some("waveform_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding:    0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        multisampled:   false,
                        view_dimension: wgpu::TextureViewDimension::D2,
                        sample_type:    wgpu::TextureSampleType::Float { filterable: true },
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:    1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding:    2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty:                 wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size:   None,
                    },
                    count: None,
                },
            ],
        });

        let waveform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label:  Some("waveform_bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&tex_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) },
                wgpu::BindGroupEntry { binding: 2, resource: params_buf.as_entire_binding() },
            ],
        });

        // ── Waveform render pipeline ──────────────────────────────────────────
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label:  Some("waveform_shader"),
            source: wgpu::ShaderSource::Wgsl(WAVEFORM_WGSL.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label:                Some("waveform_layout"),
            bind_group_layouts:   &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let waveform_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label:  Some("waveform_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module:      &shader,
                entry_point: Some("vs_main"),
                buffers:     &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module:      &shader,
                entry_point: Some("fs_main"),
                targets:     &[Some(wgpu::ColorTargetState {
                    format:     format,
                    blend:      None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology:  wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil:  None,
            multisample:    wgpu::MultisampleState::default(),
            multiview:      None,
            cache:          None,
        });

        // ── egui renderer ─────────────────────────────────────────────────────
        let egui_renderer = egui_wgpu::Renderer::new(&device, format, None, 1, false);
        let scale_factor  = window.scale_factor() as f32;
        let egui_screen   = egui_wgpu::ScreenDescriptor {
            size_in_pixels:   [size.width, size.height],
            pixels_per_point: scale_factor,
        };

        Ok(Self {
            surface,
            surface_config,
            device,
            queue,
            waveform_pipeline,
            waveform_bind_group,
            params_buf,
            num_cols,
            tex_width,
            egui_renderer,
            egui_screen,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width  = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        self.egui_screen.size_in_pixels = [width, height];
    }

    /// Render one frame.
    ///
    /// `playhead_sample`: current play position in PCM samples.
    /// `sample_rate`: the track's sample rate (samples per second per channel).
    /// `channels`: channel count (2 for stereo).
    pub fn render(
        &mut self,
        playhead_sample:  u64,
        sample_rate:      u32,
        channels:         u8,
        egui_ctx:         &egui::Context,
        full_output:      egui::FullOutput,
    ) {
        let pixels_per_point = full_output.pixels_per_point;
        let egui_shapes      = full_output.shapes;
        let textures_delta   = full_output.textures_delta;
        // ── Update waveform scroll params ─────────────────────────────────────
        let hop_size  = opendeck_analysis::waveform::HOP_SIZE as f32;
        let playhead_col = playhead_sample as f32 / channels as f32 / hop_size;
        let cols_visible = 600.0_f32;  // tune for desired zoom level

        let params = WaveformParams {
            playhead_col,
            cols_visible,
            num_cols:  self.num_cols as f32,
            tex_width: self.tex_width as f32,
            screen_w:  self.surface_config.width  as f32,
            screen_h:  self.surface_config.height as f32,
            _pad:      [0.0; 2],
        };
        self.queue.write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&params));

        // ── Get surface texture ───────────────────────────────────────────────
        let output = match self.surface.get_current_texture() {
            Ok(t)  => t,
            Err(e) => { log::warn!("surface error: {e}"); return; }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });

        // ── Pass 1: waveform ──────────────────────────────────────────────────
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("waveform_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view:           &view,
                    resolve_target: None,
                    ops:            wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color { r: 0.04, g: 0.04, b: 0.04, a: 1.0 }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set:      None,
                timestamp_writes:         None,
            });
            pass.set_pipeline(&self.waveform_pipeline);
            pass.set_bind_group(0, &self.waveform_bind_group, &[]);
            pass.draw(0..3, 0..1);  // fullscreen triangle
        }

        // ── Pass 2: egui overlay ──────────────────────────────────────────────
        // Upload any new/changed textures (e.g. font atlas) before tessellating.
        for (id, delta) in &textures_delta.set {
            self.egui_renderer.update_texture(&self.device, &self.queue, *id, delta);
        }
        for id in &textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        let primitives = egui_ctx.tessellate(egui_shapes, pixels_per_point);
        self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &primitives,
            &self.egui_screen,
        );
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view:           &view,
                    resolve_target: None,
                    ops:            wgpu::Operations {
                        load:  wgpu::LoadOp::Load,  // don't clear — keep waveform
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set:      None,
                timestamp_writes:         None,
            });
            self.egui_renderer.render(&mut pass, &primitives, &self.egui_screen);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }
}

// ── WGSL shader ───────────────────────────────────────────────────────────────

// Make HOP_SIZE visible so renderer can use it without a circular dependency.
pub use opendeck_analysis::waveform::HOP_SIZE;

const WAVEFORM_WGSL: &str = r#"
// Waveform display shader.
//
// Renders a frequency-colored bar-chart waveform.
// The waveform texture is 1 pixel tall; each texel is [R,G,B,Amp].
// R = bass, G = mid, B = high, A = overall amplitude (bar height).
//
// The bar extends from the vertical center up and down, scaled by amplitude.
// A white hairline at the horizontal center marks the playhead.

struct Params {
    playhead_col: f32,   // column index at screen center
    cols_visible:  f32,  // columns visible across full width
    num_cols:     f32,   // number of valid columns
    tex_width:    f32,   // texture width (may be padded > num_cols)
    screen_w:     f32,
    screen_h:     f32,
    _pad:         vec2<f32>,
};

@group(0) @binding(0) var waveform_tex: texture_2d<f32>;
@group(0) @binding(1) var waveform_smp: sampler;
@group(0) @binding(2) var<uniform> p: Params;

// ── Vertex: fullscreen triangle (no vertex buffer) ────────────────────────────
// Three vertices covering the entire clip space.  Only the lower-left triangle
// of the clip quad is drawn, which is sufficient for a fullscreen pass.
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32(vi & 1u) * 4.0 - 1.0;
    let y = f32((vi >> 1u) & 1u) * (-4.0) + 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

// ── Fragment ──────────────────────────────────────────────────────────────────
@fragment
fn fs_main(@builtin(position) frag_pos: vec4<f32>) -> @location(0) vec4<f32> {
    // Screen UV: x in [0,1] left→right, y in [0,1] top→bottom.
    let sx = frag_pos.x / p.screen_w;
    let sy = frag_pos.y / p.screen_h;

    // White playhead hairline at x = 0.5 (centre of screen).
    if abs(frag_pos.x - p.screen_w * 0.5) < 1.5 {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }

    // Map screen x → column index.
    let half  = p.cols_visible * 0.5;
    let left  = p.playhead_col - half;
    let col_f = left + sx * p.cols_visible;

    // Out of track range → dark.
    if col_f < 0.0 || col_f >= p.num_cols {
        return vec4<f32>(0.04, 0.04, 0.04, 1.0);
    }

    // Sample the waveform texture.
    let tex_u  = col_f / p.tex_width;
    let sample = textureSample(waveform_tex, waveform_smp, vec2<f32>(tex_u, 0.5));

    let r   = sample.r;
    let g   = sample.g;
    let b   = sample.b;
    let amp = sample.a;   // 0–1 bar height

    // Bar chart: filled within amplitude, dark outside.
    // dist_from_centre: 0 at middle of screen, 1 at top/bottom edges.
    let dist = abs(sy - 0.5) * 2.0;

    if dist < amp {
        // Slightly dim the colour toward the bar edges for a shaded look.
        let shade = 1.0 - dist / (amp + 0.001) * 0.3;
        return vec4<f32>(r * shade, g * shade, b * shade, 1.0);
    }

    // Dark background with subtle grid lines every 16 columns.
    let grid = col_f % 16.0;
    let bg   = select(0.04, 0.06, grid < 0.5);
    return vec4<f32>(bg, bg, bg, 1.0);
}
"#;
