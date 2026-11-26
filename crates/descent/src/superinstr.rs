//! # superinstr — PGO-driven superinstruction mining (§10.2, §12.2)
//!
//! Data-driven superinstruction synthesis for the descent interpreter
//! (§1.3 #16), the interpreter half of the O3 tier's "superinstruction
//! fusion" (§7.10): a profile corpus — the §5.2 `profile/` idea, the same
//! corpora the `syntact pgo` pipeline trains on — is mined for **hot
//! adjacent token-shift pairs**, and the engine executes every maximal
//! token-shift run whose consecutive pairs are hot as ONE fused dispatch
//! instead of per-token pattern recursion. This is the interpreter's
//! analogue of the generated parser's `parse_identifier_equals()` fusion
//! (§10.2): fused token-shift sequences, mined once from the profile,
//! conformance-gated (the fused run's output MUST equal the generic
//! interpreter's byte for byte — pinned by `tests/superinstr.rs`).
//!
//! # Design (the invariants the tests pin)
//!
//! - **Data-driven selection**: only pairs observed ≥ `min_count` times in
//!   the profile feeds are mined — superinstructions come "only from PGO
//!   hot paths" (§10.2), never from a hand-written whitelist.
//! - **Deterministic mining**: identical corpora produce an identical
//!   [`SuperSet`] — entries are ordered count-descending with kind-ordered
//!   tie-breaks and capped at `max_entries`, so the mined table is a
//!   reviewable, diff-stable artifact (the ADR-013 discipline applied to
//!   the profile output).
//! - **Composition, not enumeration**: the table stores hot *pairs*; the
//!   engine fuses any maximal run of consecutive `Pat::Tok` shifts whose
//!   every adjacent pair is hot — a run of any length composes from the
//!   mined pairs without a second mining pass.
//! - **Event identity by construction**: the fused run consumes tokens
//!   through the engine's ordinary `bump` path and keeps the `Seq`
//!   committed-prefix discipline on mid-run mismatch, so the fused output
//!   IS the generic output (the conformance gate), for valid and
//!   malformed input alike.

use std::collections::HashMap;
use std::ops::Range;

/// One mined superinstruction: an adjacent token-kind shift pair and the
/// number of times the corpus observed it (`first` immediately followed by
/// `second` in the profile feed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuperEntry {
    /// The adjacent shift pair, in feed order.
    pub kinds: [u16; 2],
    /// Observations across the profile corpora.
    pub count: u64,
}

/// The mined superinstruction table: the PGO output the engine's fused
/// fast path consumes (the empty set is the generic interpreter — see
/// [`super::parse_events`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SuperSet {
    /// Mined entries, count-descending with kind-ordered tie-breaks
    /// (the diff-stable order).
    entries: Vec<SuperEntry>,
    /// The gating pairs, packed `(first << 16) | second`, sorted and
    /// deduped — one `u32` compare per `is_hot_pair` probe.
    pairs: Vec<u32>,
}

impl SuperSet {
    /// The mined entries, hottest first (the reviewable mining report).
    #[must_use]
    pub fn entries(&self) -> &[SuperEntry] {
        &self.entries
    }

    /// Whether the adjacent shift `(first, second)` is hot enough to fuse.
    #[must_use]
    pub fn is_hot_pair(&self, first: u16, second: u16) -> bool {
        self.pairs
            .binary_search(&(pack_pair(first, second)))
            .is_ok()
    }

    /// The number of mined entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing was mined (the generic-interpreter table).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Mine hot adjacent token-shift pairs from profile feeds (§10.2: "only
/// from PGO hot paths"). A pair enters the table when the corpora observe
/// it at least `min_count` times; the table keeps the `max_entries` hottest
/// pairs, count-descending with kind-ordered tie-breaks — identical inputs
/// mine an identical [`SuperSet`].
///
/// Adjacency is feed adjacency: two clean tokens next to each other in the
/// feed (trivia never rides the feed, ADR-008, so a hot pair is exactly a
/// hot token shift).
#[must_use]
pub fn mine(feeds: &[&[(u16, Range<u32>)]], min_count: u64, max_entries: usize) -> SuperSet {
    let mut counts: HashMap<(u16, u16), u64> = HashMap::new();
    for feed in feeds {
        for pair in feed.windows(2) {
            *counts.entry((pair[0].0, pair[1].0)).or_insert(0) += 1;
        }
    }
    let mut entries: Vec<SuperEntry> = counts
        .into_iter()
        .filter(|(_, count)| *count >= min_count)
        .map(|((first, second), count)| SuperEntry {
            kinds: [first, second],
            count,
        })
        .collect();
    entries.sort_by(|a, b| b.count.cmp(&a.count).then(a.kinds.cmp(&b.kinds)));
    entries.truncate(max_entries);
    let mut pairs: Vec<u32> = entries
        .iter()
        .map(|entry| pack_pair(entry.kinds[0], entry.kinds[1]))
        .collect();
    pairs.sort_unstable();
    pairs.dedup();
    SuperSet { entries, pairs }
}

/// Pack one adjacent shift into the table's one-`u32`-compare key.
fn pack_pair(first: u16, second: u16) -> u32 {
    (u32::from(first) << 16) | u32::from(second)
}

#[cfg(test)]
mod tests {
    use super::{SuperSet, mine};

    /// The sorted-pair lookup must agree with the entry list on every
    /// entry and disagree off-table.
    #[test]
    fn lookup_matches_entries() {
        let feeds: Vec<Vec<(u16, std::ops::Range<u32>)>> = vec![
            (0..8)
                .map(|i| {
                    let kind = if i % 2 == 0 { 1 } else { 2 };
                    (kind, i..i + 1)
                })
                .collect(),
        ];
        let refs: Vec<&[(u16, std::ops::Range<u32>)]> =
            feeds.iter().map(std::vec::Vec::as_slice).collect();
        let set = mine(&refs, 1, 64);
        assert_eq!(set.len(), 2, "(1,2) and (2,1) both occur");
        assert!(set.is_hot_pair(1, 2));
        assert!(set.is_hot_pair(2, 1));
        assert!(!set.is_hot_pair(2, 2), "off-table pair is cold");
        // The report order: equal counts (4 and 3) — (1,2) first.
        assert_eq!(set.entries()[0].kinds, [1, 2]);
        assert_eq!(set.entries()[0].count, 4);
        assert_eq!(set.entries()[1].kinds, [2, 1]);
    }

    /// The empty set is the generic interpreter: nothing is hot.
    #[test]
    fn empty_set_is_cold() {
        let set = SuperSet::default();
        assert!(set.is_empty());
        assert!(!set.is_hot_pair(0, 1));
    }

    /// Identical corpora mine an identical table (the determinism
    /// contract).
    #[test]
    fn mining_is_deterministic() {
        let feed: Vec<(u16, std::ops::Range<u32>)> = (0..12)
            .map(|i| {
                let kind = u16::try_from(i % 3).unwrap_or(0);
                (kind, i..i + 1)
            })
            .collect();
        let one = mine(&[&feed], 1, 64);
        let two = mine(&[&feed], 1, 64);
        assert_eq!(one, two);
        assert_eq!(one.entries()[0].count, 4, "kinds cycle 0,1,2,0,1,2…");
    }
}
