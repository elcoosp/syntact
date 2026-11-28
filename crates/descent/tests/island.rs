//! The GLR island executor's descent-engine gates (roadmap §7.4, §12.2).
//!
//! Plan lines pinned: §7.4 *"GLR islands for regions the analyzer marks
//! ambiguous"* (the executor accepts where the shift-wins-resolved table
//! dead-ends — the islands' reason to exist), and §12.2's status note
//! *"the island executor — the arena GSS with fork/merge caps and the
//! descent-engine integration — does not"*. The integration under test:
//! one descent feed, three backends — the engine's own verdict, the
//! table-driven driver's, and the island executor's over the same tokens
//! ([`descent::island_over_feed`]).

use descent::{LookMap, island_over_feed, parse_events};
use gir_syntax::load_grammar;
use lr_shadow::Automaton;
use lr_shadow::driver;
use lr_shadow::island::IslandCaps;
use lr_shadow::lower::Look;

/// The ambiguous nested-pair grammar `s → a s a | ε`, annotated
/// `@allow_ambiguous` — the analyzer classifies its region as a GLR
/// island candidate, and the shift-wins-resolved table dead-ends on
/// `a a` while the engine's backtracking descent and the island
/// executor both accept it.
const NESTED_GIR: &str = r#"
language "nested" version="1.0.0"

tokens {
    a kind=lit "a"
}

grammar {
    s     kind=alt inner empty allow-ambiguous="true" node="true"
    inner kind=seq a s a
    empty kind=seq
}
"#;

/// A feed with its kind → terminal map and the automaton, over one
/// grammar: everything three backends need to answer on one document.
struct Rig {
    lowered: descent_lower::Lowered,
    scanner: lexgen::Scanner,
    automaton: Automaton,
    map: LookMap,
}

impl Rig {
    fn compile(source: &str) -> Rig {
        let grammar = load_grammar(source).expect("grammar loads");
        let lowered = descent_lower::lower(&grammar).expect("lowers");
        let scanner = lexgen::compile(&grammar).expect("lexes");
        let automaton = Automaton::build(&grammar);
        let trivia: Vec<bool> = grammar.tokens.iter().map(|token| token.trivia).collect();
        let map = LookMap::of_grammar(&lowered.kind_of_token, &trivia, lowered.bad_kind);
        Rig {
            lowered,
            scanner,
            automaton,
            map,
        }
    }

    fn feed(&self, source: &str) -> Vec<(u16, std::ops::Range<u32>)> {
        self.lowered.feed(&self.scanner, source)
    }

    fn looks(&self, feed: &[(u16, std::ops::Range<u32>)]) -> Vec<Look> {
        feed.iter().map(|(kind, _)| self.map.of(*kind)).collect()
    }
}

#[test]
fn the_island_rescues_the_table_verdict_to_match_the_engine() {
    let rig = Rig::compile(NESTED_GIR);
    let feed = rig.feed("aa");
    let caps = IslandCaps::default();

    // The engine: a clean derivation (the ε branch of the inner `s`
    // lets the outer `a s a` close over both tokens).
    let run = parse_events(&rig.lowered.desc, &feed, "aa");
    assert_eq!(run.errors, 0, "the engine cleanly derives `aa`");

    // The table: the shift-wins resolution dead-ends on the same tokens.
    let table = driver::parse(&rig.automaton, &rig.looks(&feed));
    assert!(!table.accepted, "the resolved table rejects `aa`");

    // The island: forks at the recorded conflict and accepts — the
    // table verdict is rescued to match the engine's.
    let island = island_over_feed(&feed, &rig.map, &rig.automaton, &caps);
    assert!(
        island.accepted,
        "the island executor accepts what the table cannot place"
    );
    assert_eq!(island.consumed, feed.len());
    assert!(island.forks >= 1, "the GSS forked at the conflict");
    assert!(island.rejection.is_none());
}

#[test]
fn the_three_backends_align_on_rejection() {
    let rig = Rig::compile(NESTED_GIR);
    let feed = rig.feed("a");
    let caps = IslandCaps::default();

    // The engine: the ε derivation leaves the token for the document
    // sweep — an error, not a clean derivation.
    let run = parse_events(&rig.lowered.desc, &feed, "a");
    assert!(run.errors > 0, "the engine does not cleanly derive `a`");

    // The table: the shift dead-ends (no closing `a`).
    let table = driver::parse(&rig.automaton, &rig.looks(&feed));
    assert!(!table.accepted);

    // The island: the forked branches both die on the missing closer.
    let island = island_over_feed(&feed, &rig.map, &rig.automaton, &caps);
    assert!(!island.accepted);
    let rejection = island.rejection.expect("the island reports the stop");
    assert_eq!(rejection.at, feed.len(), "the pseudo-EOF position");
}

#[test]
fn the_island_agrees_with_the_engine_and_the_table_on_json() {
    let rig = Rig::compile(descent_lower::JSON_GRAMMAR);
    let caps = IslandCaps::default();
    for document in ["{}", "{\"a\": 1}", "[1, {\"b\": null}, \"x\"]"] {
        let feed = rig.feed(document);
        let run = parse_events(&rig.lowered.desc, &feed, document);
        let table = driver::parse(&rig.automaton, &rig.looks(&feed));
        let island = island_over_feed(&feed, &rig.map, &rig.automaton, &caps);
        assert!(
            island.accepted,
            "the island accepts the deterministic document {document}"
        );
        assert_eq!(run.errors, 0, "the engine cleanly derives {document}");
        assert!(table.accepted, "the table accepts {document}");
        assert_eq!(island.consumed, feed.len());
        assert_eq!(
            island.forks, 0,
            "no conflicts in the deterministic grammar, no forks"
        );
    }
}

#[test]
fn the_island_run_over_a_feed_is_a_deterministic_function() {
    let rig = Rig::compile(NESTED_GIR);
    let feed = rig.feed("aa");
    let caps = IslandCaps::default();
    let first = island_over_feed(&feed, &rig.map, &rig.automaton, &caps);
    let second = island_over_feed(&feed, &rig.map, &rig.automaton, &caps);
    assert_eq!(first, second);
}

#[test]
fn the_look_map_is_total_over_the_engine_kind_space() {
    let rig = Rig::compile(NESTED_GIR);
    // Declared token → its terminal (token index 0 owns kind slot of `a`).
    let a_kind = rig.lowered.kind_of_token[0];
    assert_eq!(rig.map.of(a_kind), Look(0), "the token maps to its id");
    // The sentinel and past-the-table kinds ride the stranger terminal.
    assert_eq!(rig.map.of(rig.lowered.bad_kind), descent::STRANGER);
    assert_eq!(rig.map.of(u16::MAX), descent::STRANGER);
}
