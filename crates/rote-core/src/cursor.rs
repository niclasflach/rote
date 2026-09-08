/// A cursor position expressed as a char offset into a buffer's rope, plus
/// an optional selection anchor. Line/column are derived on demand from the
/// rope rather than stored, so they can never drift out of sync with edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub head: usize,
    pub anchor: Option<usize>,
}

impl Cursor {
    pub fn at(pos: usize) -> Self {
        Self { head: pos, anchor: None }
    }

    pub fn has_selection(&self) -> bool {
        matches!(self.anchor, Some(a) if a != self.head)
    }

    pub fn selection_range(&self) -> Option<(usize, usize)> {
        self.anchor.map(|a| if a < self.head { (a, self.head) } else { (self.head, a) })
    }

    pub fn collapse(&mut self) {
        self.anchor = None;
    }

    pub fn extend_to(&mut self, pos: usize) {
        if self.anchor.is_none() {
            self.anchor = Some(self.head);
        }
        self.head = pos;
    }

    pub fn move_to(&mut self, pos: usize) {
        self.head = pos;
        self.anchor = None;
    }
}
