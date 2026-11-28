//! The island runtime over the descent feed (roadmap §7.4, §12.2): the
//! GLR island executor's **descent-engine integration**.
//!
//! §12.2's status note: *"the island executor — the arena GSS with
//! fork/merge caps and the descent-engine integration — does not"*. The
//! executor lives in `lr-shadow::island` over the automaton's [`Look`]
//! alphabet; this module is the engine-side half: the executor rides the
//! **descent engine's own feed contract** — the exact
//! `&[(u16 kind, Range<u32>)]` slice [`crate::parse_events`] consumes — through
//! a [`LookMap`] bridging engine kinds into automaton terminals, with no
//! re-lex and no scanner involvement. One feed, three backends: the
//! descent verdict, the table verdict ([`lr_shadow::driver::parse`]) and
//! the island verdict all answer over the same tokens, which is what
//! `tests/island.rs` gates: the backends agree where the grammar is
//! deterministic, and on an island-classified region the executor accepts
//! where the shift-wins-resolved table dead-ends — rescuing the table
//! verdict to match the engine's own backtracking result.
//!
//! The [`LookMap`] mirrors the cross-backend oracle's convention
//! (`crates/cli/src/testall.rs`): every declared token owns exactly one
//! engine kind, so the kind → terminal map is total over the real kinds;
//! trivia kinds never enter a feed and scanner-unmatched `bad` kinds ride
//! the stranger terminal — outside the token table, no action can exist,
//! which is exactly the executor's rejection of input the scanner could
//! not match.

use std::ops::Range;

use lr_shadow::Automaton;
use lr_shadow::island::{self, IslandCaps, IslandRun};
use lr_shadow::lower::Look;

/// The stranger terminal: outside the token-id space and not the
/// pseudo-EOF, so no automaton action can ever exist for it (the
/// oracle's convention).
pub const STRANGER: Look = Look(u32::MAX - 1);

/// The engine kind → automaton terminal map, total over the engine's
/// dense kind space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookMap {
    looks: Vec<Look>,
}

impl LookMap {
    /// Build the map from the lowering's artifacts: `kinds` is
    /// `descent_lower::Lowered::kind_of_token` (token index → engine
    /// kind), `trivia` the parallel per-token trivia flags, `bad_kind`
    /// the unmatched-character sentinel kind ([`descent_lower::Lowered::
    /// bad_kind`]). Declared tokens map to their terminal; trivia and the
    /// sentinel ride [`STRANGER`] (trivia never enters a feed, and an
    /// unmatched character has no grammar action — the verdicts align on
    /// rejection).
    #[must_use]
    pub fn of_grammar(kinds: &[u16], trivia: &[bool], bad_kind: u16) -> LookMap {
        let span = usize::from(bad_kind) + 1;
        let mut looks = vec![STRANGER; span];
        for (index, &kind) in kinds.iter().enumerate() {
            if trivia.get(index).copied().unwrap_or(false) {
                // Trivia never enters a feed (`Lowered::feed` drops it);
                // the slot exists so the map is total over kinds.
                continue;
            }
            let slot = &mut looks[usize::from(kind)];
            *slot = Look(u32::try_from(index).unwrap_or(u32::MAX - 2));
        }
        looks[usize::from(bad_kind)] = STRANGER;
        LookMap { looks }
    }

    /// The terminal for an engine kind (the stranger terminal for kinds
    /// past the token table — hand-built feeds stay total).
    #[must_use]
    pub fn of(&self, kind: u16) -> Look {
        self.looks
            .get(usize::from(kind))
            .copied()
            .unwrap_or(STRANGER)
    }
}

/// Run the island executor over a descent feed: the engine's own
/// `&[(u16 kind, Range<u32>)]` slice (what [`crate::parse_events`] consumes),
/// kinds bridged through `map`, executed under `caps` over the shadow
/// automaton. The island verdict answers over the same tokens the engine
/// ran on — no re-lex, no scanner involvement.
#[must_use]
pub fn island_over_feed(
    feed: &[(u16, Range<u32>)],
    map: &LookMap,
    automaton: &Automaton,
    caps: &IslandCaps,
) -> IslandRun {
    let tokens: Vec<Look> = feed.iter().map(|(kind, _)| map.of(*kind)).collect();
    island::parse(automaton, &tokens, caps)
}
