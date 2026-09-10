//! Tree-sitter-based syntax highlighting. Every language is a normal
//! statically-linked Cargo dependency (`tree-sitter-python` today) with its
//! `highlights.scm` query bundled inside that same crate — no runtime
//! downloads, no dynamically loaded native code. Adding a language later is
//! `cargo add tree-sitter-<language>` plus a constructor here, not a
//! network fetch at startup.

use std::ops::Range;

use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

/// A semantic category a highlighted span belongs to. Deliberately small
/// and generic — not every language/query distinguishes all of these, and
/// callers map each to an actual color via their own theme rather than
/// this crate hardcoding one (this crate has no rendering knowledge at
/// all — see `rote-render`/`rote-app` for that).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HighlightKind {
    Keyword,
    String,
    Comment,
    Number,
    Function,
    Type,
    Constant,
    Property,
    Operator,
    Punctuation,
    Variable,
    Module,
}

/// One highlighted span: a byte range into the source text, tagged with
/// what it is. [`Highlighter::highlight`] guarantees the returned spans
/// are sorted by `range.start` and never overlap — a gap between two
/// spans (or before the first / after the last) is plain, unhighlighted
/// text.
pub struct HighlightSpan {
    pub range: Range<usize>,
    pub kind: HighlightKind,
}

/// Parses source text for one language and produces [`HighlightSpan`]s.
/// Owns a `tree_sitter::Parser` and a compiled `Query`, both nontrivial to
/// build — construct one per language once (`rote-app` keeps one in its
/// `WindowState`) and reuse it across relayouts rather than rebuilding it
/// every keystroke.
pub struct Highlighter {
    parser: Parser,
    query: Query,
}

impl Highlighter {
    /// A highlighter for Python, using the grammar and highlight query
    /// bundled inside the `tree-sitter-python` crate itself.
    pub fn python() -> Self {
        let language = tree_sitter_python::LANGUAGE.into();
        let mut parser = Parser::new();
        parser
            .set_language(&language)
            .expect("bundled tree-sitter-python grammar failed to load");
        let query = Query::new(&language, tree_sitter_python::HIGHLIGHTS_QUERY)
            .expect("bundled tree-sitter-python highlights.scm failed to compile");
        Self { parser, query }
    }

    /// Parses `source` from scratch and returns its highlight spans,
    /// sorted and non-overlapping. Re-parses the whole text every call
    /// rather than incrementally editing the previous tree — fine at the
    /// sizes this scaffold deals with; revisit with `tree_sitter::InputEdit`
    /// if large-file typing latency ever shows up.
    pub fn highlight(&mut self, source: &str) -> Vec<HighlightSpan> {
        let Some(tree) = self.parser.parse(source, None) else {
            return Vec::new();
        };

        let mut query_cursor = QueryCursor::new();
        let mut captures = query_cursor.captures(&self.query, tree.root_node(), source.as_bytes());

        let mut spans: Vec<HighlightSpan> = Vec::new();
        while let Some((mat, capture_index)) = captures.next() {
            let capture = mat.captures()[*capture_index];
            let name = self.query.capture_names()[capture.index as usize];
            let Some(kind) = kind_for_capture(name) else { continue };
            insert_span(&mut spans, capture.node.byte_range(), kind);
        }
        spans
    }
}

/// Inserts `range` as a highlighted span, splitting or discarding any part
/// of an existing span it overlaps. Captures later in query-iteration
/// order win for bytes they share with an earlier one — tree-sitter
/// highlight queries are conventionally authored with broad patterns
/// first and more specific overrides after (e.g. a whole f-string as
/// `string`, then its interpolated expression separately), so "later
/// wins" is what makes those overrides actually take effect.
fn insert_span(spans: &mut Vec<HighlightSpan>, range: Range<usize>, kind: HighlightKind) {
    if range.is_empty() {
        return;
    }
    let mut i = 0;
    while i < spans.len() {
        let existing_start = spans[i].range.start;
        let existing_end = spans[i].range.end;
        if existing_end <= range.start || existing_start >= range.end {
            i += 1;
            continue;
        }
        let existing_kind = spans[i].kind;
        spans.remove(i);
        if range.end < existing_end {
            spans.insert(i, HighlightSpan { range: range.end..existing_end, kind: existing_kind });
        }
        if existing_start < range.start {
            spans.insert(i, HighlightSpan { range: existing_start..range.start, kind: existing_kind });
            i += 1; // this piece ends exactly at range.start — never overlaps it, skip re-checking it
        }
    }
    let pos = spans.partition_point(|s| s.range.start < range.start);
    spans.insert(pos, HighlightSpan { range, kind });
}

/// tree-sitter capture names are dotted (e.g. `"function.builtin"`,
/// `"punctuation.bracket"`); only the top-level category is mapped —
/// good enough to start, and still forward-compatible since unmapped
/// suffixes just fall into their category's default.
fn kind_for_capture(name: &str) -> Option<HighlightKind> {
    let top = name.split('.').next().unwrap_or(name);
    Some(match top {
        "keyword" => HighlightKind::Keyword,
        "string" => HighlightKind::String,
        "comment" => HighlightKind::Comment,
        "number" => HighlightKind::Number,
        "function" | "method" => HighlightKind::Function,
        "type" | "constructor" => HighlightKind::Type,
        "constant" | "boolean" => HighlightKind::Constant,
        "property" | "attribute" => HighlightKind::Property,
        "operator" => HighlightKind::Operator,
        "punctuation" => HighlightKind::Punctuation,
        "variable" | "parameter" => HighlightKind::Variable,
        "module" | "namespace" => HighlightKind::Module,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_basic_python() {
        let mut hl = Highlighter::python();
        let source = "def double(x):\n    return x * 2\n";
        let spans = hl.highlight(source);

        assert!(!spans.is_empty());
        // Sorted and non-overlapping.
        for pair in spans.windows(2) {
            assert!(pair[0].range.end <= pair[1].range.start);
        }

        let text_of = |s: &HighlightSpan| &source[s.range.clone()];
        assert!(spans.iter().any(|s| s.kind == HighlightKind::Keyword && text_of(s) == "def"));
        assert!(spans.iter().any(|s| s.kind == HighlightKind::Keyword && text_of(s) == "return"));
        assert!(spans.iter().any(|s| s.kind == HighlightKind::Function && text_of(s) == "double"));
        assert!(spans.iter().any(|s| s.kind == HighlightKind::Number && text_of(s) == "2"));
    }

    #[test]
    fn insert_span_splits_around_overlap() {
        let mut spans = vec![HighlightSpan { range: 0..10, kind: HighlightKind::String }];
        insert_span(&mut spans, 3..6, HighlightKind::Keyword);

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].range, 0..3);
        assert_eq!(spans[0].kind, HighlightKind::String);
        assert_eq!(spans[1].range, 3..6);
        assert_eq!(spans[1].kind, HighlightKind::Keyword);
        assert_eq!(spans[2].range, 6..10);
        assert_eq!(spans[2].kind, HighlightKind::String);
    }
}
