//! GPU rendering for Rote: window surface management (wgpu) and text
//! shaping/rasterization (cosmic-text via glyphon). Nothing in here knows
//! about editing semantics — `rote-app` hands it a laid-out [`TextBuffer`]
//! and this crate gets pixels for it on screen.

mod renderer;

pub use renderer::{RectDraw, Renderer, TextDraw};

// Re-exported so callers only need to depend on `rote-render`, not reach
// into `glyphon`/`cosmic-text` directly for basic text layout types.
pub use glyphon::{
    Attrs, Buffer as TextBuffer, Color, Cursor as LayoutCursor, Family, FontSystem, Metrics, Shaping, Weight, Wrap,
};
pub use wgpu::Color as ClearColor;
