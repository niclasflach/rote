use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use glyphon::{Cache, FontSystem, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport};
use wgpu::util::{BufferInitDescriptor, DeviceExt};
use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindGroupLayoutDescriptor, BindGroupLayoutEntry,
    BindingType, BlendState, BufferBindingType, BufferUsages, ColorTargetState, ColorWrites,
    CommandEncoderDescriptor, CompositeAlphaMode, CurrentSurfaceTexture, DeviceDescriptor, FragmentState,
    Instance, InstanceDescriptor, LoadOp, MultisampleState, Operations, PipelineLayoutDescriptor,
    PresentMode, PrimitiveState, RenderPassColorAttachment, RenderPassDescriptor, RenderPipeline,
    RenderPipelineDescriptor, RequestAdapterOptions, ShaderModuleDescriptor, ShaderSource, ShaderStages,
    SurfaceColorSpace, SurfaceConfiguration, TextureFormat, TextureUsages, TextureViewDescriptor,
    VertexAttribute, VertexBufferLayout, VertexFormat, VertexState, VertexStepMode,
};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

const QUAD_SHADER: &str = r#"
struct Viewport {
    size: vec2<f32>,
};
@group(0) @binding(0) var<uniform> viewport: Viewport;

struct VertexInput {
    @location(0) position: vec2<f32>,
    @location(1) color: vec4<f32>,
};
struct VertexOutput {
    @builtin(position) clip_position: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(in: VertexInput) -> VertexOutput {
    var out: VertexOutput;
    let ndc_x = (in.position.x / viewport.size.x) * 2.0 - 1.0;
    let ndc_y = 1.0 - (in.position.y / viewport.size.y) * 2.0;
    out.clip_position = vec4<f32>(ndc_x, ndc_y, 0.0, 1.0);
    out.color = in.color;
    return out;
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    return in.color;
}
"#;

#[repr(C)]
#[derive(Copy, Clone, Pod, Zeroable)]
struct QuadVertex {
    position: [f32; 2],
    color: [f32; 4],
}

/// A solid-color screen-space rectangle — used to draw the caret and
/// selection highlight, which glyphon has no primitive for since it only
/// rasterizes glyphs.
pub struct RectDraw {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: [f32; 4],
}

fn quad_vertices(rects: &[RectDraw]) -> Vec<QuadVertex> {
    let mut vertices = Vec::with_capacity(rects.len() * 6);
    for rect in rects {
        let (x0, y0) = (rect.x, rect.y);
        let (x1, y1) = (rect.x + rect.width, rect.y + rect.height);
        let c = rect.color;
        let corners = [
            [x0, y0],
            [x1, y0],
            [x1, y1],
            [x0, y0],
            [x1, y1],
            [x0, y1],
        ];
        vertices.extend(corners.map(|position| QuadVertex { position, color: c }));
    }
    vertices
}

/// Owns the GPU surface and the glyphon text pipeline for one window.
/// One `Renderer` per window; `rote-app` drives it from the winit event
/// loop and hands it whatever [`glyphon::Buffer`]s need to be drawn each
/// frame.
pub struct Renderer {
    instance: Instance,
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_config: SurfaceConfiguration,

    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,

    quad_pipeline: RenderPipeline,
    quad_bind_group: BindGroup,
    quad_viewport_buffer: wgpu::Buffer,

    window: Arc<Window>,
}

impl Renderer {
    pub async fn new(window: Arc<Window>, event_loop: &ActiveEventLoop) -> anyhow::Result<Self> {
        let size = window.inner_size();

        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let adapter = instance
            .request_adapter(&RequestAdapterOptions::default())
            .await
            .map_err(|e| anyhow::anyhow!("no suitable GPU adapter: {e}"))?;
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await?;

        let surface = instance.create_surface(window.clone())?;
        let format = TextureFormat::Bgra8UnormSrgb;
        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode: CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &surface_config);

        let font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, format);
        let text_renderer = TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        let (quad_pipeline, quad_bind_group, quad_viewport_buffer) = create_quad_pipeline(&device, format);

        Ok(Self {
            instance,
            device,
            queue,
            surface,
            surface_config,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            quad_pipeline,
            quad_bind_group,
            quad_viewport_buffer,
            window,
        })
    }

    pub fn font_system(&mut self) -> &mut FontSystem {
        &mut self.font_system
    }

    pub fn size(&self) -> (u32, u32) {
        (self.surface_config.width, self.surface_config.height)
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    /// Shape-and-draw one or more text buffers, each positioned at its own
    /// `(left, top)` origin, with `rects` (the caret, selection highlight)
    /// drawn underneath them. `clear_color` is the background for the whole
    /// surface.
    pub fn render(
        &mut self,
        rects: &[RectDraw],
        areas: Vec<TextDraw<'_>>,
        clear_color: wgpu::Color,
    ) -> anyhow::Result<()> {
        self.viewport.update(
            &self.queue,
            glyphon::Resolution {
                width: self.surface_config.width,
                height: self.surface_config.height,
            },
        );

        self.queue.write_buffer(
            &self.quad_viewport_buffer,
            0,
            bytemuck::cast_slice(&[self.surface_config.width as f32, self.surface_config.height as f32]),
        );
        let quad_vertices = quad_vertices(rects);
        let quad_vertex_buffer = (!quad_vertices.is_empty()).then(|| {
            self.device.create_buffer_init(&BufferInitDescriptor {
                label: Some("rote-quad-vertices"),
                contents: bytemuck::cast_slice(&quad_vertices),
                usage: BufferUsages::VERTEX,
            })
        });

        let text_areas = areas.into_iter().map(|draw| TextArea {
            buffer: draw.buffer,
            left: draw.left,
            top: draw.top,
            scale: 1.0,
            bounds: TextBounds {
                left: 0,
                top: 0,
                right: self.surface_config.width as i32,
                bottom: self.surface_config.height as i32,
            },
            default_color: draw.color,
            custom_glyphs: &[],
        });

        self.text_renderer.prepare(
            &self.device,
            &self.queue,
            &mut self.font_system,
            &mut self.atlas,
            &self.viewport,
            text_areas,
            &mut self.swash_cache,
        )?;

        let frame = match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(frame) => frame,
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => {
                self.window.request_redraw();
                return Ok(());
            }
            CurrentSurfaceTexture::Outdated | CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return Ok(());
            }
            CurrentSurfaceTexture::Lost => {
                self.surface = self.instance.create_surface(self.window.clone())?;
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return Ok(());
            }
            CurrentSurfaceTexture::Validation => anyhow::bail!("surface validation error"),
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&CommandEncoderDescriptor { label: None });
        {
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(clear_color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let Some(vbuf) = &quad_vertex_buffer {
                pass.set_pipeline(&self.quad_pipeline);
                pass.set_bind_group(0, &self.quad_bind_group, &[]);
                pass.set_vertex_buffer(0, vbuf.slice(..));
                pass.draw(0..quad_vertices.len() as u32, 0..1);
            }
            self.text_renderer.render(&self.atlas, &self.viewport, &mut pass)?;
        }

        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();

        Ok(())
    }
}

/// One piece of laid-out text to draw this frame, at a screen-space origin.
pub struct TextDraw<'a> {
    pub buffer: &'a glyphon::Buffer,
    pub left: f32,
    pub top: f32,
    pub color: glyphon::Color,
}

/// Builds the pipeline that draws [`RectDraw`] quads (caret, selection
/// highlight) as flat-colored triangles, positioned in pixel space via a
/// tiny viewport-size uniform rather than a full projection matrix.
fn create_quad_pipeline(
    device: &wgpu::Device,
    format: TextureFormat,
) -> (RenderPipeline, BindGroup, wgpu::Buffer) {
    let shader = device.create_shader_module(ShaderModuleDescriptor {
        label: Some("rote-quad-shader"),
        source: ShaderSource::Wgsl(QUAD_SHADER.into()),
    });

    let bind_group_layout = device.create_bind_group_layout(&BindGroupLayoutDescriptor {
        label: Some("rote-quad-bind-group-layout"),
        entries: &[BindGroupLayoutEntry {
            binding: 0,
            visibility: ShaderStages::VERTEX,
            ty: BindingType::Buffer {
                ty: BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }],
    });

    let viewport_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rote-quad-viewport"),
        size: 8, // vec2<f32>
        usage: BufferUsages::UNIFORM | BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let bind_group = device.create_bind_group(&BindGroupDescriptor {
        label: Some("rote-quad-bind-group"),
        layout: &bind_group_layout,
        entries: &[BindGroupEntry {
            binding: 0,
            resource: viewport_buffer.as_entire_binding(),
        }],
    });

    let pipeline_layout = device.create_pipeline_layout(&PipelineLayoutDescriptor {
        label: Some("rote-quad-pipeline-layout"),
        bind_group_layouts: &[Some(&bind_group_layout)],
        immediate_size: 0,
    });

    let vertex_layout = VertexBufferLayout {
        array_stride: std::mem::size_of::<QuadVertex>() as wgpu::BufferAddress,
        step_mode: VertexStepMode::Vertex,
        attributes: &[
            VertexAttribute { offset: 0, shader_location: 0, format: VertexFormat::Float32x2 },
            VertexAttribute { offset: 8, shader_location: 1, format: VertexFormat::Float32x4 },
        ],
    };

    let pipeline = device.create_render_pipeline(&RenderPipelineDescriptor {
        label: Some("rote-quad-pipeline"),
        layout: Some(&pipeline_layout),
        vertex: VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            compilation_options: Default::default(),
            buffers: &[Some(vertex_layout)],
        },
        primitive: PrimitiveState::default(),
        depth_stencil: None,
        multisample: MultisampleState::default(),
        fragment: Some(FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            compilation_options: Default::default(),
            targets: &[Some(ColorTargetState {
                format,
                blend: Some(BlendState::ALPHA_BLENDING),
                write_mask: ColorWrites::ALL,
            })],
        }),
        multiview_mask: None,
        cache: None,
    });

    (pipeline, bind_group, viewport_buffer)
}
