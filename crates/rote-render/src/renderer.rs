use std::sync::Arc;

use glyphon::{Cache, FontSystem, SwashCache, TextArea, TextAtlas, TextBounds, TextRenderer, Viewport};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, CurrentSurfaceTexture, DeviceDescriptor, Instance,
    InstanceDescriptor, LoadOp, MultisampleState, Operations, PresentMode, RenderPassColorAttachment,
    RenderPassDescriptor, RequestAdapterOptions, SurfaceColorSpace, SurfaceConfiguration, TextureFormat,
    TextureUsages, TextureViewDescriptor,
};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

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
    /// `(left, top)` origin. `clear_color` is the background for the whole
    /// surface.
    pub fn render(
        &mut self,
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
