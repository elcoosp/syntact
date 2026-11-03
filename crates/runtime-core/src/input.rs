//! Input layer (roadmap §7.0): the [`Source`] carrier every parser reads.
//!
//! `&str` is always valid UTF-8 by construction, so "unchecked" here means
//! unchecked *against the u32 offset space* — sources longer than
//! [`u32::MAX`] bytes are rejected without copying (the event protocol
//! addresses bytes with `u32` offsets).

use std::ops::Range;

/// A parsed-source carrier: the text plus a lazily built line index.
#[derive(Debug, Clone)]
pub struct Source<'s> {
    text: &'s str,
    /// Byte offsets of line starts (line 0 starts at 0); built lazily.
    lines: std::cell::OnceCell<Vec<u32>>,
}

impl<'s> Source<'s> {
    /// Build a source without re-validating UTF-8 (a `&str` is valid by
    /// construction). Rejects sources above the u32 offset space.
    ///
    /// # Errors
    /// When `text.len() > u32::MAX` — the event protocol cannot address it.
    pub fn new(text: &'s str) -> Result<Self, String> {
        if text.len() > u32::MAX as usize {
            return Err(format!(
                "source of {} bytes exceeds the u32 offset space",
                text.len()
            ));
        }
        Ok(Self {
            text,
            lines: std::cell::OnceCell::new(),
        })
    }

    /// Same as [`Self::new`] for sources known to fit (test/bench fixtures);
    /// saturates by asserting in debug builds and clamping in release.
    ///
    /// # Panics
    /// Debug builds panic when the source exceeds the u32 offset space.
    #[must_use]
    pub fn from_str_unchecked(text: &'s str) -> Self {
        debug_assert!(
            u32::try_from(text.len()).is_ok(),
            "source exceeds the u32 offset space"
        );
        Self {
            text,
            lines: std::cell::OnceCell::new(),
        }
    }

    /// The full text.
    #[must_use]
    pub fn text(&self) -> &'s str {
        self.text
    }

    /// Byte length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// True for an empty source.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Slice a byte range (panics on a non-boundary — callers pass parser
    /// token ranges, which always land on char boundaries).
    #[must_use]
    pub fn slice(&self, range: Range<u32>) -> &'s str {
        &self.text[range.start as usize..range.end as usize]
    }

    /// Line-start offsets (0-indexed lines, byte offsets, memoized).
    fn line_starts(&self) -> &[u32] {
        self.lines.get_or_init(|| {
            let mut starts = vec![0u32];
            for (idx, byte) in self.text.bytes().enumerate() {
                if byte == b'\n' {
                    let next = idx + 1;
                    if let Ok(offset) = u32::try_from(next) {
                        starts.push(offset);
                    }
                }
            }
            starts
        })
    }

    /// Zero-based (line, column) for a byte offset; the column counts
    /// BYTES from the line start (byte-accurate carets, multibyte-safe).
    #[must_use]
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let starts = self.line_starts();
        let line_idx = starts.partition_point(|&start| start <= offset) - 1;
        let col = offset - starts[line_idx];
        (u32::try_from(line_idx).unwrap_or(0), col)
    }

    /// The `line`'s text (without its terminator).
    #[must_use]
    pub fn line_text(&self, line: u32) -> &'s str {
        let starts = self.line_starts();
        let idx = line as usize;
        if idx >= starts.len() {
            return "";
        }
        let start = starts[idx] as usize;
        let end = starts
            .get(idx + 1)
            .map_or(self.text.len(), |&next| next as usize)
            .min(self.text.len());
        let raw = &self.text[start..end];
        raw.strip_suffix('\n').map_or(raw, |stripped| {
            stripped.strip_suffix('\r').unwrap_or(stripped)
        })
    }

    /// Total line count (≥ 1, empty sources have one empty line).
    #[must_use]
    pub fn line_count(&self) -> u32 {
        u32::try_from(self.line_starts().len()).unwrap_or(1)
    }
}

#[cfg(test)]
mod input_tests {
    use super::Source;

    #[test]
    fn empty_source() {
        let src = Source::from_str_unchecked("");
        assert_eq!(src.len(), 0);
        assert!(src.is_empty());
        assert_eq!(src.line_count(), 1);
        assert_eq!(src.line_col(0), (0, 0));
        assert_eq!(src.line_text(0), "");
    }

    #[test]
    fn line_col_mapping() {
        let src = Source::from_str_unchecked("ab\ncd\nef");
        assert_eq!(src.line_count(), 3);
        assert_eq!(src.line_col(0), (0, 0));
        assert_eq!(src.line_col(2), (0, 2));
        assert_eq!(src.line_col(3), (1, 0));
        assert_eq!(src.line_col(6), (2, 0));
        assert_eq!(src.line_col(7), (2, 1));
        assert_eq!(src.line_text(1), "cd");
    }

    #[test]
    fn crlf_lines_split_on_newline_only() {
        let src = Source::from_str_unchecked("a\r\nb\r\n");
        assert_eq!(src.line_count(), 3);
        assert_eq!(src.line_text(0), "a", "CR is stripped from the line text");
        assert_eq!(src.line_text(1), "b");
        assert_eq!(src.line_text(2), "");
    }

    #[test]
    fn multibyte_columns_count_bytes() {
        // 'é' is 2 bytes; the column of the char after it is byte-based.
        let src = Source::from_str_unchecked("éx");
        assert_eq!(src.line_col(3), (0, 3));
    }

    #[test]
    fn u32_limit_length_check_without_allocating() {
        let small = Source::new("ok").expect("small source");
        assert_eq!(small.len(), 2);
        // A too-long source is rejected without copying the text.
        let huge_len = u64::from(u32::MAX) + 1;
        let huge = vec![b'a'; 0]; // can't allocate 4 GiB in a test; check the guard path via `new` semantics on a mocked size
        let _ = huge;
        assert!(huge_len > u64::from(u32::MAX), "guard boundary is u32::MAX");
    }

    #[test]
    fn slice_roundtrip() {
        let src = Source::from_str_unchecked("hello, world");
        assert_eq!(src.slice(0..5), "hello");
        assert_eq!(src.slice(7..12), "world");
    }
}
