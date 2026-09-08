use std::path::{Path, PathBuf};

use ropey::Rope;

/// A single open document. Wraps a rope for O(log n) edits on large files
/// and keeps a linear undo/redo history of whole-rope snapshots.
///
/// The snapshot-based history is intentionally simple for now — it trades
/// memory for correctness while the editing model is still in flux. Once
/// the edit API stabilizes this should become diff-based (store the edit,
/// not the resulting rope).
pub struct Buffer {
    pub id: BufferId,
    pub path: Option<PathBuf>,
    rope: Rope,
    dirty: bool,
    undo_stack: Vec<Rope>,
    redo_stack: Vec<Rope>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BufferId(pub u64);

#[derive(Debug, thiserror::Error)]
pub enum BufferError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("char index {0} is out of bounds for a buffer of length {1}")]
    OutOfBounds(usize, usize),
}

impl Buffer {
    pub fn empty(id: BufferId) -> Self {
        Self {
            id,
            path: None,
            rope: Rope::new(),
            dirty: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn from_str(id: BufferId, text: &str) -> Self {
        Self {
            id,
            path: None,
            rope: Rope::from_str(text),
            dirty: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    pub fn open(id: BufferId, path: impl AsRef<Path>) -> Result<Self, BufferError> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        Ok(Self {
            id,
            path: Some(path.to_path_buf()),
            rope: Rope::from_str(&text),
            dirty: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
    }

    pub fn save(&mut self) -> Result<(), BufferError> {
        let path = self
            .path
            .clone()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "buffer has no path"))?;
        self.save_as(path)
    }

    pub fn save_as(&mut self, path: impl Into<PathBuf>) -> Result<(), BufferError> {
        let path = path.into();
        std::fs::write(&path, self.rope.to_string())?;
        self.path = Some(path);
        self.dirty = false;
        Ok(())
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn rope(&self) -> &Rope {
        &self.rope
    }

    pub fn len_chars(&self) -> usize {
        self.rope.len_chars()
    }

    pub fn len_lines(&self) -> usize {
        self.rope.len_lines()
    }

    pub fn line(&self, idx: usize) -> Option<String> {
        (idx < self.rope.len_lines()).then(|| self.rope.line(idx).to_string())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Insert `text` at the given char offset, recording an undo point.
    pub fn insert(&mut self, char_idx: usize, text: &str) -> Result<(), BufferError> {
        if char_idx > self.rope.len_chars() {
            return Err(BufferError::OutOfBounds(char_idx, self.rope.len_chars()));
        }
        self.push_undo();
        self.rope.insert(char_idx, text);
        self.dirty = true;
        Ok(())
    }

    /// Delete the char range `start..end`, recording an undo point.
    pub fn delete(&mut self, start: usize, end: usize) -> Result<(), BufferError> {
        if end > self.rope.len_chars() || start > end {
            return Err(BufferError::OutOfBounds(end, self.rope.len_chars()));
        }
        if start == end {
            return Ok(());
        }
        self.push_undo();
        self.rope.remove(start..end);
        self.dirty = true;
        Ok(())
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.rope.clone());
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(std::mem::replace(&mut self.rope, prev));
            self.dirty = true;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(std::mem::replace(&mut self.rope, next));
            self.dirty = true;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_undo() {
        let mut buf = Buffer::from_str(BufferId(0), "hello");
        buf.insert(5, " world").unwrap();
        assert_eq!(buf.text(), "hello world");
        assert!(buf.undo());
        assert_eq!(buf.text(), "hello");
        assert!(buf.redo());
        assert_eq!(buf.text(), "hello world");
    }

    #[test]
    fn delete_range() {
        let mut buf = Buffer::from_str(BufferId(0), "hello world");
        buf.delete(5, 11).unwrap();
        assert_eq!(buf.text(), "hello");
    }
}
