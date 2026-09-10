//! Editing model for Rote: ropes, cursors, and the buffer registry.
//! Deliberately has no dependency on rendering, windowing or Python —
//! it should stay usable headlessly (e.g. for a future test harness or
//! a language-server-style batch mode).

mod buffer;
mod cursor;
mod editor;

pub use buffer::{Buffer, BufferError, BufferId};
pub use cursor::Cursor;
pub use editor::Editor;

// Re-exported so `rote-app` can name the rope type when mapping between
// rope char offsets and screen-space text layout, without taking its own
// direct dependency on `ropey`.
pub use ropey::Rope;
