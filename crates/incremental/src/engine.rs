//! The reuse engine (issue 030): damage classification, the margin
//! window fast path with safety predicates, splicing, and the reuse
//! statistics the tests gate on.

use std::ops::Range;

use crate::diff::{changed_ranges, fnv1a};
use crate::edit::Edit;

/// A token stream source: `(kind, byte range)` pairs tiling the input.
pub trait Lexer {
    /// Lex `source` fully (clean or recovered — error regions ride
    /// their own tokens, exactly like the packs' side tables).
    fn lex(&self, source: &str) -> Vec<(u16, Range<u32>)>;
}

/// A lexer backed by any whole-document parse: the ranges side table IS
/// the token stream (this is how the packs plug in).
pub struct ParseLexer<F: Fn(&str) -> Vec<(u16, Range<u32>)>> {
    /// The pack's `parse_events`-style range extractor.
    pub parse: F,
}

impl<F: Fn(&str) -> Vec<(u16, Range<u32>)>> Lexer for ParseLexer<F> {
    fn lex(&self, source: &str) -> Vec<(u16, Range<u32>)> {
        (self.parse)(source)
    }
}

/// Hash of a token's lexical context: `(prev kind, prev text, own kind,
/// own text, next kind, next text)` — two tokens with equal context
/// hashes are interchangeable for reuse purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextHash(pub u64);

impl ContextHash {
    /// Hash the flanks around token `index` in `tokens` over `source`.
    #[must_use]
    pub fn around(tokens: &[(u16, Range<u32>)], source: &str, index: usize) -> Option<Self> {
        let (kind, range) = tokens.get(index)?;
        let text = |r: &Range<u32>| {
            let start = (r.start as usize).min(source.len());
            let end = (r.end as usize).min(source.len()).max(start);
            &source[start..end]
        };
        let prev = index
            .checked_sub(1)
            .and_then(|p| tokens.get(p))
            .map_or(0, |(k, r)| {
                fnv1a(&[u8::try_from(*k).unwrap_or(0), 0]).wrapping_add(fnv1a(text(r).as_bytes()))
            });
        let next = tokens.get(index + 1).map_or(0, |(k, r)| {
            fnv1a(&[u8::try_from(*k).unwrap_or(0), 1]).wrapping_add(fnv1a(text(r).as_bytes()))
        });
        Some(Self(
            fnv1a(&[u8::try_from(*kind).unwrap_or(0), 2])
                .wrapping_add(fnv1a(text(range).as_bytes()))
                .wrapping_mul(3)
                .wrapping_add(prev)
                .wrapping_add(next),
        ))
    }
}

/// The reuse decision point: the position a fragment would splice back
/// at, plus the context hash its safety predicates compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReuseCursor {
    /// Token index in the old stream the fragment lands at.
    pub index: usize,
    /// The context hash the fragment must reproduce.
    pub context: ContextHash,
}

/// Fast-path and reuse accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Fast-path splices (no full relex).
    pub fast_path_hits: usize,
    /// Fast-path candidates the safety predicates rejected.
    pub fast_path_rejections: usize,
    /// Tokens surviving the edit untouched (before/after classes).
    pub reused_tokens: usize,
    /// Tokens the fallback relex produced.
    pub fresh_tokens: usize,
}

/// Why a fast-path candidate fell back to the full relex.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// More than one token overlapped the edited range.
    MultiTokenDamage,
    /// The window relex did not tile the window exactly.
    WindowNotTiled,
    /// The relexed window's flank kinds disagree with the old context.
    ContextMismatch,
}

/// The engine's product: the new token stream, the stats, and the
/// changed byte ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The new token stream (equal to a full lex of the new source).
    pub tokens: Vec<(u16, Range<u32>)>,
    /// Fast-path / reuse accounting.
    pub stats: Stats,
    /// The first safety predicate that rejected the fast path, if any.
    pub rejection: Option<Rejection>,
    /// Byte ranges of the new source that differ from the old stream.
    pub changed: Vec<Range<usize>>,
}

/// The incremental engine over a [`Lexer`].
pub struct Engine<'l> {
    /// The token oracle (a pack's parse ranges, a standalone lexer…).
    pub lexer: &'l dyn Lexer,
}

impl Engine<'_> {
    /// Build the new token stream for `old_tokens` after `edit`.
    #[must_use]
    pub fn incremental_edit(
        &self,
        old_source: &str,
        old_tokens: &[(u16, Range<u32>)],
        edit: Edit,
    ) -> Outcome {
        let edit = edit.normalize(old_source.len());
        let new_source = edit.apply(old_source);
        let delta = edit.delta();

        // Byte-position split: the last token ending at/before the edit
        // (left flank) and the first starting at/after it (right flank).
        // Everything strictly between is the damaged window.
        let left_index = old_tokens
            .iter()
            .rposition(|(_, r)| (r.end as usize) <= edit.start);
        let right_index = old_tokens
            .iter()
            .position(|(_, r)| (r.start as usize) >= edit.end);
        let window: Range<usize> =
            left_index.map_or(0, |i| i + 1)..right_index.unwrap_or(old_tokens.len());

        // Fast path: at most one damaged token, relex the margin window.
        if window.end - window.start <= 1 {
            match self.try_fast_path(
                old_source,
                &new_source,
                old_tokens,
                left_index,
                right_index,
                delta,
            ) {
                Ok(tokens) => {
                    let changed = changed_ranges(old_tokens, old_source, &tokens, &new_source);
                    return Outcome {
                        stats: Stats {
                            fast_path_hits: 1,
                            reused_tokens: old_tokens.len() - window.len(),
                            ..Stats::default()
                        },
                        rejection: None,
                        changed,
                        tokens,
                    };
                }
                Err(rejection) => {
                    let tokens = self.fallback(&new_source);
                    let changed = changed_ranges(old_tokens, old_source, &tokens, &new_source);
                    return Outcome {
                        stats: Stats {
                            fast_path_rejections: 1,
                            fresh_tokens: tokens.len(),
                            ..Stats::default()
                        },
                        rejection: Some(rejection),
                        changed,
                        tokens,
                    };
                }
            }
        }

        // Fallback: multi-token damage — full relex; count survivors.
        let tokens = self.fallback(&new_source);
        let mut reused = 0_usize;
        for (index, (kind, range)) in old_tokens.iter().enumerate() {
            if window.start <= index && index < window.end {
                continue;
            }
            let shifted = shift_range(range, delta);
            if tokens
                .iter()
                .any(|(k, r)| *k == *kind && r.start == shifted.start && r.end == shifted.end)
            {
                reused += 1;
            }
        }
        let changed = changed_ranges(old_tokens, old_source, &tokens, &new_source);
        Outcome {
            stats: Stats {
                reused_tokens: reused,
                fresh_tokens: tokens.len(),
                ..Stats::default()
            },
            rejection: Some(Rejection::MultiTokenDamage),
            changed,
            tokens,
        }
    }

    /// The margin-window fast path. The window is the damaged zone
    /// between the flanks; the safety relex covers ONE EXTRA old token
    /// on each side, and the splice is accepted only when the wide
    /// window tiles and its outer tokens keep their old kinds AND texts
    /// (an adjacent edit that merges into a flank changes its text and
    /// is rejected — the context-hash discipline).
    #[allow(clippy::too_many_lines)] // one predicate ladder
    fn try_fast_path(
        &self,
        old_source: &str,
        new_source: &str,
        old_tokens: &[(u16, Range<u32>)],
        left_index: Option<usize>,
        right_index: Option<usize>,
        delta: i64,
    ) -> Result<Vec<(u16, Range<u32>)>, Rejection> {
        // One extra token of margin beyond the flanks.
        let outer_left = left_index.and_then(|i| i.checked_sub(1));
        let outer_right = right_index
            .filter(|i| *i + 1 < old_tokens.len())
            .map(|i| i + 1);
        let lo = outer_left.map_or(0, |i| old_tokens[i].1.start as usize);
        let old_hi = outer_right.map_or(old_source.len(), |i| old_tokens[i].1.end as usize);
        let hi_new = usize::try_from(i64::from(u32::try_from(old_hi).unwrap_or(u32::MAX)) + delta)
            .map_err(|_| Rejection::WindowNotTiled)?;
        if hi_new > new_source.len() || lo > hi_new {
            return Err(Rejection::WindowNotTiled);
        }
        let text = &new_source[lo..hi_new];
        let relexed = self.lexer.lex(text);
        // Predicate 1: the wide window tiles exactly.
        let tiled = relexed
            .last()
            .is_some_and(|(_, r)| r.end as usize == text.len())
            || (relexed.is_empty() && text.is_empty());
        if !tiled {
            return Err(Rejection::WindowNotTiled);
        }
        // Predicate 2: the fragment must be error-free — an ERROR token
        // inside the window means the out-of-context fragment parse
        // could not interpret the region (e.g. a `]` that only makes
        // sense with the preceding header).
        if relexed.iter().any(|(k, _)| *k == runtime_core::ERROR_KIND) {
            return Err(Rejection::ContextMismatch);
        }
        // Predicate 3: the outer tokens keep their kinds AND texts (the
        // context hash made concrete — a merged/extended flank fails).
        if let Some(oi) = outer_left {
            let (okind, orange) = &old_tokens[oi];
            match relexed.first() {
                Some((k, r))
                    if *k == *okind && flank_text(text, r) == flank_text(old_source, orange) =>
                {
                    let _ = r;
                }
                _ => return Err(Rejection::ContextMismatch),
            }
        }
        if let Some(oi) = outer_right {
            let (okind, orange) = &old_tokens[oi];
            match relexed.last() {
                Some((k, r))
                    if *k == *okind && flank_text(text, r) == flank_text(old_source, orange) =>
                {
                    // the outer right token must also be the LAST one
                }
                _ => return Err(Rejection::ContextMismatch),
            }
            if relexed.is_empty() {
                return Err(Rejection::ContextMismatch);
            }
        }
        // Splice: everything before the wide window, then the WHOLE
        // wide relex (its outer tokens were just verified identical to
        // the old ones), then the shifted tail.
        let mut out: Vec<(u16, Range<u32>)> = Vec::with_capacity(old_tokens.len() + 2);
        for (kind, range) in &old_tokens[..outer_left.unwrap_or(0)] {
            out.push((*kind, range.clone()));
        }
        for (kind, range) in &relexed {
            let start = u32::try_from(range.start as usize + lo).unwrap_or(u32::MAX);
            let end = u32::try_from(range.end as usize + lo).unwrap_or(u32::MAX);
            out.push((*kind, start..end));
        }
        // The tail resumes past the wide window's coverage (old_hi).
        let tail_start = outer_right.map_or_else(
            || {
                old_tokens
                    .iter()
                    .position(|(_, r)| (r.start as usize) >= old_hi)
                    .unwrap_or(old_tokens.len())
            },
            |i| i + 1,
        );
        for (kind, range) in &old_tokens[tail_start..] {
            out.push((*kind, shift_range(range, delta)));
        }
        Ok(out)
    }

    /// The full relex fallback.
    fn fallback(&self, new_source: &str) -> Vec<(u16, Range<u32>)> {
        self.lexer.lex(new_source)
    }
}

/// The text of a token range, clipped to the source.
fn flank_text<'a>(source: &'a str, r: &Range<u32>) -> &'a str {
    let start = (r.start as usize).min(source.len());
    let end = (r.end as usize).min(source.len()).max(start);
    &source[start..end]
}

/// Shift a range by the edit delta (positions past the edit move).
#[must_use]
pub fn shift_range(range: &Range<u32>, delta: i64) -> Range<u32> {
    let shift = |v: u32| -> u32 {
        let shifted = i64::from(v) + delta;
        u32::try_from(shifted).unwrap_or(0)
    };
    shift(range.start)..shift(range.end)
}

/// Tree-granularity reuse: total bytes of green-tree tokens fully
/// outside the changed ranges (the `ReusedSubtree` accounting).
#[must_use]
pub fn reused_subtree_bytes(tokens: &[(u16, Range<u32>)], changed: &[Range<usize>]) -> usize {
    tokens
        .iter()
        .filter(|(_, range)| {
            let start = range.start as usize;
            let end = range.end as usize;
            !changed.iter().any(|c| c.start < end && start < c.end)
        })
        .map(|(_, range)| (range.end - range.start) as usize)
        .sum()
}
