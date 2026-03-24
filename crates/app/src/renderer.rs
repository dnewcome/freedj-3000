//! wgpu + egui renderer for the MVP waveform display.
//!
//! Two render passes per frame:
//!   Pass 1 — custom waveform shader (fullscreen quad, scrolling storage buffer)
//!   Pass 2 — egui overlay (time counter, play state, instructions)

use anyhow::{Context, Result};
use opendeck_analysis::WaveformCache;
use opendeck_types::BeatGrid;
use std::sync::Arc;
use wgpu::util::DeviceExt;
use winit::window::Window;

// ── Uniform struct ────────────────────────────────────────────────────────────

/// Sent to the waveform shader every frame.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct WaveformParams {
    /// Index of the column at the horizontal centre of the screen (float).
    playhead_col:     f32,
    /// How many columns are visible across the full screen width.
    cols_visible:     f32,
    /// Total number of valid columns in the buffer.
    num_cols:         f32,
    /// Surface width in pixels.
    screen_w:         f32,
    /// Surface height in pixels.
    screen_h:         f32,
    /// Beat grid: column index of anchor beat (0 if no grid).
    beat_anchor_col:  f32,
    /// Beat grid: columns per beat (0 if no grid).
    beat_period_cols: f32,
    /// Which beat within the bar beat 0 falls on (0 = beat 0 is a downbeat).
    downbeat_offset:  f32,
    /// Beats per bar (4 for 4/4).
    beats_per_bar:    f32,
    _pad0:            f32,
    _pad1:            f32,
    _pad2:            f32,
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
            present_mode:                 wgpu::PresentMode::AutoNoVsync,
            alpha_mode:                   caps.alpha_modes[0],
            view_formats:                 vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        // ── Waveform storage buffer ───────────────────────────────────────────
        // Pack each [R,G,B,A] column into a single u32 (little-endian bytes).
        // No texture dimension limits — storage buffers handle arbitrary sizes.
        let num_cols = waveform.len() as u32;
        let waveform_data: Vec<u32> = waveform.columns.iter()
            .map(|col| u32::from_le_bytes(*col))
            .collect();

        let waveform_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label:    Some("waveform_data"),
            contents: bytemuck::cast_slice(&waveform_data),
            usage:    wgpu::BufferUsages::STORAGE,
        });

        // ── Params uniform buffer ─────────────────────────────────────────────
        let initial_params = WaveformParams {
            playhead_col:     0.0,
            cols_visible:     600.0,
            num_cols:         num_cols as f32,
            screen_w:         size.width as f32,
            screen_h:         size.height as f32,
            beat_anchor_col:  0.0,
            beat_period_cols: 0.0,
            downbeat_offset:  0.0,
            beats_per_bar:    4.0,
            _pad0:            0.0,
            _pad1:            0.0,
            _pad2:            0.0,
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
                // binding 0: waveform storage buffer (read-only)
                wgpu::BindGroupLayoutEntry {
                    binding:    0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty:                 wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size:   None,
                    },
                    count: None,
                },
                // binding 1: params uniform
                wgpu::BindGroupLayoutEntry {
                    binding:    1,
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
                wgpu::BindGroupEntry { binding: 0, resource: waveform_buf.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: params_buf.as_entire_binding() },
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
                entry_point: "vs_main",
                buffers:     &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module:      &shader,
                entry_point: "fs_main",
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

    pub fn render(
        &mut self,
        playhead_sample:  u64,
        sample_rate:      u32,
        channels:         u8,
        beat_grid:        Option<&BeatGrid>,
        egui_ctx:         &egui::Context,
        full_output:      egui::FullOutput,
    ) {
        let pixels_per_point = full_output.pixels_per_point;
        let egui_shapes      = full_output.shapes;
        let textures_delta   = full_output.textures_delta;

        // ── Update waveform scroll params ─────────────────────────────────────
        let hop_size     = opendeck_analysis::waveform::HOP_SIZE as f32;
        let playhead_col = playhead_sample as f32 / channels as f32 / hop_size;

        let (beat_anchor_col, beat_period_cols, downbeat_offset, beats_per_bar) = beat_grid
            .map(|g| {
                let anchor = g.anchor_sample as f32 / hop_size;
                let period = (sample_rate as f32 * 60.0 / g.bpm as f32) / hop_size;
                (anchor, period, g.downbeat_offset as f32, 4.0f32)
            })
            .unwrap_or((0.0, 0.0, 0.0, 4.0));

        let params = WaveformParams {
            playhead_col,
            cols_visible:     600.0,
            num_cols:         self.num_cols as f32,
            screen_w:         self.surface_config.width  as f32,
            screen_h:         self.surface_config.height as f32,
            beat_anchor_col,
            beat_period_cols,
            downbeat_offset,
            beats_per_bar,
            _pad0:            0.0,
            _pad1:            0.0,
            _pad2:            0.0,
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
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view:           &view,
                        resolve_target: None,
                        ops:            wgpu::Operations {
                            load:  wgpu::LoadOp::Load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    occlusion_query_set:      None,
                    timestamp_writes:         None,
                })
                .forget_lifetime();
            self.egui_renderer.render(&mut pass, &primitives, &self.egui_screen);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();
    }
}

// ── WGSL shader ───────────────────────────────────────────────────────────────

const WAVEFORM_WGSL: &str = r#"
// Waveform display shader.
//
// Waveform data is a storage buffer of u32 values, one per column.
// Each u32 packs [R, G, B, Amp] as little-endian bytes:
//   bits  0-7:  R (bass energy)
//   bits  8-15: G (mid energy)
//   bits 16-23: B (high energy)
//   bits 24-31: A (overall amplitude, controls bar height)

struct Params {
    playhead_col:      f32,  // column index at screen center
    cols_visible:      f32,  // columns visible across full width
    num_cols:          f32,  // number of valid columns in the buffer
    screen_w:          f32,
    screen_h:          f32,
    beat_anchor_col:   f32,  // column of beat 0 (0 = no grid)
    beat_period_cols:  f32,  // columns per beat (0 = no grid)
    downbeat_offset:   f32,  // which beat within the bar is beat 0
    beats_per_bar:     f32,  // 4 for 4/4
    _pad0:             f32,
    _pad1:             f32,
    _pad2:             f32,
};

@group(0) @binding(0) var<storage, read> waveform: array<u32>;
@group(0) @binding(1) var<uniform> p: Params;

// ── Vertex: fullscreen triangle ───────────────────────────────────────────────
@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32(vi & 1u) * 4.0 - 1.0;
    let y = f32((vi >> 1u) & 1u) * (-4.0) + 1.0;
    return vec4<f32>(x, y, 0.0, 1.0);
}

// ── Fragment ──────────────────────────────────────────────────────────────────
@fragment
fn fs_main(@builtin(position) frag_pos: vec4<f32>) -> @location(0) vec4<f32> {
    let sx = frag_pos.x / p.screen_w;
    let sy = frag_pos.y / p.screen_h;

    // White playhead hairline at x = 0.5.
    if abs(frag_pos.x - p.screen_w * 0.5) < 1.5 {
        return vec4<f32>(1.0, 1.0, 1.0, 1.0);
    }

    // Map screen x → column index.
    let half  = p.cols_visible * 0.5;
    let col_f = (p.playhead_col - half) + sx * p.cols_visible;

    // Out of track range → dark background.
    if col_f < 0.0 || col_f >= p.num_cols {
        return vec4<f32>(0.04, 0.04, 0.04, 1.0);
    }

    // Read packed waveform column — bilinear interpolation between adjacent
    // columns so the waveform scrolls sub-pixel smoothly.
    let col_lo  = u32(col_f);
    let col_hi  = min(col_lo + 1u, u32(p.num_cols) - 1u);
    let frac    = col_f - f32(col_lo);

    let p0 = waveform[col_lo];
    let p1 = waveform[col_hi];

    let r   = mix(f32( p0        & 0xFFu) / 255.0, f32( p1        & 0xFFu) / 255.0, frac);
    let g   = mix(f32((p0 >>  8u) & 0xFFu) / 255.0, f32((p1 >>  8u) & 0xFFu) / 255.0, frac);
    let b   = mix(f32((p0 >> 16u) & 0xFFu) / 255.0, f32((p1 >> 16u) & 0xFFu) / 255.0, frac);
    let amp = mix(f32((p0 >> 24u) & 0xFFu) / 255.0, f32((p1 >> 24u) & 0xFFu) / 255.0, frac);

    // Beat grid tick marks.
    // Downbeats: orange (CDJ-style), 2 columns wide.
    // Beats:     white, 1 column wide.
    if p.beat_period_cols > 0.0 {
        let rel = col_f - p.beat_anchor_col;

        // beat_pos in [0, beat_period_cols) — always positive
        let beat_pos = ((rel % p.beat_period_cols) + p.beat_period_cols) % p.beat_period_cols;

        // Fractional beat number (may be negative before anchor).
        let beat_num = floor(rel / p.beat_period_cols);

        // Bar-relative beat after applying the downbeat offset.
        let bpb      = p.beats_per_bar;
        let adjusted = ((beat_num + p.downbeat_offset) % bpb + bpb) % bpb;
        let is_downbeat = adjusted < 0.5;

        let tick_w = select(1.0, 2.0, is_downbeat);

        if beat_pos < tick_w || beat_pos > p.beat_period_cols - tick_w {
            if is_downbeat {
                // Red downbeat (beat 1) marker.
                return vec4<f32>(1.0, 0.15, 0.15, 1.0);
            } else {
                // White beat marker.
                return vec4<f32>(1.0, 1.0, 1.0, 0.85);
            }
        }
    }

    // Bar chart centered vertically, height = amp.
    let dist = abs(sy - 0.5) * 2.0;

    if dist < amp {
        let shade = 1.0 - dist / (amp + 0.001) * 0.3;
        return vec4<f32>(r * shade, g * shade, b * shade, 1.0);
    }

    // Dark background with subtle grid lines every 16 columns.
    let grid = col_f % 16.0;
    let bg   = select(0.04, 0.06, grid < 0.5);
    return vec4<f32>(bg, bg, bg, 1.0);
}
"#;
