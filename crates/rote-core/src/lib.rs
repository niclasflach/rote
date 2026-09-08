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
