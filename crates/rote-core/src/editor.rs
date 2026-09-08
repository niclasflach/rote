use std::collections::HashMap;
use std::path::Path;

use crate::buffer::{Buffer, BufferError, BufferId};
use crate::cursor::Cursor;

/// Top-level editor state: every open buffer, plus one cursor per buffer
/// and which buffer currently has focus. This has no knowledge of
/// rendering, windowing or plugins — those layer on top via `rote-render`,
/// `rote-ui` and `rote-pyapi`.
pub struct Editor {
    buffers: HashMap<BufferId, Buffer>,
    cursors: HashMap<BufferId, Cursor>,
    active: Option<BufferId>,
    next_id: u64,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            buffers: HashMap::new(),
            cursors: HashMap::new(),
            active: None,
            next_id: 0,
        }
    }

    fn alloc_id(&mut self) -> BufferId {
        let id = BufferId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn new_buffer(&mut self) -> BufferId {
        let id = self.alloc_id();
        self.buffers.insert(id, Buffer::empty(id));
        self.cursors.insert(id, Cursor::at(0));
        self.active = Some(id);
        id
    }

    pub fn open_file(&mut self, path: impl AsRef<Path>) -> Result<BufferId, BufferError> {
        let id = self.alloc_id();
        let buffer = Buffer::open(id, path)?;
        self.buffers.insert(id, buffer);
        self.cursors.insert(id, Cursor::at(0));
        self.active = Some(id);
        Ok(id)
    }

    pub fn active_id(&self) -> Option<BufferId> {
        self.active
    }

    pub fn set_active(&mut self, id: BufferId) {
        if self.buffers.contains_key(&id) {
            self.active = Some(id);
        }
    }

    pub fn buffer(&self, id: BufferId) -> Option<&Buffer> {
        self.buffers.get(&id)
    }

    pub fn buffer_mut(&mut self, id: BufferId) -> Option<&mut Buffer> {
        self.buffers.get_mut(&id)
    }

    pub fn active_buffer(&self) -> Option<&Buffer> {
        self.active.and_then(|id| self.buffers.get(&id))
    }

    pub fn active_buffer_mut(&mut self) -> Option<&mut Buffer> {
        self.active.and_then(|id| self.buffers.get_mut(&id))
    }

    pub fn cursor(&self, id: BufferId) -> Option<&Cursor> {
        self.cursors.get(&id)
    }

    pub fn cursor_mut(&mut self, id: BufferId) -> Option<&mut Cursor> {
        self.cursors.get_mut(&id)
    }

    pub fn active_cursor_mut(&mut self) -> Option<&mut Cursor> {
        let id = self.active?;
        self.cursors.get_mut(&id)
    }

    /// Insert text at the active cursor and advance it past the inserted text.
    pub fn insert_at_cursor(&mut self, text: &str) -> Result<(), BufferError> {
        let id = match self.active {
            Some(id) => id,
            None => return Ok(()),
        };
        let pos = self.cursors.get(&id).map(|c| c.head).unwrap_or(0);
        if let Some(buf) = self.buffers.get_mut(&id) {
            buf.insert(pos, text)?;
        }
        if let Some(cursor) = self.cursors.get_mut(&id) {
            cursor.move_to(pos + text.chars().count());
        }
        Ok(())
    }

    /// Delete one char before the active cursor (backspace).
    pub fn backspace_at_cursor(&mut self) -> Result<(), BufferError> {
        let id = match self.active {
            Some(id) => id,
            None => return Ok(()),
        };
        let pos = self.cursors.get(&id).map(|c| c.head).unwrap_or(0);
        if pos == 0 {
            return Ok(());
        }
        if let Some(buf) = self.buffers.get_mut(&id) {
            buf.delete(pos - 1, pos)?;
        }
        if let Some(cursor) = self.cursors.get_mut(&id) {
            cursor.move_to(pos - 1);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_backspace_move_cursor() {
        let mut ed = Editor::new();
        ed.new_buffer();
        ed.insert_at_cursor("hi").unwrap();
        assert_eq!(ed.active_buffer().unwrap().text(), "hi");
        assert_eq!(ed.cursor(ed.active_id().unwrap()).unwrap().head, 2);
        ed.backspace_at_cursor().unwrap();
        assert_eq!(ed.active_buffer().unwrap().text(), "h");
        assert_eq!(ed.cursor(ed.active_id().unwrap()).unwrap().head, 1);
    }
}
