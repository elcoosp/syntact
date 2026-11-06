//! The position layer (issue 041): byte offsets ↔ LSP UTF-16
//! line/character positions over one document, plus the word scanner
//! the hover/completion features sit on. The index is rebuilt once per
//! document change; lookups are `O(log n)` binary searches.

use gir_syntax::ByteSpan;
use serde::{Deserialize, Serialize};

/// An LSP position: line + UTF-16 code units into that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Position {
    /// Zero-based line.
    pub line: u32,
    /// Zero-based UTF-16 code-unit offset into the line.
    pub character: u32,
}

/// An LSP range: start and end positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    /// First character of the range.
    pub start: Position,
    /// One past the last character of the range.
    pub end: Position,
}

/// Byte-offset ↔ UTF-16 position index over one document text.
#[derive(Debug, Clone)]
pub struct LineIndex {
    line_starts: Vec<u32>,
}

impl LineIndex {
    /// Build the index for `text`.
    #[must_use]
    pub fn new(text: &str) -> Self {
        let mut line_starts = vec![0u32];
        for (offset, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                // Documents beyond u32::MAX bytes clamp to u32::MAX:
                // positions stay valid, merely saturating.
                let start = u32::try_from(offset + 1).unwrap_or(u32::MAX);
                line_starts.push(start);
            }
        }
        Self { line_starts }
    }

    /// Byte offset → UTF-16 position (offsets past the end clamp).
    #[must_use]
    pub fn position(&self, text: &str, offset: usize) -> Position {
        let offset = offset.min(text.len());
        let offset = u32::try_from(offset).unwrap_or(u32::MAX);
        let line = match self.line_starts.binary_search(&offset) {
            Ok(exact) => exact,
            Err(insert) => insert - 1,
        };
        let line_start = self.line_starts[line] as usize;
        let character: usize = text[line_start..offset as usize]
            .chars()
            .map(char::len_utf16)
            .sum();
        Position {
            line: u32::try_from(line).unwrap_or(u32::MAX),
            character: u32::try_from(character).unwrap_or(u32::MAX),
        }
    }

    /// UTF-16 position → byte offset (positions past the end clamp to
    /// the document end; characters past the line end clamp to the line
    /// end). Returns `None` only for a line number beyond the document.
    #[must_use]
    pub fn offset(&self, text: &str, position: Position) -> Option<usize> {
        let line = position.line as usize;
        let line_start = *self.line_starts.get(line)? as usize;
        let line_end = if line + 1 < self.line_starts.len() {
            // Exclude the newline itself: a position may not point past it.
            self.line_starts[line + 1] as usize - 1
        } else {
            text.len()
        };
        let mut units = 0usize;
        for (byte_offset, ch) in text[line_start..line_end].char_indices() {
            if units >= position.character as usize {
                return Some(line_start + byte_offset);
            }
            units += ch.len_utf16();
        }
        Some(line_end)
    }

    /// Byte span → LSP range.
    #[must_use]
    pub fn range(&self, text: &str, span: ByteSpan) -> Range {
        Range {
            start: self.position(text, span.start),
            end: self.position(text, span.end),
        }
    }

    /// LSP range → byte span (`None` when the start line is out of
    /// range or the range is reversed after clamping).
    #[must_use]
    pub fn span(&self, text: &str, range: Range) -> Option<ByteSpan> {
        let start = self.offset(text, range.start)?;
        let end = self.offset(text, range.end)?;
        (start <= end).then_some(ByteSpan { start, end })
    }

    /// Number of lines (for tests and bounds).
    #[must_use]
    pub fn len(&self) -> usize {
        self.line_starts.len()
    }

    /// The byte offset where `line` starts (`None` past the last line).
    #[must_use]
    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.line_starts.get(line).map(|start| *start as usize)
    }

    /// The line containing byte `offset` (binary search only — no
    /// UTF-16 character accounting).
    #[must_use]
    pub fn line_of(&self, offset: usize) -> usize {
        let offset = u32::try_from(offset).unwrap_or(u32::MAX);
        match self.line_starts.binary_search(&offset) {
            Ok(exact) => exact,
            Err(insert) => insert.saturating_sub(1),
        }
    }

    /// Whether the index has a single line.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.line_starts.len() <= 1
    }
}

/// The identifier run containing `offset` (or ending right at it when
/// the offset sits at a word boundary, so hovering just after a word
/// still resolves).
///
/// Identifier runs are `[A-Za-z0-9_-]` plus any non-ASCII byte — KDL
/// bare identifiers are unicode (`α`, `привет`), and because UTF-8
/// continuation bytes are all `>= 0x80`, runs stay char-boundary
/// aligned and mid-char offsets resolve to the containing char.
/// Quoted-string runs are matched separately by [`quoted_at`].
#[must_use]
pub fn word_at(text: &str, offset: usize) -> Option<ByteSpan> {
    let bytes = text.as_bytes();
    let offset = offset.min(bytes.len());
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b >= 0x80;
    // A word character on the left extends the word leftwards.
    let start = if offset > 0 && is_word(bytes[offset - 1]) {
        let mut start = offset - 1;
        while start > 0 && is_word(bytes[start - 1]) {
            start -= 1;
        }
        start
    } else {
        offset
    };
    if start < bytes.len() && is_word(bytes[start]) {
        let mut end = start + 1;
        while end < bytes.len() && is_word(bytes[end]) {
            end += 1;
        }
        return Some(ByteSpan { start, end });
    }
    None
}

/// The quoted-string literal (including quotes) containing `offset`.
#[must_use]
pub fn quoted_at(text: &str, offset: usize) -> Option<ByteSpan> {
    let bytes = text.as_bytes();
    let offset = offset.min(bytes.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |pos| pos + 1);
    let line_end = text[offset..]
        .find('\n')
        .map_or(text.len(), |rel| offset + rel);
    let line = &bytes[line_start..line_end];
    let mut open: Option<usize> = None;
    for (index, &byte) in line.iter().enumerate() {
        if byte == b'"' {
            match open {
                None => open = Some(index),
                Some(open_index) => {
                    let start = line_start + open_index;
                    let end = line_start + index + 1;
                    if start <= offset && offset < end {
                        return Some(ByteSpan { start, end });
                    }
                    open = None;
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // LineIndex
    // -----------------------------------------------------------------

    #[test]
    fn ascii_positions_round_trip() {
        let text = "grammar {\n    value kind=alt object\n}\n";
        let index = LineIndex::new(text);
        // Ends with a newline: the empty final line counts.
        assert_eq!(index.len(), 4);
        let offset = text.find("object").expect("present");
        let position = index.position(text, offset);
        assert_eq!(
            position,
            Position {
                line: 1,
                character: 19
            }
        );
        assert_eq!(index.offset(text, position), Some(offset));
    }

    #[test]
    fn multibyte_positions_count_utf16_units() {
        // "α" is 2 UTF-8 bytes, 1 UTF-16 unit; "😀" is 4 bytes, 2 units.
        let text = "α😀x\ny";
        let index = LineIndex::new(text);
        let offset_x = 6; // after α (2 bytes) + 😀 (4 bytes)
        let position = index.position(text, offset_x);
        assert_eq!(
            position,
            Position {
                line: 0,
                character: 3
            }
        );
        assert_eq!(index.offset(text, position), Some(offset_x));
    }

    #[test]
    fn crlf_lines_do_not_leak_the_cr_into_the_next_line() {
        let text = "a\r\nbb\r\nccc";
        let index = LineIndex::new(text);
        let offset_bb = text.find("bb").expect("present");
        let position = index.position(text, offset_bb);
        assert_eq!(
            position,
            Position {
                line: 1,
                character: 0
            }
        );
        let offset_ccc = text.find("ccc").expect("present");
        let position = index.position(text, offset_ccc);
        assert_eq!(
            position,
            Position {
                line: 2,
                character: 0
            }
        );
        assert_eq!(
            index.offset(
                text,
                Position {
                    line: 2,
                    character: 3
                }
            ),
            Some(text.len())
        );
    }

    #[test]
    fn positions_clamp_instead_of_panic() {
        let text = "ab\ncd";
        let index = LineIndex::new(text);
        assert_eq!(
            index.position(text, usize::MAX),
            Position {
                line: 1,
                character: 2
            }
        );
        assert_eq!(
            index.offset(
                text,
                Position {
                    line: 9,
                    character: 0
                }
            ),
            None
        );
        // Character past the line end clamps to the line end.
        assert_eq!(
            index.offset(
                text,
                Position {
                    line: 0,
                    character: 99
                }
            ),
            Some(2)
        );
    }

    #[test]
    fn range_conversions_round_trip() {
        let text = "grammar {\n    value kind=alt object\n}\n";
        let index = LineIndex::new(text);
        let span = ByteSpan {
            start: text.find("value").expect("present"),
            end: text.find("object").expect("present") + "object".len(),
        };
        let range = index.range(text, span);
        assert_eq!(range.start.line, 1);
        let back = index.span(text, range).expect("valid");
        assert_eq!(back, span);
    }

    #[test]
    fn reversed_range_is_rejected() {
        let text = "one two";
        let index = LineIndex::new(text);
        let range = Range {
            start: index.position(text, 5),
            end: index.position(text, 2),
        };
        assert_eq!(index.span(text, range), None);
    }

    // -----------------------------------------------------------------
    // Word scanning
    // -----------------------------------------------------------------

    #[test]
    fn word_at_finds_the_run_containing_the_offset() {
        let text = "value kind=alt object";
        let offset = text.find("alt").expect("present") + 1;
        let span = word_at(text, offset).expect("inside a word");
        assert_eq!(span.slice(text), "alt");
    }

    #[test]
    fn word_at_resolves_just_after_a_word() {
        let text = "value kind=alt object";
        let offset = text.find("alt").expect("present") + 3; // at '='
        let span = word_at(text, offset).expect("word ends here");
        assert_eq!(span.slice(text), "alt");
    }

    #[test]
    fn word_at_misses_whitespace_and_dashes_are_word_chars() {
        assert_eq!(word_at("   ", 1), None);
        let text = "wrap-error=true";
        let span = word_at(text, 4).expect("dash run");
        assert_eq!(span.slice(text), "wrap-error");
    }

    #[test]
    fn word_at_handles_unicode_identifiers() {
        let text = "    α kind=lit \"α\"";
        let offset = text.find('α').expect("present") + 1; // mid-char
        let span = word_at(text, offset).expect("unicode run");
        assert_eq!(span.slice(text), "α");
        // The run starts and ends on char boundaries.
        assert!(text.is_char_boundary(span.start));
        assert!(text.is_char_boundary(span.end));
    }

    #[test]
    fn quoted_at_finds_the_enclosing_string() {
        let text = r#"kind=re "[0-9]+""#;
        let offset = text.find("0-9").expect("present");
        let span = quoted_at(text, offset).expect("inside quotes");
        assert_eq!(span.slice(text), r#""[0-9]+""#);
        // The second `{` sits outside the quoted `{`.
        let text = r#"kind=lit "{" {}"#;
        assert_eq!(quoted_at(text, text.len() - 2), None);
    }
}
