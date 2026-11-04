//! # incremental — the reparse engine (issue 030, §7.7)
//!
//! Edits land on a token snapshot; the engine repairs it with the
//! cheapest sufficient strategy and reports what it did:
//!
//! 1. **Edit normalization** — an [`Edit`] is a byte-range replace; the
//!    engine locates the affected token span.
//! 2. **Single-token relex fast path** — when the edit stays inside one
//!    token and [`Adapter::relex_one`] returns the same kind, that one
//!    token is patched and the suffix shifts by the length delta.
//! 3. **Balanced-block fragment reuse** — otherwise the enclosing
//!    bracket-balanced block is the reuse boundary: tokens outside the
//!    block carry over (range-shifted) after the **safety predicates**
//!    hold — the bytes outside the block are byte-identical old↔new and
//!    the boundary tokens' **context hashes** (kind + text + neighbor
//!    kinds) match. A failed predicate REJECTS the reuse and the whole
//!    document is rescanned (counted).
//! 4. **`ReusedSubtree` splicing** — the carried fragments are reported
//!    as old/new range pairs; the token list itself is the splice.
//! 5. **Changed ranges** — [`changed_ranges`] diffs the old and new
//!    token lists (the brute-force reference lives beside the tests).
//!
//! The equivalence gate (issue 030): seeded fuzz over the json+toml
//! packs holds `incremental_tokens(edit(x)) == fresh_scan(x)` and the
//! fresh tree renders byte-faithfully — every run, no exceptions.

pub mod ranges;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;

/// One edit: replace `source[range]` with `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// The replaced byte range (start == end ⇒ insertion).
    pub range: Range<u32>,
    /// The inserted text.
    pub text: String,
}

/// What the engine has done so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stats {
    /// Edits applied.
    pub edits: usize,
    /// Single-token relex fast-path hits.
    pub fast_path_relex: usize,
    /// Reuses rejected by the safety predicates (full rescan fallback).
    pub reuses_rejected: usize,
    /// Fragments (prefix + suffix spans) carried over verbatim.
    pub fragments_reused: usize,
    /// Tokens total after the last edit.
    pub tokens: usize,
}

/// A carried-over fragment: the old byte range and where it sits now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReusedSubtree {
    /// The fragment's byte range in the OLD source.
    pub old_range: Range<u32>,
    /// The fragment's byte range in the NEW source.
    pub new_range: Range<u32>,
}

/// The engine result for one edit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    /// The repaired token list over the new source.
    pub tokens: Vec<(u16, Range<u32>)>,
    /// The cumulative stats after this edit.
    pub stats: Stats,
    /// The fragments carried over by this edit.
    pub reused: Vec<ReusedSubtree>,
    /// The changed byte ranges vs the previous snapshot.
    pub changed: Vec<Range<u32>>,
}

/// The pack hook the engine drives (token-level; no tree knowledge).
pub trait Adapter {
    /// Full scan of the source into clean `(kind, range)` tokens.
    fn scan(&self, source: &str) -> Vec<(u16, Range<u32>)>;
    /// Single-token rescan at `offset` — the fast path. `None` when the
    /// pack has no single-token entry point.
    fn relex_one(&self, source: &str, offset: u32) -> Option<(u16, Range<u32>)> {
        let _ = (source, offset);
        None
    }
}

impl<A: Adapter> Adapter for &A {
    fn scan(&self, source: &str) -> Vec<(u16, Range<u32>)> {
        (*self).scan(source)
    }

    fn relex_one(&self, source: &str, offset: u32) -> Option<(u16, Range<u32>)> {
        (*self).relex_one(source, offset)
    }
}

/// The incremental engine over one adapter.
pub struct Engine<A: Adapter> {
    adapter: A,
    source: String,
    tokens: Vec<(u16, Range<u32>)>,
    stats: Stats,
}

impl<A: Adapter> Engine<A> {
    /// Scan the initial source.
    #[must_use]
    pub fn new(adapter: A, source: &str) -> Self {
        let tokens = adapter.scan(source);
        let stats = Stats {
            tokens: tokens.len(),
            ..Stats::default()
        };
        Self {
            adapter,
            source: source.to_owned(),
            tokens,
            stats,
        }
    }

    /// The current token list.
    #[must_use]
    pub fn tokens(&self) -> &[(u16, Range<u32>)] {
        &self.tokens
    }

    /// The cumulative stats.
    #[must_use]
    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Apply one edit and repair the token list.
    pub fn edit(&mut self, edit: &Edit, new_source: &str) -> Snapshot {
        let old = std::mem::take(&mut self.tokens);
        let old_source = std::mem::take(&mut self.source);
        let old_len = u32::try_from(old_source.len()).unwrap_or(u32::MAX);
        let new_len = u32::try_from(new_source.len()).unwrap_or(u32::MAX);
        let delta = i64::from(new_len) - i64::from(old_len);

        self.stats.edits += 1;

        // The affected token span: first token ending after edit.start,
        // last token starting before edit.end.
        let anchor = old
            .iter()
            .position(|(_, r)| r.end > edit.range.start)
            .unwrap_or(old.len());
        let last = old
            .iter()
            .rposition(|(_, r)| r.start < edit.range.end)
            .map_or(anchor, |idx| idx + 1);

        // --- Fast path: one token, same kind after a relex -------------
        if last == anchor + 1 {
            if let Some(snapshot) =
                self.try_fast_path(&old, anchor, delta, old_len, new_len, new_source)
            {
                return self.commit(
                    snapshot.tokens,
                    snapshot.reused,
                    snapshot.changed,
                    new_source,
                );
            }
        }

        // --- Balanced-block reuse --------------------------------------
        let block = enclosing_block(&old, anchor);
        let (prefix, suffix) = (block.start, block.end.min(old.len()));
        let block_bytes = block_byte_span(&old, prefix, suffix, old_len);
        let tail_bytes = old_len - block_bytes.end;

        // Safety predicates: (a) the bytes outside the block are
        // byte-identical old↔new; (b) the boundary tokens' context hashes
        // match (kind + text + neighbor kinds).
        let prefix_ok = old_source.as_bytes().get(..block_bytes.start as usize)
            == new_source.as_bytes().get(..block_bytes.start as usize);
        let suffix_ok = tail_bytes == 0
            || old_source.as_bytes().get(block_bytes.end as usize..)
                == new_source.as_bytes().get((new_len - tail_bytes) as usize..);
        let boundary_ok =
            boundary_hashes_match(&old, prefix, suffix, &old_source, new_source, delta);

        if prefix_ok && suffix_ok && boundary_ok {
            let new_block_end = new_len - tail_bytes;
            let fresh = self.adapter.scan(new_source);
            let mut tokens = Vec::with_capacity(fresh.len());
            tokens.extend(old[..prefix].iter().cloned());
            tokens.extend(
                fresh
                    .iter()
                    .filter(|(_, r)| r.start >= block_bytes.start && r.end <= new_block_end)
                    .cloned(),
            );
            for (kind, range) in &old[suffix..] {
                tokens.push((*kind, shift_range(range, delta)));
            }
            let mut reused = Vec::new();
            if prefix > 0 {
                reused.push(ReusedSubtree {
                    old_range: 0..block_bytes.start,
                    new_range: 0..block_bytes.start,
                });
                self.stats.fragments_reused += 1;
            }
            if suffix < old.len() {
                reused.push(ReusedSubtree {
                    old_range: block_bytes.end..old_len,
                    new_range: new_block_end..new_len,
                });
                self.stats.fragments_reused += 1;
            }
            self.stats.tokens = tokens.len();
            let changed = ranges::changed_ranges(&old, &tokens);
            return self.commit(tokens, reused, changed, new_source);
        }

        // --- Rejected: full rescan --------------------------------------
        self.stats.reuses_rejected += 1;
        let fresh = self.adapter.scan(new_source);
        self.stats.tokens = fresh.len();
        let changed = ranges::changed_ranges(&old, &fresh);
        self.commit(fresh, Vec::new(), changed, new_source)
    }

    /// The single-token relex fast path: the edit fully absorbed inside
    /// one token (same kind, same start, end shifted by the length
    /// delta) plus a one-token MARGIN check — the next token must relex
    /// identically (or there must be nothing after it), so an edit that
    /// changes the tail's lexing falls through. Returns `None` when the
    /// fast path does not apply.
    fn try_fast_path(
        &mut self,
        old: &[(u16, Range<u32>)],
        anchor: usize,
        delta: i64,
        old_len: u32,
        new_len: u32,
        new_source: &str,
    ) -> Option<Snapshot> {
        let token = old.get(anchor)?;
        let relexed = self.adapter.relex_one(new_source, token.1.start)?;
        let expected_end = i64::from(token.1.end) + delta;
        let margin_ok = match old.get(anchor + 1) {
            Some(next) => {
                let margin = self.adapter.relex_one(new_source, relexed.1.end);
                margin.is_some_and(|m| {
                    m.0 == next.0 && m.1.start == shift_range(&next.1.clone(), delta).start
                })
            }
            None => self.adapter.relex_one(new_source, relexed.1.end).is_none(),
        };
        if !(margin_ok
            && relexed.0 == token.0
            && relexed.1.start == token.1.start
            && i64::from(relexed.1.end) == expected_end)
        {
            return None;
        }
        let mut tokens = Vec::with_capacity(old.len());
        tokens.extend(old[..anchor].iter().cloned());
        tokens.push(relexed);
        for (kind, range) in &old[anchor + 1..] {
            tokens.push((*kind, shift_range(range, delta)));
        }
        self.stats.fast_path_relex += 1;
        self.stats.tokens = tokens.len();
        let changed = ranges::changed_ranges(old, &tokens);
        let reused = vec![ReusedSubtree {
            old_range: 0..old_len,
            new_range: 0..new_len,
        }];
        self.stats.fragments_reused += 1;
        Some(Snapshot {
            tokens,
            stats: self.stats,
            reused,
            changed,
        })
    }

    fn commit(
        &mut self,
        tokens: Vec<(u16, Range<u32>)>,
        reused: Vec<ReusedSubtree>,
        changed: Vec<Range<u32>>,
        new_source: &str,
    ) -> Snapshot {
        self.tokens.clone_from(&tokens);
        new_source.clone_into(&mut self.source);
        Snapshot {
            tokens,
            stats: self.stats,
            reused,
            changed,
        }
    }
}

fn shift_range(range: &Range<u32>, delta: i64) -> Range<u32> {
    let shift = |v: u32| -> u32 {
        let v = i64::from(v) + delta;
        u32::try_from(v).unwrap_or(0)
    };
    shift(range.start)..shift(range.end)
}

/// The byte span of tokens `[prefix..suffix)` in the old source. When
/// the block reaches the end of the token list, the span extends to the
/// DOCUMENT end (trivia and garbage after the last token belong to the
/// rescan, not to a phantom tail).
fn block_byte_span(
    tokens: &[(u16, Range<u32>)],
    prefix: usize,
    suffix: usize,
    old_len: u32,
) -> Range<u32> {
    // An empty prefix means the rescan owns the document head; an empty
    // suffix (block to the end) means it owns the tail.
    let start = if prefix == 0 {
        0
    } else {
        tokens.get(prefix).map_or(old_len, |(_, r)| r.start)
    };
    let end = if suffix >= tokens.len() {
        old_len
    } else if suffix > prefix {
        tokens.get(suffix - 1).map_or(start, |(_, r)| r.end)
    } else {
        start
    };
    start..end.max(start)
}

/// The boundary context-hash predicate: the tokens just inside the reuse
/// boundaries must keep their kind and text in the new source.
fn boundary_hashes_match(
    old: &[(u16, Range<u32>)],
    prefix: usize,
    suffix: usize,
    old_source: &str,
    new_source: &str,
    delta: i64,
) -> bool {
    let hash_at = |tokens: &[(u16, Range<u32>)], idx: usize, source: &str, delta: i64| {
        let (kind, range) = &tokens[idx];
        let shifted = shift_range(range, delta);
        let text = source
            .get(shifted.start as usize..shifted.end as usize)
            .unwrap_or("");
        let prev = if idx > 0 {
            Some(tokens[idx - 1].0)
        } else {
            None
        };
        let next = tokens.get(idx + 1).map(|(k, _)| *k);
        context_hash(*kind, text, prev, next)
    };
    if prefix > 0 {
        let idx = prefix - 1;
        // The prefix is byte-identical, so the boundary token's old range
        // is valid in the new source with delta 0.
        if hash_at(old, idx, old_source, 0) != hash_at(old, idx, new_source, 0) {
            return false;
        }
    }
    if suffix < old.len() {
        // The suffix shifts by delta; hash both sides at their home
        // offsets (old at 0, new at delta).
        if hash_at(old, suffix, old_source, 0) != hash_at(old, suffix, new_source, delta) {
            return false;
        }
    }
    true
}

/// The enclosing bracket-balanced block around token `anchor`: the
/// closest opener (before) with its matching closer (after). The
/// document `[0, len)` is the fallback. Bracket kinds are the C-style
/// `{ [ (` family the C-family/JSON/TOML packs share.
#[must_use]
pub fn enclosing_block(tokens: &[(u16, Range<u32>)], anchor: usize) -> Range<usize> {
    const OPENERS: &[u16] = &[123, 91, 40]; // { [ (
    const CLOSERS: &[u16] = &[125, 93, 41]; // } ] )
    let opener_pos = |k: u16| OPENERS.iter().position(|o| *o == k);
    let closer_pos = |k: u16| CLOSERS.iter().position(|c| *c == k);
    // Walk back: an opener at depth 0 is the block start.
    let mut depth = 0_i32;
    for idx in (0..anchor).rev() {
        let kind = tokens[idx].0;
        if let Some(pos) = opener_pos(kind) {
            let _ = pos;
            if depth == 0 {
                let start = idx;
                // Matching closer after the anchor.
                let mut depth2 = 0_i32;
                for (jdx, (kind2, _)) in tokens.iter().enumerate().skip(start) {
                    if opener_pos(*kind2).is_some() {
                        depth2 += 1;
                    } else if closer_pos(*kind2).is_some() {
                        depth2 -= 1;
                        if depth2 == 0 {
                            return start..(jdx + 1);
                        }
                    }
                }
                return start..tokens.len();
            }
            depth -= 1;
        } else if closer_pos(kind).is_some() {
            depth += 1;
        }
    }
    0..tokens.len()
}

/// The context hash: kind + text + neighbor kinds (the reuse safety
/// predicate's fingerprint).
#[must_use]
pub fn context_hash(kind: u16, text: &str, prev_kind: Option<u16>, next_kind: Option<u16>) -> u64 {
    let mut hasher = DefaultHasher::new();
    kind.hash(&mut hasher);
    text.hash(&mut hasher);
    prev_kind.hash(&mut hasher);
    next_kind.hash(&mut hasher);
    hasher.finish()
}
