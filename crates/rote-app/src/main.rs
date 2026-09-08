use std::sync::{Arc, Mutex};

use rote_core::Editor;
use rote_pyapi::PluginHost;
use rote_render::{Attrs, ClearColor, Color, Family, Metrics, Renderer, Shaping, TextBuffer, TextDraw};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

mod plugins;

const BG: ClearColor = ClearColor {
    r: 0.098,
    g: 0.106,
    b: 0.122,
    a: 1.0,
};
const FG: Color = Color::rgb(214, 219, 227);
const STATUS_FG: Color = Color::rgb(122, 162, 247);

struct WindowState {
    renderer: Renderer,
    window: Arc<Window>,
    editor: Arc<Mutex<Editor>>,
    plugin_host: PluginHost,
    body: TextBuffer,
    status: TextBuffer,
    modifiers: ModifiersState,
}

impl WindowState {
    async fn new(window: Arc<Window>, event_loop: &ActiveEventLoop, path: Option<String>) -> anyhow::Result<Self> {
        let mut renderer = Renderer::new(window.clone(), event_loop).await?;

        let mut editor = Editor::new();
        match &path {
            Some(p) => {
                editor.open_file(p)?;
            }
            None => {
                editor.new_buffer();
                editor.insert_at_cursor(
                    "Welcome to Rote.\n\nThis is a scaffold: a GPU-rendered text buffer using wgpu + cosmic-text (via glyphon).\n\nOpen a file with `rote <path>`. Type to edit, Ctrl+S to save, Ctrl+Shift+H to run the example plugin command, Esc to quit.\n",
                )?;
            }
        }
        let editor = Arc::new(Mutex::new(editor));

        let plugin_host = PluginHost::new(editor.clone());
        for dir in plugins::plugin_dirs() {
            match plugin_host.load_dir(&dir) {
                Ok(0) => {}
                Ok(n) => tracing::info!("loaded {n} plugin(s) from {}", dir.display()),
                Err(e) => tracing::warn!("failed loading plugins from {}: {e:#}", dir.display()),
            }
        }

        let body = TextBuffer::new(renderer.font_system(), Metrics::new(16.0, 22.0));
        let status = TextBuffer::new(renderer.font_system(), Metrics::new(13.0, 16.0));

        let mut state = Self {
            renderer,
            window,
            editor,
            plugin_host,
            body,
            status,
            modifiers: ModifiersState::empty(),
        };
        state.relayout();
        Ok(state)
    }

    fn relayout(&mut self) {
        let (width, height) = self.renderer.size();

        let text = {
            let editor = self.editor.lock().unwrap();
            editor.active_buffer().map(|b| b.text()).unwrap_or_default()
        };
        let status_text = self.status_line_text();

        let font_system = self.renderer.font_system();

        self.body.set_size(Some(width as f32 - 20.0), Some(height as f32 - 40.0));
        self.body
            .set_text(&text, &Attrs::new().family(Family::Monospace), Shaping::Advanced, None);
        self.body.shape_until_scroll(font_system, false);

        self.status.set_size(Some(width as f32), Some(24.0));
        self.status.set_text(
            &status_text,
            &Attrs::new().family(Family::SansSerif),
            Shaping::Advanced,
            None,
        );
        self.status.shape_until_scroll(font_system, false);

        self.window.request_redraw();
    }

    fn status_line_text(&self) -> String {
        let editor = self.editor.lock().unwrap();
        let Some(id) = editor.active_id() else {
            return "[no buffer]".to_string();
        };
        let buf = editor.buffer(id);
        let name = buf
            .and_then(|b| b.path.as_ref())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "[scratch]".to_string());
        let dirty = buf.map(|b| b.is_dirty()).unwrap_or(false);
        let pos = editor.cursor(id).map(|c| c.head).unwrap_or(0);
        let (line, col) = buf
            .map(|b| {
                let rope = b.rope();
                let line = rope.char_to_line(pos.min(rope.len_chars()));
                let col = pos - rope.line_to_char(line);
                (line + 1, col + 1)
            })
            .unwrap_or((1, 1));
        format!(
            "{name}{}  —  Ln {line}, Col {col}  —  Rote (wgpu + cosmic-text)",
            if dirty { " [+]" } else { "" }
        )
    }

    fn handle_key(&mut self, event: KeyEvent) -> anyhow::Result<()> {
        if event.state != ElementState::Pressed {
            return Ok(());
        }
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();

        match &event.logical_key {
            Key::Character(s) if ctrl && shift && s.eq_ignore_ascii_case("h") => {
                self.plugin_host.run_command("hello")?;
            }
            Key::Character(s) if ctrl && s.as_str() == "s" => {
                let mut editor = self.editor.lock().unwrap();
                if let Some(buf) = editor.active_buffer_mut() {
                    if buf.path.is_some() {
                        buf.save()?;
                    }
                }
            }
            Key::Character(s) if !ctrl => {
                self.editor.lock().unwrap().insert_at_cursor(s)?;
            }
            Key::Named(NamedKey::Enter) => {
                self.editor.lock().unwrap().insert_at_cursor("\n")?;
            }
            Key::Named(NamedKey::Space) => {
                self.editor.lock().unwrap().insert_at_cursor(" ")?;
            }
            Key::Named(NamedKey::Backspace) => {
                self.editor.lock().unwrap().backspace_at_cursor()?;
            }
            _ => return Ok(()),
        }

        self.relayout();
        Ok(())
    }
}

struct App {
    state: Option<WindowState>,
    path: Option<String>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_inner_size(LogicalSize::new(1000.0, 700.0))
            .with_title("Rote");
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                tracing::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        match pollster::block_on(WindowState::new(window, event_loop, self.path.clone())) {
            Ok(state) => self.state = Some(state),
            Err(e) => {
                tracing::error!("failed to initialize renderer: {e:#}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                state.renderer.resize(size.width, size.height);
                state.relayout();
            }
            WindowEvent::ModifiersChanged(mods) => {
                state.modifiers = mods.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if matches!(&event.logical_key, Key::Named(NamedKey::Escape)) {
                    event_loop.exit();
                    return;
                }
                if let Err(e) = state.handle_key(event) {
                    tracing::error!("edit error: {e:#}");
                }
            }
            WindowEvent::RedrawRequested => {
                let (_, height) = state.renderer.size();
                let areas = vec![
                    TextDraw {
                        buffer: &state.body,
                        left: 10.0,
                        top: 10.0,
                        color: FG,
                    },
                    TextDraw {
                        buffer: &state.status,
                        left: 10.0,
                        top: (height as f32 - 22.0).max(0.0),
                        color: STATUS_FG,
                    },
                ];
                if let Err(e) = state.renderer.render(areas, BG) {
                    tracing::error!("render error: {e:#}");
                }
            }
            _ => {}
        }
    }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let path = std::env::args().nth(1);
    let event_loop = EventLoop::new()?;
    let mut app = App { state: None, path };
    event_loop.run_app(&mut app)?;
    Ok(())
}
