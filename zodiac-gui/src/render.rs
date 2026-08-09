//! wgpu 30 + glyphon 0.12 renderer (roadmap 3.3, 3.5, 3.6), built from the
//! S3 spike prototype (docs/decisions/0004-gui-stack.md). One surface, one
//! render pass per frame: background cell quads (instanced rects) ->
//! under-text kitty image quads (z < 0) -> glyphon text (tab bar, grid or
//! agent transcript, status bar) -> over-text image quads -> overlay rects
//! (cursor, permission modal chrome) -> overlay text (modal body).
//!
//! Damage model: text is re-shaped only when a content signature changes
//! (per-buffer hash of cells/attrs or transcript state); rect and image
//! instance uploads are rebuilt every redraw (microseconds at grid scale).
//! Redraws happen on demand — server frames, input, resize — never on a
//! timer. The atlas is never trimmed per frame (S3: trim triples the cost).

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use glyphon::{
    Attrs, Buffer as TextBuffer, Cache, Color as TextColor, Family, FontSystem, Metrics,
    Resolution, Shaping, Style, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer,
    Viewport, Weight, Wrap,
};
use winit::window::Window;
use zodiac::client_core::{tool_compact, truncate, ARole, CPane};
use zodiac::protocol::SessionState;

use crate::anim::AnimStore;
use crate::font::{Fonts, FONT_PX};
use crate::palette::{self, CellStyle, ACCENT, CHROME_BG, CHROME_FG, DEFAULT_BG, DEFAULT_FG};

const RECT_WGSL: &str = r#"
struct Globals { res: vec2f, _pad: vec2f }
@group(0) @binding(0) var<uniform> globals: Globals;

struct VsOut { @builtin(position) pos: vec4f, @location(0) color: vec4f }

@vertex
fn vs(@builtin(vertex_index) vi: u32,
      @location(0) ipos: vec2f,
      @location(1) isize: vec2f,
      @location(2) icolor: vec4f) -> VsOut {
    let corner = vec2f(f32(vi & 1u), f32(vi >> 1u));
    let p = ipos + corner * isize;
    var out: VsOut;
    out.pos = vec4f(p.x / globals.res.x * 2.0 - 1.0,
                    1.0 - p.y / globals.res.y * 2.0, 0.0, 1.0);
    out.color = icolor;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4f { return in.color; }
"#;

const IMG_WGSL: &str = r#"
struct Globals { res: vec2f, _pad: vec2f }
@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;

struct VsOut { @builtin(position) pos: vec4f, @location(0) uv: vec2f }

@vertex
fn vs(@builtin(vertex_index) vi: u32,
      @location(0) rect: vec4f,
      @location(1) uvr: vec4f) -> VsOut {
    let corner = vec2f(f32(vi & 1u), f32(vi >> 1u));
    let p = rect.xy + corner * rect.zw;
    var out: VsOut;
    out.pos = vec4f(p.x / globals.res.x * 2.0 - 1.0,
                    1.0 - p.y / globals.res.y * 2.0, 0.0, 1.0);
    out.uv = uvr.xy + corner * uvr.zw;
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4f {
    return textureSample(tex, samp, in.uv);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct RectInst {
    pos: [f32; 2],
    size: [f32; 2],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct ImgInst {
    rect: [f32; 4],
    uv: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Globals {
    res: [f32; 2],
    _pad: [f32; 2],
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn rgba(c: [u8; 3], a: f32) -> [f32; 4] {
    [
        srgb_to_linear(c[0]),
        srgb_to_linear(c[1]),
        srgb_to_linear(c[2]),
        a,
    ]
}

/// One styled run inside a rich-text build.
#[derive(Clone, Copy, PartialEq)]
struct SpanAttr {
    color: [u8; 3],
    bold: bool,
    italic: bool,
}

/// Text + attribute spans destined for one glyphon buffer.
#[derive(Default)]
struct RichText {
    text: String,
    spans: Vec<(usize, usize, SpanAttr)>,
}

impl RichText {
    fn push(&mut self, s: &str, attr: SpanAttr) {
        let start = self.text.len();
        self.text.push_str(s);
        self.spans.push((start, self.text.len(), attr));
    }

    /// Extend the last span to cover trailing raw text (the row-`\n` trick:
    /// every byte of the string must be covered by a span or cosmic-text
    /// shapes the whole grid as one line — S3 lesson).
    fn push_raw(&mut self, s: &str) {
        let start = self.text.len();
        self.text.push_str(s);
        match self.spans.last_mut() {
            Some(last) if last.1 == start => last.1 = self.text.len(),
            _ => self.spans.push((
                start,
                self.text.len(),
                SpanAttr {
                    color: DEFAULT_FG,
                    bold: false,
                    italic: false,
                },
            )),
        }
    }
}

fn apply_rich(buf: &mut TextBuffer, fs: &mut FontSystem, family: &str, rt: &RichText) {
    let mut default_attrs = Attrs::new();
    default_attrs.family = Family::Name(family);
    let text = &rt.text;
    let spans = rt.spans.iter().map(|(a, b, s)| {
        let mut at = Attrs::new();
        at.family = Family::Name(family);
        at.color_opt = Some(TextColor::rgb(s.color[0], s.color[1], s.color[2]));
        if s.bold {
            at.weight = Weight::BOLD;
        }
        if s.italic {
            at.style = Style::Italic;
        }
        (&text[*a..*b], at)
    });
    buf.set_rich_text(spans, &default_attrs, Shaping::Advanced, None);
    buf.shape_until_scroll(fs, false);
}

struct ImgTex {
    bind: wgpu::BindGroup,
    w: u32,
    h: u32,
}

/// Texture cache key: (pane id, img id, ver, display frame — 0 = root).
type TexKey = (u64, u32, u32, u32);

/// What one redraw hands back to the app.
#[derive(Default)]
pub struct FrameOut {
    /// Tab bar hit ranges: (pane index, px range) for click-to-focus.
    pub tab_hits: Vec<(usize, Range<f32>)>,
    /// Draw-time clamp of the active agent pane's transcript scroll.
    pub agent_scroll: Option<usize>,
    /// Earliest wall-clock deadline at which a visible running animation
    /// flips frames (roadmap 4.2). None = nothing animates, no timer.
    pub next_anim: Option<Instant>,
}

pub struct Renderer {
    pub window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    // text
    fonts: Fonts,
    swash: SwashCache,
    atlas: TextAtlas,
    viewport: Viewport,
    text_main: TextRenderer,
    text_overlay: TextRenderer,
    grid_buf: TextBuffer,
    top_buf: TextBuffer,
    bottom_buf: TextBuffer,
    input_buf: TextBuffer,
    modal_buf: TextBuffer,
    // rect pipeline
    rect_pipeline: wgpu::RenderPipeline,
    rect_bind: wgpu::BindGroup,
    globals_buf: wgpu::Buffer,
    rect_buf: wgpu::Buffer,
    rect_cap: usize,
    // image pipeline (kitty blits)
    img_pipeline: wgpu::RenderPipeline,
    img_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    img_buf: wgpu::Buffer,
    img_cap: usize,
    /// Decoded frame textures; None caches a failed decode so it isn't
    /// retried every redraw.
    textures: HashMap<TexKey, Option<ImgTex>>,
    // metrics + damage signatures
    pub cell: (f32, f32),
    grid_sig: u64,
    top_sig: u64,
    bottom_sig: u64,
    input_sig: u64,
    modal_sig: u64,
}

impl Renderer {
    pub fn new(window: Arc<Window>, mut fonts: Fonts) -> anyhow::Result<Self> {
        let scale = window.scale_factor() as f32;
        let cell = fonts.cell_size(scale);
        let font_px = FONT_PX * scale;

        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone())?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
            compatible_surface: Some(&surface),
        }))
        .map_err(|e| anyhow::anyhow!("no wgpu adapter: {e:?}"))?;
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: None,
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                memory_hints: Default::default(),
                trace: Default::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
            }))?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| anyhow::anyhow!("surface has no default config"))?;
        config.present_mode = wgpu::PresentMode::AutoVsync;
        surface.configure(&device, &config);
        let format = config.format;

        // text infrastructure
        let swash = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_main =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let text_overlay =
            TextRenderer::new(&mut atlas, &device, wgpu::MultisampleState::default(), None);
        let metrics = Metrics::new(font_px, cell.1);
        let mkbuf = |wrap: Wrap, fs: &mut FontSystem| {
            let mut b = TextBuffer::new(fs, metrics);
            b.set_wrap(wrap);
            b
        };
        let grid_buf = mkbuf(Wrap::None, &mut fonts.system);
        let top_buf = mkbuf(Wrap::None, &mut fonts.system);
        let bottom_buf = mkbuf(Wrap::None, &mut fonts.system);
        let input_buf = mkbuf(Wrap::None, &mut fonts.system);
        let modal_buf = mkbuf(Wrap::Word, &mut fonts.system);

        // globals uniform
        let globals_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals = Globals {
            res: [config.width as f32, config.height as f32],
            _pad: [0.0; 2],
        };
        queue.write_buffer(&globals_buf, 0, bytemuck::bytes_of(&globals));

        // rect pipeline
        let rect_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rect"),
            source: wgpu::ShaderSource::Wgsl(RECT_WGSL.into()),
        });
        let rect_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let rect_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &rect_bgl,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buf.as_entire_binding(),
            }],
        });
        let rect_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&rect_bgl)],
            immediate_size: 0,
        });
        let rect_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("rect"),
            layout: Some(&rect_layout),
            vertex: wgpu::VertexState {
                module: &rect_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<RectInst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &rect_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let rect_cap = 8192;
        let rect_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rect instances"),
            size: (rect_cap * std::mem::size_of::<RectInst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        // image pipeline
        let img_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("img"),
            source: wgpu::ShaderSource::Wgsl(IMG_WGSL.into()),
        });
        let img_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let img_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&img_bgl)],
            immediate_size: 0,
        });
        let img_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("img"),
            layout: Some(&img_layout),
            vertex: wgpu::VertexState {
                module: &img_shader,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<ImgInst>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
                })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &img_shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: Default::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let img_cap = 64;
        let img_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("img instances"),
            size: (img_cap * std::mem::size_of::<ImgInst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            window,
            surface,
            device,
            queue,
            config,
            fonts,
            swash,
            atlas,
            viewport,
            text_main,
            text_overlay,
            grid_buf,
            top_buf,
            bottom_buf,
            input_buf,
            modal_buf,
            rect_pipeline,
            rect_bind,
            globals_buf,
            rect_buf,
            rect_cap,
            img_pipeline,
            img_bgl,
            sampler,
            img_buf,
            img_cap,
            textures: HashMap::new(),
            cell,
            grid_sig: 0,
            top_sig: 0,
            bottom_sig: 0,
            input_sig: 0,
            modal_sig: 0,
        })
    }

    pub fn resize(&mut self, w: u32, h: u32) {
        if w == 0 || h == 0 {
            return;
        }
        self.config.width = w;
        self.config.height = h;
        self.surface.configure(&self.device, &self.config);
        let globals = Globals {
            res: [w as f32, h as f32],
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.globals_buf, 0, bytemuck::bytes_of(&globals));
    }

    /// Re-measure the font at a new scale factor and reset the text
    /// buffers' metrics; every signature is invalidated.
    pub fn set_scale(&mut self, scale: f32) {
        self.cell = self.fonts.cell_size(scale);
        let metrics = Metrics::new(FONT_PX * scale, self.cell.1);
        for b in [
            &mut self.grid_buf,
            &mut self.top_buf,
            &mut self.bottom_buf,
            &mut self.input_buf,
            &mut self.modal_buf,
        ] {
            b.set_metrics(metrics);
        }
        self.grid_sig = 0;
        self.top_sig = 0;
        self.bottom_sig = 0;
        self.input_sig = 0;
        self.modal_sig = 0;
    }

    /// Pane grid size implied by the window: full frame minus the 1-line
    /// tab bar and 1-line status bar.
    pub fn grid_size(&self) -> (u16, u16) {
        let rows = (self.config.height as f32 / self.cell.1).floor() as i32 - 2;
        let cols = (self.config.width as f32 / self.cell.0).floor() as i32;
        (
            rows.clamp(2, u16::MAX as i32) as u16,
            cols.clamp(10, u16::MAX as i32) as u16,
        )
    }

    /// Decode + upload one image frame (display frame 0 = the root
    /// `T_GFX_IMG` data; 1.. = animation frames) unless already cached.
    #[allow(clippy::too_many_arguments)]
    fn ensure_texture(&mut self, key: TexKey, format: u8, zlib: bool, w: u32, h: u32, data: &[u8]) {
        if self.textures.contains_key(&key) {
            return;
        }
        let decoded = crate::img::decode_rgba(format, zlib, w, h, data);
        let entry = decoded.map(|(w, h, rgba)| {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pane img"),
                size: wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &tex,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &rgba,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(h),
                },
                wgpu::Extent3d {
                    width: w,
                    height: h,
                    depth_or_array_layers: 1,
                },
            );
            let view = tex.create_view(&Default::default());
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.img_bgl,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.globals_buf.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            ImgTex { bind, w, h }
        });
        self.textures.insert(key, entry);
    }

    /// Drop textures whose backing image is gone from its pane (or whose
    /// pane is gone) — mirrors the server-driven image lifetime. Frame
    /// textures additionally require their frame to still be in the store.
    fn evict_textures(&mut self, panes: &[CPane], anim: &AnimStore) {
        self.textures.retain(|(pid, img, ver, fidx), _| {
            let root_live = panes
                .iter()
                .find(|p| p.id == *pid)
                .and_then(|p| p.images.get(img))
                .is_some_and(|i| i.ver == *ver);
            root_live && (*fidx == 0 || anim.has_frame(*pid, *img, *ver, *fidx as usize - 1))
        });
    }

    pub fn render(
        &mut self,
        panes: &[CPane],
        active: usize,
        state: Option<&SessionState>,
        session: &str,
        anim: &mut AnimStore,
        now: Instant,
    ) -> FrameOut {
        let mut out = FrameOut::default();
        let (w, h) = (self.config.width as f32, self.config.height as f32);
        let (cw, ch) = self.cell;
        if w < cw * 4.0 || h < ch * 3.0 {
            return out;
        }
        let cols = (w / cw).floor() as usize;
        let grid_top = ch;
        let grid_bottom = h - ch;

        let mut rects_under: Vec<RectInst> = Vec::new();
        let mut rects_over: Vec<RectInst> = Vec::new();
        // (z, instance, texture key) split around the text pass.
        let mut imgs_under: Vec<(i32, ImgInst, TexKey)> = Vec::new();
        let mut imgs_over: Vec<(i32, ImgInst, TexKey)> = Vec::new();

        // ---------------------------------------------------------- chrome
        rects_under.push(RectInst {
            pos: [0.0, 0.0],
            size: [w, ch],
            color: rgba(CHROME_BG, 1.0),
        });
        rects_under.push(RectInst {
            pos: [0.0, grid_bottom],
            size: [w, ch],
            color: rgba(CHROME_BG, 1.0),
        });

        // Tab bar: " ● name " per pane, active highlighted. Hit ranges are
        // estimated at one cell per char — exact for monospace glyphs.
        let mut top_rt = RichText::default();
        let mut x = 0.0f32;
        let mut top_sig_h = DefaultHasher::new();
        for (i, p) in panes.iter().enumerate() {
            let status = state
                .and_then(|s| s.panes.iter().find(|sp| sp.id == p.id))
                .map(|sp| sp.status.as_str())
                .unwrap_or("idle");
            let dot = match status {
                "working" => [240, 180, 60],
                "needs_input" => [240, 90, 90],
                "done" => [120, 220, 120],
                _ => [110, 110, 125],
            };
            let name: String = p.name.chars().filter(|c| *c != '\n').take(24).collect();
            let chars = 2 + 1 + name.chars().count() + 1;
            let width = chars as f32 * cw;
            if i == active {
                rects_under.push(RectInst {
                    pos: [x, 0.0],
                    size: [width, ch],
                    color: rgba([45, 45, 62], 1.0),
                });
            }
            let fg = if i == active { DEFAULT_FG } else { CHROME_FG };
            top_rt.push(
                " ●",
                SpanAttr {
                    color: dot,
                    bold: false,
                    italic: false,
                },
            );
            top_rt.push(
                &format!(" {name} "),
                SpanAttr {
                    color: fg,
                    bold: i == active,
                    italic: false,
                },
            );
            out.tab_hits.push((i, x..x + width));
            x += width;
            (i == active, status, &name).hash(&mut top_sig_h);
        }
        cols.hash(&mut top_sig_h);
        let top_sig = top_sig_h.finish();
        if top_sig != self.top_sig {
            self.top_sig = top_sig;
            apply_rich(
                &mut self.top_buf,
                &mut self.fonts.system,
                &self.fonts.family,
                &top_rt,
            );
        }

        // Status bar: session + active pane title left, grid size right.
        let (grid_rows, grid_cols) = self.grid_size();
        let bottom_str = {
            let (name, title) = panes
                .get(active)
                .map(|p| {
                    let t = if p.is_agent() {
                        p.kind.clone()
                    } else {
                        p.parser.screen().title().to_string()
                    };
                    (p.name.clone(), t)
                })
                .unwrap_or_default();
            let left = if title.is_empty() {
                format!(" {session} · {name}")
            } else {
                format!(" {session} · {name} — {}", truncate(&title, 60))
            };
            let right = format!("{grid_cols}x{grid_rows} ");
            let pad = cols
                .saturating_sub(left.chars().count())
                .saturating_sub(right.chars().count());
            format!("{left}{}{right}", " ".repeat(pad))
        };
        let bottom_sig = {
            let mut hh = DefaultHasher::new();
            bottom_str.hash(&mut hh);
            hh.finish()
        };
        if bottom_sig != self.bottom_sig {
            self.bottom_sig = bottom_sig;
            let mut rt = RichText::default();
            rt.push(
                &bottom_str,
                SpanAttr {
                    color: CHROME_FG,
                    bold: false,
                    italic: false,
                },
            );
            apply_rich(
                &mut self.bottom_buf,
                &mut self.fonts.system,
                &self.fonts.family,
                &rt,
            );
        }

        // ------------------------------------------------------- main pane
        let active_pane = panes.get(active);
        let is_agent = active_pane.is_some_and(|p| p.is_agent());
        let mut grid_text_area = false;
        let mut input_area_top: Option<f32> = None;
        let mut transcript_top = grid_top;
        let mut modal: Option<(f32, f32, f32, f32)> = None; // x, y, w, h

        if let Some(p) = active_pane {
            if is_agent {
                self.build_agent(
                    p,
                    w,
                    grid_top,
                    grid_bottom,
                    &mut rects_under,
                    &mut rects_over,
                    &mut transcript_top,
                    &mut input_area_top,
                    &mut modal,
                    &mut out,
                );
            } else {
                self.build_grid(p, grid_top, &mut rects_under, &mut rects_over);
                grid_text_area = true;
                // kitty placements for the active pty pane
                let scroll = p.scroll as i32;
                // Unicode placeholders (4.3): collect the pane's U+10EEEE
                // cells once — (fg-encoded image id if any, row, col) in
                // the *displayed* screen (scrollback view included), so
                // tiles land exactly where their cells are drawn.
                let virt_count = p.gfx.placements.iter().filter(|v| v.virt).count();
                let mut ph_cells: Vec<(Option<u32>, u16, u16)> = Vec::new();
                if virt_count > 0 {
                    let screen = p.parser.screen();
                    let (rows, cols) = screen.size();
                    for row in 0..rows {
                        for col in 0..cols {
                            let Some(cell) = screen.cell(row, col) else {
                                continue;
                            };
                            if !cell.contents().starts_with(crate::placeholder::PLACEHOLDER) {
                                continue;
                            }
                            let id = match cell.fgcolor() {
                                vt100::Color::Rgb(r, g, b) => {
                                    Some(((r as u32) << 16) | ((g as u32) << 8) | b as u32)
                                }
                                vt100::Color::Idx(i) => Some(i as u32),
                                vt100::Color::Default => None,
                            };
                            ph_cells.push((id, row, col));
                        }
                    }
                }
                for vp in &p.gfx.placements {
                    let Some(img) = p.images.get(&vp.img) else {
                        continue;
                    };
                    if img.ver != vp.img_ver {
                        continue;
                    }
                    // Animation playhead (4.2): pick the display frame and
                    // remember the earliest next flip. Stopped/frameless
                    // images show display frame 0 (the root data).
                    let (fidx, deadline) = match p.gfx.anim.iter().find(|a| a.img == vp.img) {
                        Some(sa) => anim.playhead(p.id, vp.img, img.ver, sa, now),
                        None => (0, None),
                    };
                    if let Some(d) = deadline {
                        out.next_anim = Some(out.next_anim.map_or(d, |x| x.min(d)));
                    }
                    let mut key = (p.id, vp.img, img.ver, fidx as u32);
                    if fidx == 0 {
                        self.ensure_texture(key, img.format, img.zlib, img.w, img.h, &img.data);
                    } else if let Some(f) = anim.frame(p.id, vp.img, img.ver, fidx - 1) {
                        self.ensure_texture(key, f.format, f.zlib, f.w, f.h, &f.data);
                    }
                    if !matches!(self.textures.get(&key), Some(Some(_))) {
                        // Frame missing or failed to decode: root fallback.
                        key = (p.id, vp.img, img.ver, 0);
                        self.ensure_texture(key, img.format, img.zlib, img.w, img.h, &img.data);
                    }
                    let Some(Some(tex)) = self.textures.get(&key) else {
                        continue;
                    };
                    let (sx, sy, sw0, sh0) = vp.src;
                    let sw = if sw0 > 0 {
                        sw0
                    } else {
                        tex.w.saturating_sub(sx)
                    };
                    let sh = if sh0 > 0 {
                        sh0
                    } else {
                        tex.h.saturating_sub(sy)
                    };
                    if sw == 0 || sh == 0 {
                        continue;
                    }
                    let layer = if vp.z < 0 {
                        &mut imgs_under
                    } else {
                        &mut imgs_over
                    };
                    if vp.virt {
                        // 4.3: blit source tiles at the placeholder cells.
                        // Cells whose fg carries an id must match this
                        // image's (lower 24 bits); default-fg cells count
                        // only when this is the sole virt placement.
                        let cells: Vec<(u16, u16)> = ph_cells
                            .iter()
                            .filter(|(id, ..)| match id {
                                Some(id) => *id == (vp.img & 0xFF_FFFF),
                                None => virt_count == 1,
                            })
                            .map(|(_, r, c)| (*r, *c))
                            .collect();
                        let Some((tc, tr, runs)) = crate::placeholder::runs(&cells) else {
                            continue;
                        };
                        let (tcw, trh) = (sw as f32 / tc as f32, sh as f32 / tr as f32);
                        for r in runs {
                            let inst = ImgInst {
                                rect: [
                                    r.col as f32 * cw,
                                    grid_top + r.row as f32 * ch,
                                    r.len as f32 * cw,
                                    ch,
                                ],
                                uv: [
                                    (sx as f32 + r.tile_col as f32 * tcw) / tex.w as f32,
                                    (sy as f32 + r.tile_row as f32 * trh) / tex.h as f32,
                                    r.len as f32 * tcw / tex.w as f32,
                                    trh / tex.h as f32,
                                ],
                            };
                            layer.push((vp.z, inst, key));
                        }
                        continue;
                    }
                    if vp.cols == 0 || vp.rows == 0 {
                        continue;
                    }
                    let inst = ImgInst {
                        rect: [
                            vp.col as f32 * cw + vp.offx as f32,
                            grid_top + (vp.row + scroll) as f32 * ch + vp.offy as f32,
                            vp.cols as f32 * cw,
                            vp.rows as f32 * ch,
                        ],
                        uv: [
                            sx as f32 / tex.w as f32,
                            sy as f32 / tex.h as f32,
                            sw as f32 / tex.w as f32,
                            sh as f32 / tex.h as f32,
                        ],
                    };
                    layer.push((vp.z, inst, key));
                }
                imgs_under.sort_by_key(|(z, ..)| *z);
                imgs_over.sort_by_key(|(z, ..)| *z);
            }
        }
        self.evict_textures(panes, anim);

        // ---------------------------------------------------- GPU uploads
        let under_n = rects_under.len();
        let total_rects = under_n + rects_over.len();
        if total_rects > self.rect_cap {
            self.rect_cap = total_rects.next_power_of_two();
            self.rect_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("rect instances"),
                size: (self.rect_cap * std::mem::size_of::<RectInst>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        let mut all_rects = rects_under;
        all_rects.extend_from_slice(&rects_over);
        if !all_rects.is_empty() {
            self.queue
                .write_buffer(&self.rect_buf, 0, bytemuck::cast_slice(&all_rects));
        }
        let img_n = imgs_under.len() + imgs_over.len();
        if img_n > self.img_cap {
            self.img_cap = img_n.next_power_of_two();
            self.img_buf = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("img instances"),
                size: (self.img_cap * std::mem::size_of::<ImgInst>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if img_n > 0 {
            let insts: Vec<ImgInst> = imgs_under
                .iter()
                .chain(imgs_over.iter())
                .map(|(_, i, _)| *i)
                .collect();
            self.queue
                .write_buffer(&self.img_buf, 0, bytemuck::cast_slice(&insts));
        }

        // ----------------------------------------------------- text prepare
        self.viewport.update(
            &self.queue,
            Resolution {
                width: self.config.width,
                height: self.config.height,
            },
        );
        let full = TextBounds {
            left: 0,
            top: 0,
            right: self.config.width as i32,
            bottom: self.config.height as i32,
        };
        let grid_bounds = TextBounds {
            left: 0,
            top: grid_top as i32,
            right: self.config.width as i32,
            bottom: input_area_top.unwrap_or(grid_bottom) as i32,
        };
        let dc = TextColor::rgb(DEFAULT_FG[0], DEFAULT_FG[1], DEFAULT_FG[2]);
        macro_rules! main_areas {
            () => {{
                let mut areas = vec![
                    TextArea {
                        buffer: &self.top_buf,
                        left: 0.0,
                        top: 0.0,
                        scale: 1.0,
                        bounds: full,
                        default_color: dc,
                        custom_glyphs: &[],
                    },
                    TextArea {
                        buffer: &self.bottom_buf,
                        left: 0.0,
                        top: grid_bottom,
                        scale: 1.0,
                        bounds: full,
                        default_color: dc,
                        custom_glyphs: &[],
                    },
                ];
                if grid_text_area || is_agent {
                    areas.push(TextArea {
                        buffer: &self.grid_buf,
                        left: 0.0,
                        top: transcript_top,
                        scale: 1.0,
                        bounds: grid_bounds,
                        default_color: dc,
                        custom_glyphs: &[],
                    });
                }
                if let Some(top) = input_area_top {
                    areas.push(TextArea {
                        buffer: &self.input_buf,
                        left: 0.0,
                        top,
                        scale: 1.0,
                        bounds: full,
                        default_color: dc,
                        custom_glyphs: &[],
                    });
                }
                areas
            }};
        }
        if self
            .text_main
            .prepare(
                &self.device,
                &self.queue,
                &mut self.fonts.system,
                &mut self.atlas,
                &self.viewport,
                main_areas!(),
                &mut self.swash,
            )
            .is_err()
        {
            // AtlasFull: trim once and retry (S3 policy — never per frame).
            self.atlas.trim();
            let _ = self.text_main.prepare(
                &self.device,
                &self.queue,
                &mut self.fonts.system,
                &mut self.atlas,
                &self.viewport,
                main_areas!(),
                &mut self.swash,
            );
        }
        let overlay_areas: Vec<TextArea> = match modal {
            Some((mx, my, _, _)) => vec![TextArea {
                buffer: &self.modal_buf,
                left: mx + cw,
                top: my + ch * 0.75,
                scale: 1.0,
                bounds: full,
                default_color: dc,
                custom_glyphs: &[],
            }],
            None => Vec::new(),
        };
        let _ = self.text_overlay.prepare(
            &self.device,
            &self.queue,
            &mut self.fonts.system,
            &mut self.atlas,
            &self.viewport,
            overlay_areas,
            &mut self.swash,
        );

        // ------------------------------------------------------------ pass
        use wgpu::CurrentSurfaceTexture as CST;
        let tex = match self.surface.get_current_texture() {
            CST::Success(t) | CST::Suboptimal(t) => t,
            _ => {
                // Lost/outdated surface: reconfigure, redraw on next request.
                self.surface.configure(&self.device, &self.config);
                self.window.request_redraw();
                return out;
            }
        };
        let view = tex.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let bg = rgba(DEFAULT_BG, 1.0);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if under_n > 0 {
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_bind_group(0, &self.rect_bind, &[]);
                pass.set_vertex_buffer(0, self.rect_buf.slice(..));
                pass.draw(0..4, 0..under_n as u32);
            }
            // Kitty images are clipped to the grid area.
            let scissor = (
                0u32,
                grid_top as u32,
                self.config.width,
                (grid_bottom - grid_top).max(1.0) as u32,
            );
            if !imgs_under.is_empty() {
                pass.set_scissor_rect(scissor.0, scissor.1, scissor.2, scissor.3);
                pass.set_pipeline(&self.img_pipeline);
                pass.set_vertex_buffer(0, self.img_buf.slice(..));
                for (i, (_, _, key)) in imgs_under.iter().enumerate() {
                    if let Some(Some(t)) = self.textures.get(key) {
                        pass.set_bind_group(0, &t.bind, &[]);
                        pass.draw(0..4, i as u32..i as u32 + 1);
                    }
                }
                pass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            }
            let _ = self
                .text_main
                .render(&self.atlas, &self.viewport, &mut pass);
            if !imgs_over.is_empty() {
                pass.set_scissor_rect(scissor.0, scissor.1, scissor.2, scissor.3);
                pass.set_pipeline(&self.img_pipeline);
                pass.set_vertex_buffer(0, self.img_buf.slice(..));
                let base = imgs_under.len() as u32;
                for (i, (_, _, key)) in imgs_over.iter().enumerate() {
                    if let Some(Some(t)) = self.textures.get(key) {
                        pass.set_bind_group(0, &t.bind, &[]);
                        let idx = base + i as u32;
                        pass.draw(0..4, idx..idx + 1);
                    }
                }
                pass.set_scissor_rect(0, 0, self.config.width, self.config.height);
            }
            if !rects_over.is_empty() {
                pass.set_pipeline(&self.rect_pipeline);
                pass.set_bind_group(0, &self.rect_bind, &[]);
                pass.set_vertex_buffer(0, self.rect_buf.slice(..));
                pass.draw(0..4, under_n as u32..total_rects as u32);
            }
            if modal.is_some() {
                let _ = self
                    .text_overlay
                    .render(&self.atlas, &self.viewport, &mut pass);
            }
        }
        self.queue.submit([encoder.finish()]);
        self.queue.present(tex);
        out
    }

    /// Grid path (3.3): per-cell bg quads + underline hairlines + cursor
    /// block from the pane's vt100 screen; text re-shaped only when the
    /// cell signature changes.
    fn build_grid(
        &mut self,
        p: &CPane,
        grid_top: f32,
        rects_under: &mut Vec<RectInst>,
        rects_over: &mut Vec<RectInst>,
    ) {
        let (cw, ch) = self.cell;
        let screen = p.parser.screen();
        let (rows, cols) = screen.size();
        let mut sig = DefaultHasher::new();
        (rows, cols, p.id, p.scroll).hash(&mut sig);

        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                let contents = cell.contents();
                let style = CellStyle {
                    fg: cell.fgcolor(),
                    bg: cell.bgcolor(),
                    bold: cell.bold(),
                    dim: cell.dim(),
                    inverse: cell.inverse(),
                };
                let (fg, bg) = palette::cell_colors(&style);
                // signature: contents + everything that affects glyphs
                contents.hash(&mut sig);
                (fg, cell.italic(), cell.underline(), style.bold).hash(&mut sig);
                let x = col as f32 * cw;
                let y = grid_top + row as f32 * ch;
                if let Some(bg) = bg {
                    rects_under.push(RectInst {
                        pos: [x, y],
                        size: [cw + 0.5, ch + 0.5],
                        color: rgba(bg, 1.0),
                    });
                }
                if cell.underline() {
                    rects_under.push(RectInst {
                        pos: [x, y + ch - 1.5],
                        size: [cw, 1.0],
                        color: rgba(fg, 1.0),
                    });
                }
            }
        }
        // Block cursor: translucent overlay quad above the glyphs.
        if !screen.hide_cursor() && p.scroll == 0 {
            let (r, c) = screen.cursor_position();
            if r < rows && c < cols {
                rects_over.push(RectInst {
                    pos: [c as f32 * cw, grid_top + r as f32 * ch],
                    size: [cw, ch],
                    color: [0.9, 0.9, 0.9, 0.35],
                });
            }
        }
        let sig = sig.finish();
        if sig == self.grid_sig {
            return;
        }
        self.grid_sig = sig;

        let mut rt = RichText::default();
        rt.text.reserve(rows as usize * (cols as usize + 1));
        for row in 0..rows {
            for col in 0..cols {
                let Some(cell) = screen.cell(row, col) else {
                    continue;
                };
                if cell.is_wide_continuation() {
                    continue; // the wide glyph before it owns the advance
                }
                let contents = cell.contents();
                let style = CellStyle {
                    fg: cell.fgcolor(),
                    bg: cell.bgcolor(),
                    bold: cell.bold(),
                    dim: cell.dim(),
                    inverse: cell.inverse(),
                };
                let (fg, _) = palette::cell_colors(&style);
                let attr = SpanAttr {
                    color: fg,
                    bold: cell.bold(),
                    italic: cell.italic(),
                };
                if contents.is_empty() || contents.starts_with(crate::placeholder::PLACEHOLDER) {
                    // Placeholder cells (4.3) shape as blanks — the image
                    // tile is blitted over them, never a tofu glyph.
                    rt.push(" ", attr);
                } else {
                    rt.push(&contents, attr);
                }
            }
            rt.push_raw("\n");
        }
        self.grid_buf.set_wrap(Wrap::None);
        self.grid_buf.set_size(None, None);
        apply_rich(
            &mut self.grid_buf,
            &mut self.fonts.system,
            &self.fonts.family,
            &rt,
        );
    }

    /// Agent path (2.x panes on the GUI): transcript with role glyphs, a
    /// one-line local prompt editor, and the permission modal.
    #[allow(clippy::too_many_arguments)]
    fn build_agent(
        &mut self,
        p: &CPane,
        w: f32,
        grid_top: f32,
        grid_bottom: f32,
        rects_under: &mut Vec<RectInst>,
        rects_over: &mut Vec<RectInst>,
        transcript_top: &mut f32,
        input_area_top: &mut Option<f32>,
        modal: &mut Option<(f32, f32, f32, f32)>,
        out: &mut FrameOut,
    ) {
        let (cw, ch) = self.cell;
        let input_top = grid_bottom - ch;
        *input_area_top = Some(input_top);
        // Prompt row backdrop.
        rects_under.push(RectInst {
            pos: [0.0, input_top],
            size: [w, ch],
            color: rgba([22, 22, 30], 1.0),
        });

        const ROLE: [(ARole, &str, [u8; 3]); 4] = [
            (ARole::User, "❯ ", [130, 170, 255]),
            (ARole::Assistant, "✦ ", [190, 150, 255]),
            (ARole::Tool, "⏺ ", [120, 220, 120]),
            (ARole::Error, "✗ ", [240, 90, 90]),
        ];
        let a = &p.agent;
        let mut sig = DefaultHasher::new();
        (p.id, a.log.len(), &a.stream, a.thinking, a.scroll).hash(&mut sig);
        (w as u32, ch as u32).hash(&mut sig);
        let sig = sig.finish();
        if sig != self.grid_sig {
            self.grid_sig = sig;
            let mut rt = RichText::default();
            let skip = a.log.len().saturating_sub(400);
            for (role, text) in a.log.iter().skip(skip) {
                let (_, glyph, color) = ROLE
                    .iter()
                    .find(|(r, ..)| r == role)
                    .map(|(r, g, c)| (*r, *g, *c))
                    .unwrap_or((ARole::Tool, "⏺ ", [120, 220, 120]));
                rt.push(
                    glyph,
                    SpanAttr {
                        color,
                        bold: true,
                        italic: false,
                    },
                );
                let dimmed = *role == ARole::Tool;
                rt.push(
                    text,
                    SpanAttr {
                        color: if dimmed { [150, 150, 160] } else { DEFAULT_FG },
                        bold: false,
                        italic: false,
                    },
                );
                rt.push_raw("\n");
            }
            if a.thinking && a.stream.is_empty() {
                rt.push(
                    "✦ …",
                    SpanAttr {
                        color: [120, 120, 135],
                        bold: false,
                        italic: true,
                    },
                );
                rt.push_raw("\n");
            } else if !a.stream.is_empty() {
                rt.push(
                    "✦ ",
                    SpanAttr {
                        color: [190, 150, 255],
                        bold: true,
                        italic: false,
                    },
                );
                rt.push(
                    &a.stream,
                    SpanAttr {
                        color: DEFAULT_FG,
                        bold: false,
                        italic: false,
                    },
                );
                rt.push(
                    "▌",
                    SpanAttr {
                        color: ACCENT,
                        bold: false,
                        italic: false,
                    },
                );
                rt.push_raw("\n");
            }
            if rt.text.is_empty() {
                rt.push(
                    "agent pane — type below, Enter sends",
                    SpanAttr {
                        color: [110, 110, 125],
                        bold: false,
                        italic: true,
                    },
                );
            }
            self.grid_buf.set_wrap(Wrap::Word);
            self.grid_buf.set_size(Some(w - cw), None);
            // Proportional shaping for transcript prose (roadmap 4.5):
            // grid panes stay monospace; agent transcripts read as text.
            apply_rich(
                &mut self.grid_buf,
                &mut self.fonts.system,
                &self.fonts.ui_family,
                &rt,
            );
        }
        // Bottom-anchor the transcript; clamp the wheel offset to content.
        let total_lines = self.grid_buf.layout_runs().count();
        let total_h = total_lines as f32 * ch;
        let avail = input_top - grid_top;
        let max_scroll = (((total_h - avail) / ch).ceil().max(0.0)) as usize;
        let scroll = a.scroll.min(max_scroll);
        out.agent_scroll = Some(scroll);
        *transcript_top = if total_h <= avail {
            grid_top
        } else {
            input_top - total_h + scroll as f32 * ch
        };

        // Prompt editor row.
        let input_sig = {
            let mut hh = DefaultHasher::new();
            (&a.input, a.cursor).hash(&mut hh);
            hh.finish()
        };
        if input_sig != self.input_sig {
            self.input_sig = input_sig;
            let mut rt = RichText::default();
            rt.push(
                "❯ ",
                SpanAttr {
                    color: ACCENT,
                    bold: true,
                    italic: false,
                },
            );
            rt.push(
                &a.input,
                SpanAttr {
                    color: DEFAULT_FG,
                    bold: false,
                    italic: false,
                },
            );
            self.input_buf.set_wrap(Wrap::None);
            self.input_buf.set_size(None, None);
            apply_rich(
                &mut self.input_buf,
                &mut self.fonts.system,
                &self.fonts.family,
                &rt,
            );
        }
        rects_over.push(RectInst {
            pos: [(2 + a.cursor) as f32 * cw, input_top],
            size: [cw, ch],
            color: [0.9, 0.9, 0.9, 0.35],
        });

        // Permission modal (first pending request).
        if let Some(pr) = a.perms.first() {
            let box_w = (w - 8.0 * cw).min(72.0 * cw).max(24.0 * cw);
            let modal_sig = {
                let mut hh = DefaultHasher::new();
                (&pr.request_id, box_w as u32).hash(&mut hh);
                hh.finish()
            };
            if modal_sig != self.modal_sig {
                self.modal_sig = modal_sig;
                let mut rt = RichText::default();
                rt.push(
                    "permission request\n",
                    SpanAttr {
                        color: [240, 180, 60],
                        bold: true,
                        italic: false,
                    },
                );
                let tool = pr
                    .display_name
                    .clone()
                    .unwrap_or_else(|| pr.tool_name.clone());
                rt.push(
                    &format!("{tool}({})\n", tool_compact(&pr.input)),
                    SpanAttr {
                        color: DEFAULT_FG,
                        bold: false,
                        italic: false,
                    },
                );
                rt.push(
                    "[y] allow    [n] deny",
                    SpanAttr {
                        color: CHROME_FG,
                        bold: false,
                        italic: false,
                    },
                );
                self.modal_buf.set_wrap(Wrap::Word);
                self.modal_buf.set_size(Some(box_w - 2.0 * cw), None);
                apply_rich(
                    &mut self.modal_buf,
                    &mut self.fonts.system,
                    &self.fonts.family,
                    &rt,
                );
            }
            let lines = self.modal_buf.layout_runs().count().max(3);
            let box_h = lines as f32 * ch + 1.5 * ch;
            let mx = (w - box_w) / 2.0;
            let my = (grid_top + (grid_bottom - grid_top - box_h) / 2.0).max(grid_top);
            rects_over.push(RectInst {
                pos: [mx - 2.0, my - 2.0],
                size: [box_w + 4.0, box_h + 4.0],
                color: rgba(ACCENT, 1.0),
            });
            rects_over.push(RectInst {
                pos: [mx, my],
                size: [box_w, box_h],
                color: rgba([30, 30, 44], 1.0),
            });
            *modal = Some((mx, my, box_w, box_h));
        } else {
            self.modal_sig = 0;
        }
    }
}
