use std::collections::HashMap;
use std::path::Path;

use ropey::Rope;

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

    /// Insert text at the active cursor and advance it past the inserted
    /// text. Replaces the current selection, if any, rather than inserting
    /// alongside it.
    pub fn insert_at_cursor(&mut self, text: &str) -> Result<(), BufferError> {
        self.delete_selection()?;
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

    /// Delete one char before the active cursor (backspace), or the
    /// selection if one is active.
    pub fn backspace_at_cursor(&mut self) -> Result<(), BufferError> {
        if self.delete_selection()? {
            return Ok(());
        }
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

    /// Delete one char after the active cursor (Delete key), or the
    /// selection if one is active.
    pub fn delete_forward_at_cursor(&mut self) -> Result<(), BufferError> {
        if self.delete_selection()? {
            return Ok(());
        }
        let id = match self.active {
            Some(id) => id,
            None => return Ok(()),
        };
        let pos = self.cursors.get(&id).map(|c| c.head).unwrap_or(0);
        let len = self.buffers.get(&id).map(|b| b.len_chars()).unwrap_or(0);
        if pos >= len {
            return Ok(());
        }
        if let Some(buf) = self.buffers.get_mut(&id) {
            buf.delete(pos, pos + 1)?;
        }
        Ok(())
    }

    /// Delete the active selection, if any. Returns whether anything was
    /// deleted, so callers (backspace/delete) can fall back to their
    /// single-char behavior when there was no selection to consume.
    pub fn delete_selection(&mut self) -> Result<bool, BufferError> {
        let id = match self.active {
            Some(id) => id,
            None => return Ok(false),
        };
        let range = match self.cursors.get(&id).and_then(Cursor::selection_range) {
            Some(range) => range,
            None => return Ok(false),
        };
        if let Some(buf) = self.buffers.get_mut(&id) {
            buf.delete(range.0, range.1)?;
        }
        if let Some(cursor) = self.cursors.get_mut(&id) {
            cursor.move_to(range.0);
        }
        Ok(true)
    }

    /// Move (or extend the selection to) an absolute char offset — used for
    /// mouse clicks and drags once screen coordinates have been hit-tested
    /// against the laid-out text.
    pub fn set_cursor_pos(&mut self, pos: usize, extend: bool) {
        let Some(rope) = self.active_rope() else { return };
        let pos = pos.min(rope.len_chars());
        if let Some(cursor) = self.active_cursor_mut() {
            if extend {
                cursor.extend_to(pos);
            } else {
                cursor.move_to(pos);
            }
            cursor.sticky_col = None;
        }
    }

    pub fn select_all(&mut self) {
        let Some(rope) = self.active_rope() else { return };
        if let Some(cursor) = self.active_cursor_mut() {
            cursor.anchor = Some(0);
            cursor.head = rope.len_chars();
            cursor.sticky_col = None;
        }
    }

    pub fn move_left(&mut self, extend: bool) {
        if let Some(cursor) = self.active_cursor_mut() {
            let pos = cursor.head.saturating_sub(1);
            Self::apply_horizontal(cursor, pos, extend);
        }
    }

    pub fn move_right(&mut self, extend: bool) {
        let Some(rope) = self.active_rope() else { return };
        if let Some(cursor) = self.active_cursor_mut() {
            let pos = (cursor.head + 1).min(rope.len_chars());
            Self::apply_horizontal(cursor, pos, extend);
        }
    }

    pub fn move_word_left(&mut self, extend: bool) {
        let Some(rope) = self.active_rope() else { return };
        if let Some(cursor) = self.active_cursor_mut() {
            let mut pos = cursor.head;
            while pos > 0 && rope.char(pos - 1).is_whitespace() {
                pos -= 1;
            }
            if pos > 0 {
                let word = Self::is_word_char(rope.char(pos - 1));
                while pos > 0 {
                    let c = rope.char(pos - 1);
                    if c.is_whitespace() || Self::is_word_char(c) != word {
                        break;
                    }
                    pos -= 1;
                }
            }
            Self::apply_horizontal(cursor, pos, extend);
        }
    }

    pub fn move_word_right(&mut self, extend: bool) {
        let Some(rope) = self.active_rope() else { return };
        if let Some(cursor) = self.active_cursor_mut() {
            let len = rope.len_chars();
            let mut pos = cursor.head;
            while pos < len && rope.char(pos).is_whitespace() {
                pos += 1;
            }
            if pos < len {
                let word = Self::is_word_char(rope.char(pos));
                while pos < len {
                    let c = rope.char(pos);
                    if c.is_whitespace() || Self::is_word_char(c) != word {
                        break;
                    }
                    pos += 1;
                }
            }
            Self::apply_horizontal(cursor, pos, extend);
        }
    }

    pub fn move_line_start(&mut self, extend: bool) {
        let Some(rope) = self.active_rope() else { return };
        if let Some(cursor) = self.active_cursor_mut() {
            let line = rope.char_to_line(cursor.head.min(rope.len_chars()));
            let pos = rope.line_to_char(line);
            Self::apply_horizontal(cursor, pos, extend);
        }
    }

    pub fn move_line_end(&mut self, extend: bool) {
        let Some(rope) = self.active_rope() else { return };
        if let Some(cursor) = self.active_cursor_mut() {
            let line = rope.char_to_line(cursor.head.min(rope.len_chars()));
            let pos = Self::line_content_end(&rope, line);
            Self::apply_horizontal(cursor, pos, extend);
        }
    }

    pub fn move_buffer_start(&mut self, extend: bool) {
        if let Some(cursor) = self.active_cursor_mut() {
            Self::apply_horizontal(cursor, 0, extend);
        }
    }

    pub fn move_buffer_end(&mut self, extend: bool) {
        let Some(rope) = self.active_rope() else { return };
        if let Some(cursor) = self.active_cursor_mut() {
            let pos = rope.len_chars();
            Self::apply_horizontal(cursor, pos, extend);
        }
    }

    pub fn move_up(&mut self, extend: bool) {
        let Some(rope) = self.active_rope() else { return };
        if let Some(cursor) = self.active_cursor_mut() {
            let line = rope.char_to_line(cursor.head.min(rope.len_chars()));
            let col = cursor.sticky_col.unwrap_or(cursor.head - rope.line_to_char(line));
            let pos = if line == 0 {
                0
            } else {
                let target_line = line - 1;
                let start = rope.line_to_char(target_line);
                let end = Self::line_content_end(&rope, target_line);
                (start + col).min(end)
            };
            if extend {
                cursor.extend_to(pos);
            } else {
                cursor.move_to(pos);
            }
            cursor.sticky_col = Some(col);
        }
    }

    pub fn move_down(&mut self, extend: bool) {
        let Some(rope) = self.active_rope() else { return };
        if let Some(cursor) = self.active_cursor_mut() {
            let line = rope.char_to_line(cursor.head.min(rope.len_chars()));
            let col = cursor.sticky_col.unwrap_or(cursor.head - rope.line_to_char(line));
            let last_line = rope.len_lines().saturating_sub(1);
            let pos = if line >= last_line {
                rope.len_chars()
            } else {
                let target_line = line + 1;
                let start = rope.line_to_char(target_line);
                let end = Self::line_content_end(&rope, target_line);
                (start + col).min(end)
            };
            if extend {
                cursor.extend_to(pos);
            } else {
                cursor.move_to(pos);
            }
            cursor.sticky_col = Some(col);
        }
    }

    fn apply_horizontal(cursor: &mut Cursor, pos: usize, extend: bool) {
        if extend {
            cursor.extend_to(pos);
        } else {
            cursor.move_to(pos);
        }
        cursor.sticky_col = None;
    }

    fn active_rope(&self) -> Option<Rope> {
        self.active.and_then(|id| self.buffers.get(&id)).map(|b| b.rope().clone())
    }

    fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// End of `line`'s visible content, i.e. just before its `\n`/`\r\n`
    /// terminator (or the buffer's end, for the last line). Cursors landing
    /// "at end of line" stop here rather than past the terminator.
    fn line_content_end(rope: &Rope, line: usize) -> usize {
        let start = rope.line_to_char(line);
        let full_end = if line + 1 < rope.len_lines() {
            rope.line_to_char(line + 1)
        } else {
            rope.len_chars()
        };
        let mut end = full_end;
        if end > start && rope.char(end - 1) == '\n' {
            end -= 1;
            if end > start && rope.char(end - 1) == '\r' {
                end -= 1;
            }
        }
        end
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

    fn head(ed: &Editor) -> usize {
        ed.cursor(ed.active_id().unwrap()).unwrap().head
    }

    #[test]
    fn word_and_line_motions() {
        let mut ed = Editor::new();
        ed.new_buffer();
        ed.insert_at_cursor("foo bar\nbaz").unwrap();
        // cursor is at end ("baz" end, offset 11)
        ed.move_line_start(false);
        assert_eq!(head(&ed), 8); // start of "baz"
        ed.move_word_left(false);
        assert_eq!(head(&ed), 4); // start of "bar" (past the newline, which counts as whitespace)
        ed.move_word_left(false);
        assert_eq!(head(&ed), 0); // start of "foo"
        ed.move_word_right(false);
        assert_eq!(head(&ed), 3); // end of "foo"
        ed.move_line_end(false);
        assert_eq!(head(&ed), 7); // end of "foo bar", before the \n
        ed.move_buffer_end(false);
        assert_eq!(head(&ed), 11);
        ed.move_buffer_start(false);
        assert_eq!(head(&ed), 0);
    }

    #[test]
    fn vertical_motion_keeps_sticky_column() {
        let mut ed = Editor::new();
        ed.new_buffer();
        ed.insert_at_cursor("long line\nhi\nlong line").unwrap();
        ed.move_buffer_start(false);
        for _ in 0..7 {
            ed.move_right(false);
        }
        assert_eq!(head(&ed), 7);
        ed.move_down(false);
        // "hi" is only 2 chars, so the cursor clamps to end of that line...
        assert_eq!(head(&ed), 12);
        ed.move_down(false);
        // ...but remembers column 7 once back on a long enough line.
        assert_eq!(head(&ed), 20);
    }

    #[test]
    fn selection_extends_and_deletes() {
        let mut ed = Editor::new();
        ed.new_buffer();
        ed.insert_at_cursor("hello world").unwrap();
        ed.move_buffer_start(false);
        for _ in 0..5 {
            ed.move_right(true);
        }
        let id = ed.active_id().unwrap();
        assert_eq!(ed.cursor(id).unwrap().selection_range(), Some((0, 5)));
        ed.insert_at_cursor("bye").unwrap();
        assert_eq!(ed.active_buffer().unwrap().text(), "bye world");
        assert_eq!(head(&ed), 3);
    }

    #[test]
    fn select_all_selects_whole_buffer() {
        let mut ed = Editor::new();
        ed.new_buffer();
        ed.insert_at_cursor("hello").unwrap();
        ed.select_all();
        let id = ed.active_id().unwrap();
        assert_eq!(ed.cursor(id).unwrap().selection_range(), Some((0, 5)));
    }
}
