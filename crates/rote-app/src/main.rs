use std::sync::{Arc, Mutex};

use rote_core::{Editor, Rope};
use rote_pyapi::{build_chord, PluginHost};
use rote_render::{
    Attrs, ClearColor, Color, Family, LayoutCursor, Metrics, RectDraw, Renderer, Shaping, TextBuffer, TextDraw,
    Wrap,
};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
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
const CURSOR_COLOR: [f32; 4] = [0.839, 0.859, 0.890, 1.0];
const SELECTION_COLOR: [f32; 4] = [0.30, 0.38, 0.62, 0.35];
const CURSOR_WIDTH: f32 = 2.0;

const BASE_LEFT: f32 = 10.0;
const BODY_TOP: f32 = 10.0;
const BODY_FONT_SIZE: f32 = 16.0;
const BODY_LINE_HEIGHT: f32 = 22.0;
const GUTTER_FG: Color = Color::rgb(110, 118, 138);
const GUTTER_GAP: f32 = 16.0;

/// Converts a rope char offset to the `(line, byte_index)` cursor that
/// cosmic-text's layout API (hit-testing, highlight spans) expects.
fn char_offset_to_layout_cursor(rope: &Rope, pos: usize) -> LayoutCursor {
    let pos = pos.min(rope.len_chars());
    let line = rope.char_to_line(pos);
    let col_chars = pos - rope.line_to_char(line);
    let line_str = rope.line(line).to_string();
    let byte_idx = line_str
        .char_indices()
        .nth(col_chars)
        .map(|(b, _)| b)
        .unwrap_or(line_str.len());
    LayoutCursor::new(line, byte_idx)
}

/// The inverse of [`char_offset_to_layout_cursor`] — converts a cosmic-text
/// hit-test result back into a rope char offset.
fn layout_cursor_to_char_offset(rope: &Rope, cursor: LayoutCursor) -> usize {
    let line = cursor.line.min(rope.len_lines().saturating_sub(1));
    let line_str = rope.line(line).to_string();
    let byte_idx = cursor.index.min(line_str.len());
    let col_chars = line_str[..byte_idx].chars().count();
    rope.line_to_char(line) + col_chars
}

/// The key-name half of a `rote_pyapi::build_chord` chord for a winit key
/// event — lowercased so `"P"` (Shift held) and `"p"` canonicalize the same
/// way; `shift` is carried separately as a modifier flag, same as the
/// hardcoded `Ctrl+Shift+H` arm above does via `eq_ignore_ascii_case`.
/// `None` for a key with no sensible chord name (modifier-only presses,
/// media keys, IME composition, ...) — those never reach a plugin keymap.
fn key_name(key: &Key) -> Option<String> {
    match key {
        Key::Character(s) => Some(s.as_str().to_ascii_lowercase()),
        Key::Named(named) => named_key_name(*named).map(str::to_string),
        _ => None,
    }
}

fn named_key_name(named: NamedKey) -> Option<&'static str> {
    Some(match named {
        NamedKey::Enter => "enter",
        NamedKey::Tab => "tab",
        NamedKey::Space => "space",
        NamedKey::Backspace => "backspace",
        NamedKey::Delete => "delete",
        NamedKey::Escape => "escape",
        NamedKey::ArrowUp => "up",
        NamedKey::ArrowDown => "down",
        NamedKey::ArrowLeft => "left",
        NamedKey::ArrowRight => "right",
        NamedKey::Home => "home",
        NamedKey::End => "end",
        NamedKey::PageUp => "pageup",
        NamedKey::PageDown => "pagedown",
        NamedKey::Insert => "insert",
        NamedKey::F1 => "f1",
        NamedKey::F2 => "f2",
        NamedKey::F3 => "f3",
        NamedKey::F4 => "f4",
        NamedKey::F5 => "f5",
        NamedKey::F6 => "f6",
        NamedKey::F7 => "f7",
        NamedKey::F8 => "f8",
        NamedKey::F9 => "f9",
        NamedKey::F10 => "f10",
        NamedKey::F11 => "f11",
        NamedKey::F12 => "f12",
        _ => return None,
    })
}

/// The number of rows to show below the query line — items beyond this
/// are still filterable, just not all listed at once. Keeps a picker over
/// a big directory cheap to shape and readable on screen.
const MAX_PICKER_ROWS: usize = 20;

/// Drives the in-editor overlay a plugin gets from `rote.open_picker` —
/// `rote-pyapi` only stores the item list and the `on_select` callback;
/// everything about actually *showing* a filterable, navigable list (the
/// query text, which row is selected, rendering) lives here instead, since
/// none of it needs Python at all.
///
/// While open, the picker replaces the body/gutter draw for that frame
/// rather than floating on top of them (see `RedrawRequested`) — a real
/// floating overlay would need a second, non-clearing render pass so its
/// panel occludes the body text under it instead of being drawn under it;
/// swapping the view is simpler and has no draw-order pitfalls.
struct Picker {
    items: Vec<String>,
    query: String,
    selected: usize,
}

impl Picker {
    fn new(items: Vec<String>) -> Self {
        Self { items, query: String::new(), selected: 0 }
    }

    fn filtered(&self) -> Vec<&str> {
        if self.query.is_empty() {
            self.items.iter().map(String::as_str).collect()
        } else {
            let query = self.query.to_ascii_lowercase();
            self.items
                .iter()
                .filter(|item| item.to_ascii_lowercase().contains(&query))
                .map(String::as_str)
                .collect()
        }
    }

    /// Clamps `selected` back into range after the filtered list shrinks
    /// (typing narrowed it, or it was already empty).
    fn clamp_selection(&mut self) {
        let count = self.filtered().len();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }
}

struct WindowState {
    renderer: Renderer,
    window: Arc<Window>,
    editor: Arc<Mutex<Editor>>,
    plugin_host: PluginHost,
    body: TextBuffer,
    status: TextBuffer,
    gutter: TextBuffer,
    picker: Option<Picker>,
    picker_buffer: TextBuffer,
    /// Screen-space x where the body text starts — `BASE_LEFT` normally,
    /// pushed right by a gutter plugin's rendered width (if any).
    /// Recomputed every [`WindowState::relayout`].
    body_left: f32,
    modifiers: ModifiersState,
    mouse_pos: (f32, f32),
    dragging: bool,
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

        let mut body = TextBuffer::new(renderer.font_system(), Metrics::new(BODY_FONT_SIZE, BODY_LINE_HEIGHT));
        // No wrapping (yet): a gutter's Nth row must line up with the body's
        // Nth row, and the plugin API deals in logical line numbers, not
        // wrapped visual rows. Long lines run off the right edge for now
        // instead of wrapping — see the roadmap note in README.md.
        body.set_wrap(Wrap::None);
        let status = TextBuffer::new(renderer.font_system(), Metrics::new(13.0, 16.0));
        let mut gutter = TextBuffer::new(renderer.font_system(), Metrics::new(BODY_FONT_SIZE, BODY_LINE_HEIGHT));
        gutter.set_wrap(Wrap::None);
        let mut picker_buffer = TextBuffer::new(renderer.font_system(), Metrics::new(BODY_FONT_SIZE, BODY_LINE_HEIGHT));
        picker_buffer.set_wrap(Wrap::None);

        let mut state = Self {
            renderer,
            window,
            editor,
            plugin_host,
            body,
            status,
            gutter,
            picker: None,
            picker_buffer,
            body_left: BASE_LEFT,
            modifiers: ModifiersState::empty(),
            mouse_pos: (0.0, 0.0),
            dragging: false,
        };
        state.relayout();
        Ok(state)
    }

    fn relayout(&mut self) {
        let (width, height) = self.renderer.size();

        let (text, total_lines, current_line) = {
            let editor = self.editor.lock().unwrap();
            match editor.active_id().and_then(|id| Some((editor.buffer(id)?, editor.cursor(id)?))) {
                Some((buf, cursor)) => {
                    let line = buf.rope().char_to_line(cursor.head.min(buf.len_chars()));
                    (buf.text(), buf.len_lines(), line)
                }
                None => (String::new(), 1, 0),
            }
        };
        let status_text = self.status_line_text();

        let gutter_text = if self.plugin_host.has_gutter() {
            (0..total_lines)
                .map(|i| self.plugin_host.gutter_text(i + 1, false, i == current_line).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            String::new()
        };

        let font_system = self.renderer.font_system();

        self.gutter
            .set_text(&gutter_text, &Attrs::new().family(Family::Monospace), Shaping::Advanced, None);
        self.gutter.shape_until_scroll(font_system, false);
        let gutter_width = self.gutter.layout_runs().map(|r| r.line_w).fold(0.0_f32, f32::max);
        self.body_left = if gutter_width > 0.0 { BASE_LEFT + gutter_width + GUTTER_GAP } else { BASE_LEFT };

        self.body
            .set_size(Some(width as f32 - self.body_left - 10.0), Some(height as f32 - 40.0));
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

        if let Some(picker) = &self.picker {
            let mut lines = vec![format!("Open file: {}_", picker.query), String::new()];
            let filtered = picker.filtered();
            lines.extend(filtered.iter().take(MAX_PICKER_ROWS).enumerate().map(|(i, item)| {
                let marker = if i == picker.selected { "> " } else { "  " };
                format!("{marker}{item}")
            }));
            if filtered.is_empty() {
                lines.push("  (no matches)".to_string());
            }
            let picker_text = lines.join("\n");
            self.picker_buffer
                .set_text(&picker_text, &Attrs::new().family(Family::Monospace), Shaping::Advanced, None);
            self.picker_buffer.shape_until_scroll(font_system, false);
        }

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
        if self.picker.is_some() {
            return self.handle_picker_key(event);
        }
        let ctrl = self.modifiers.control_key();
        let shift = self.modifiers.shift_key();

        match &event.logical_key {
            Key::Character(s) if ctrl && s.as_str() == "s" => {
                let mut editor = self.editor.lock().unwrap();
                if let Some(buf) = editor.active_buffer_mut() {
                    if buf.path.is_some() {
                        buf.save()?;
                    }
                }
            }
            Key::Character(s) if ctrl && s.as_str() == "a" => {
                self.editor.lock().unwrap().select_all();
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
            Key::Named(NamedKey::Delete) => {
                self.editor.lock().unwrap().delete_forward_at_cursor()?;
            }
            Key::Named(NamedKey::ArrowLeft) if ctrl => {
                self.editor.lock().unwrap().move_word_left(shift);
            }
            Key::Named(NamedKey::ArrowLeft) => {
                self.editor.lock().unwrap().move_left(shift);
            }
            Key::Named(NamedKey::ArrowRight) if ctrl => {
                self.editor.lock().unwrap().move_word_right(shift);
            }
            Key::Named(NamedKey::ArrowRight) => {
                self.editor.lock().unwrap().move_right(shift);
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.editor.lock().unwrap().move_up(shift);
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.editor.lock().unwrap().move_down(shift);
            }
            Key::Named(NamedKey::Home) if ctrl => {
                self.editor.lock().unwrap().move_buffer_start(shift);
            }
            Key::Named(NamedKey::Home) => {
                self.editor.lock().unwrap().move_line_start(shift);
            }
            Key::Named(NamedKey::End) if ctrl => {
                self.editor.lock().unwrap().move_buffer_end(shift);
            }
            Key::Named(NamedKey::End) => {
                self.editor.lock().unwrap().move_line_end(shift);
            }
            key => {
                // Nothing built in wants this chord — hand it to whatever a
                // plugin bound via `rote.bind_key`. A no-op if nothing did.
                if let Some(name) = key_name(key) {
                    let alt = self.modifiers.alt_key();
                    let sup = self.modifiers.super_key();
                    let chord = build_chord(ctrl, alt, shift, sup, &name);
                    self.plugin_host.run_keymap(&chord)?;
                    // The chord we just ran might have been `rote.open_picker` —
                    // pick up its item list and start driving the overlay.
                    if let Some(items) = self.plugin_host.pending_picker_items() {
                        self.picker = Some(Picker::new(items));
                    }
                } else {
                    return Ok(());
                }
            }
        }

        self.relayout();
        Ok(())
    }

    fn handle_picker_key(&mut self, event: KeyEvent) -> anyhow::Result<()> {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.plugin_host.cancel_picker();
                self.picker = None;
            }
            Key::Named(NamedKey::Enter) => {
                let chosen = self.picker.as_ref().and_then(|p| p.filtered().get(p.selected).map(|s| s.to_string()));
                self.picker = None;
                if let Some(chosen) = chosen {
                    self.plugin_host.confirm_picker(&chosen)?;
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(picker) = &mut self.picker {
                    picker.selected = picker.selected.saturating_sub(1);
                }
            }
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(picker) = &mut self.picker {
                    picker.selected += 1;
                    picker.clamp_selection();
                }
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(picker) = &mut self.picker {
                    picker.query.pop();
                    picker.selected = 0;
                }
            }
            Key::Named(NamedKey::Space) => {
                if let Some(picker) = &mut self.picker {
                    picker.query.push(' ');
                    picker.selected = 0;
                }
            }
            Key::Character(s) => {
                if let Some(picker) = &mut self.picker {
                    picker.query.push_str(s);
                    picker.selected = 0;
                }
            }
            _ => return Ok(()),
        }

        self.relayout();
        Ok(())
    }

    /// Hit-tests a window-space pixel position against the body text and
    /// returns the rope char offset under it, if any (`None` before the
    /// buffer has been laid out, or if the click landed outside the body's
    /// vertical extent and no line matched).
    fn hit_test(&self, window_x: f32, window_y: f32) -> Option<usize> {
        let local_x = window_x - self.body_left;
        let local_y = window_y - BODY_TOP;
        let layout_cursor = self.body.hit(local_x, local_y)?;
        let editor = self.editor.lock().unwrap();
        let rope = editor.active_buffer()?.rope();
        Some(layout_cursor_to_char_offset(rope, layout_cursor))
    }

    fn handle_mouse_pressed(&mut self, button: MouseButton) {
        if button != MouseButton::Left {
            return;
        }
        if let Some(pos) = self.hit_test(self.mouse_pos.0, self.mouse_pos.1) {
            self.editor.lock().unwrap().set_cursor_pos(pos, self.modifiers.shift_key());
        }
        self.dragging = true;
        self.relayout();
    }

    fn handle_mouse_released(&mut self, button: MouseButton) {
        if button == MouseButton::Left {
            self.dragging = false;
        }
    }

    fn handle_cursor_moved(&mut self, x: f32, y: f32) {
        self.mouse_pos = (x, y);
        if !self.dragging {
            return;
        }
        if let Some(pos) = self.hit_test(x, y) {
            self.editor.lock().unwrap().set_cursor_pos(pos, true);
            self.relayout();
        }
    }

    /// Computes the caret and selection-highlight rectangles for the
    /// current frame, in the same window-space pixel coordinates as the
    /// body's `TextDraw` origin. Empty while a picker overlay is up — it
    /// draws its own selected-row highlight instead, see [`Self::picker_rects`].
    fn cursor_rects(&mut self) -> Vec<RectDraw> {
        if self.picker.is_some() {
            return Vec::new();
        }
        let (rope, cursor) = {
            let editor = self.editor.lock().unwrap();
            let Some(id) = editor.active_id() else { return Vec::new() };
            let Some(buf) = editor.buffer(id) else { return Vec::new() };
            let Some(cursor) = editor.cursor(id) else { return Vec::new() };
            (buf.rope().clone(), *cursor)
        };

        let mut rects = Vec::new();

        if let Some((start, end)) = cursor.selection_range() {
            let start_cursor = char_offset_to_layout_cursor(&rope, start);
            let end_cursor = char_offset_to_layout_cursor(&rope, end);
            for run in self.body.layout_runs() {
                for (x, width) in run.highlight(start_cursor, end_cursor) {
                    rects.push(RectDraw {
                        x: self.body_left + x,
                        y: BODY_TOP + run.line_top,
                        width,
                        height: run.line_height,
                        color: SELECTION_COLOR,
                    });
                }
            }
        }

        let head_cursor = char_offset_to_layout_cursor(&rope, cursor.head);
        let (x, top) = self.body.cursor_position(&head_cursor).unwrap_or((0.0, 0.0));
        rects.push(RectDraw {
            x: self.body_left + x,
            y: BODY_TOP + top,
            width: CURSOR_WIDTH,
            height: BODY_LINE_HEIGHT,
            color: CURSOR_COLOR,
        });

        rects
    }

    /// Highlights the selected row of an open picker overlay — the picker
    /// text itself already marks it with `>`, this is just the visual band
    /// behind it, same technique as the caret/selection rects above (read
    /// the shaped buffer's own layout rather than recomputing pixel math).
    fn picker_rects(&self) -> Vec<RectDraw> {
        let Some(picker) = &self.picker else { return Vec::new() };
        // Row 0 is the query prompt, row 1 a blank separator, so the
        // selected item starts at row 2 — see the `lines` built in relayout.
        let row = 2 + picker.selected;
        let Some(run) = self.picker_buffer.layout_runs().nth(row) else { return Vec::new() };
        let (width, _) = self.renderer.size();
        vec![RectDraw {
            x: BASE_LEFT,
            y: BODY_TOP + run.line_top,
            width: width as f32 - BASE_LEFT - 10.0,
            height: run.line_height,
            color: SELECTION_COLOR,
        }]
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
            WindowEvent::CursorMoved { position, .. } => {
                state.handle_cursor_moved(position.x as f32, position.y as f32);
            }
            WindowEvent::MouseInput { state: element_state, button, .. } => {
                match element_state {
                    ElementState::Pressed => state.handle_mouse_pressed(button),
                    ElementState::Released => state.handle_mouse_released(button),
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Esc quits — unless a picker overlay is up, where it closes
                // that instead (handled inside handle_key/handle_picker_key).
                if matches!(&event.logical_key, Key::Named(NamedKey::Escape)) && state.picker.is_none() {
                    event_loop.exit();
                    return;
                }
                if let Err(e) = state.handle_key(event) {
                    tracing::error!("edit error: {e:#}");
                }
            }
            WindowEvent::RedrawRequested => {
                let (_, height) = state.renderer.size();
                let has_picker = state.picker.is_some();
                // A picker overlay takes over the whole body area rather than
                // floating on top of it — simpler and correct by construction
                // (no draw-order/occlusion bugs), see the design note in
                // Picker's doc comment.
                let rects = if has_picker { state.picker_rects() } else { state.cursor_rects() };
                let status = TextDraw {
                    buffer: &state.status,
                    left: 10.0,
                    top: (height as f32 - 22.0).max(0.0),
                    color: STATUS_FG,
                };
                let areas = if has_picker {
                    vec![
                        TextDraw { buffer: &state.picker_buffer, left: BASE_LEFT, top: BODY_TOP, color: FG },
                        status,
                    ]
                } else {
                    vec![
                        TextDraw { buffer: &state.gutter, left: BASE_LEFT, top: BODY_TOP, color: GUTTER_FG },
                        TextDraw { buffer: &state.body, left: state.body_left, top: BODY_TOP, color: FG },
                        status,
                    ]
                };
                if let Err(e) = state.renderer.render(&rects, areas, BG) {
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
