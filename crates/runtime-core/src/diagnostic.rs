//! Token sets and the diagnostic side table (roadmap §7.10, issue 009).
//!
//! A [`TokenSet`] is a 192-bit bitset over token kinds (3 × `u64` runs) —
//! the parser's expected-set currency. A [`DiagTable`] holds deferred
//! diagnostic texts (the event stream carries only the index).

use std::fmt::Write as _;

/// A 192-bit set of token kinds (kinds 0..=191).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TokenSet {
    /// The three 64-bit runs (kind bit `k` → run `k / 64`, bit `k % 64`).
    pub runs: [u64; 3],
}

impl TokenSet {
    /// The empty set.
    pub const EMPTY: Self = Self { runs: [0; 3] };

    /// A singleton set.
    #[must_use]
    pub fn single(kind: u16) -> Self {
        let mut runs = [0u64; 3];
        if kind < 192 {
            runs[kind as usize / 64] = 1 << (kind % 64);
        }
        Self { runs }
    }

    /// Insert a kind (no-op above 191 — callers keep kinds dense).
    pub fn insert(&mut self, kind: u16) {
        if kind < 192 {
            self.runs[kind as usize / 64] |= 1 << (kind % 64);
        }
    }

    /// Membership test.
    #[must_use]
    pub fn contains(&self, kind: u16) -> bool {
        if kind >= 192 {
            return false;
        }
        self.runs[kind as usize / 64] & (1 << (kind % 64)) != 0
    }

    /// Union in place.
    pub fn union(&mut self, other: &Self) {
        for idx in 0..3 {
            self.runs[idx] |= other.runs[idx];
        }
    }

    /// Set intersection.
    #[must_use]
    pub const fn intersect(&self, other: &Self) -> Self {
        Self {
            runs: [
                self.runs[0] & other.runs[0],
                self.runs[1] & other.runs[1],
                self.runs[2] & other.runs[2],
            ],
        }
    }

    /// True when no kind is set.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.runs[0] == 0 && self.runs[1] == 0 && self.runs[2] == 0
    }

    /// Iterate the contained kinds ascending.
    #[must_use]
    pub fn kinds(&self) -> Vec<u16> {
        let mut out = Vec::new();
        for (run_idx, &run) in self.runs.iter().enumerate() {
            let mut bits = run;
            while bits != 0 {
                let bit = bits.trailing_zeros();
                out.push(u16::try_from(run_idx * 64 + bit as usize).unwrap_or(0));
                bits &= bits - 1;
            }
        }
        out
    }
}

impl std::ops::BitOr for TokenSet {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self {
            runs: [
                self.runs[0] | rhs.runs[0],
                self.runs[1] | rhs.runs[1],
                self.runs[2] | rhs.runs[2],
            ],
        }
    }
}

/// A deferred diagnostic: index-carried in the event stream, text here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// Human-readable message.
    pub message: String,
    /// Byte range the diagnostic points at.
    pub range: std::ops::Range<u32>,
}

/// The side table event `diag` indexes into.
#[derive(Debug, Clone, Default)]
pub struct DiagTable {
    entries: Vec<Diagnostic>,
}

impl DiagTable {
    /// Push a diagnostic, returning its stream index.
    pub fn push(&mut self, message: impl Into<String>, range: std::ops::Range<u32>) -> u32 {
        let idx = self.entries.len();
        if idx <= crate::events::MAX_DIAG as usize {
            self.entries.push(Diagnostic {
                message: message.into(),
                range,
            });
        }
        u32::try_from(idx).unwrap_or(crate::events::MAX_DIAG)
    }

    /// Fetch a diagnostic by index.
    #[must_use]
    pub fn get(&self, idx: u32) -> Option<&Diagnostic> {
        self.entries.get(idx as usize)
    }

    /// Number of diagnostics (may exceed what the stream references).
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no diagnostics.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Render diagnostic `idx` against `source` as a caret snippet.
    #[must_use]
    pub fn render(&self, idx: u32, source: &crate::input::Source<'_>) -> Option<String> {
        let diag = self.get(idx)?;
        let (line, col) = source.line_col(diag.range.start);
        let mut out = String::new();
        let _ = writeln!(out, "{}", diag.message);
        let _ = writeln!(out, "  --> input:{}:{}", line + 1, col + 1);
        let _ = writeln!(out, "   | {}", source.line_text(line));
        let mut caret = String::from("   | ");
        for _ in 0..col {
            caret.push(' ');
        }
        let width = diag.range.end.saturating_sub(diag.range.start).max(1);
        for _ in 0..width {
            caret.push('^');
        }
        let _ = writeln!(out, "{caret}");
        Some(out)
    }
}

#[cfg(test)]
mod diag_tests {
    use super::{DiagTable, TokenSet};

    #[test]
    fn empty_expected_set_omits_note() {
        let empty = TokenSet::EMPTY;
        assert!(empty.is_empty());
        assert_eq!(empty.kinds(), [] as [u16; 0]);
    }

    #[test]
    fn token_set_algebra_matches_naive_reference() {
        // Hand-computed union/intersection/contains on a few sets.
        let a = TokenSet::single(3);
        let mut b = TokenSet::single(70);
        b.insert(191);
        assert!(a.contains(3));
        assert!(!a.contains(4));
        assert!(b.contains(70));
        assert!(b.contains(191));
        assert!(
            !b.contains(192),
            "above the 192-bit space is never contained"
        );
        let both = a | b;
        assert!(both.contains(3) && both.contains(70) && both.contains(191));
        let overlap = both.intersect(&b);
        assert!(overlap.contains(70) && !overlap.contains(3));
        let mut acc = TokenSet::EMPTY;
        acc.insert(3);
        acc.union(&b);
        assert_eq!(acc, both);
        assert_eq!(both.kinds(), vec![3, 70, 191]);
    }

    #[test]
    fn render_includes_snippet() {
        let src = crate::input::Source::from_str_unchecked("let x = ;\n");
        let mut table = DiagTable::default();
        let idx = table.push("syntax error", 8..9);
        let rendered = table.render(idx, &src).expect("renders");
        assert!(rendered.contains("syntax error"), "{rendered}");
        assert!(rendered.contains("let x = ;"), "{rendered}");
        assert!(rendered.contains('^'), "{rendered}");
    }

    #[test]
    fn multibyte_line_renders_byte_based_caret() {
        let src = crate::input::Source::from_str_unchecked("é = 1\n");
        let mut table = DiagTable::default();
        // Point at the byte offset after the 2-byte 'é'.
        let idx = table.push("unexpected token", 2..3);
        let rendered = table.render(idx, &src).expect("renders");
        assert!(rendered.contains("unexpected token"));
    }

    #[test]
    fn side_table_roundtrip() {
        let mut table = DiagTable::default();
        let i0 = table.push("first", 0..1);
        let i1 = table.push("second", 2..3);
        assert_eq!(table.get(i0).expect("present").message, "first");
        assert_eq!(table.get(i1).expect("present").message, "second");
        assert!(table.get(9).is_none());
    }
}
