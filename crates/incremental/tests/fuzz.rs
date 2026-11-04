//! Incremental engine integration tests (issue 030 TDD): fast-path
//! trigger stats, reuse predicate rejections, changed ranges vs brute
//! force, and the seeded fuzz equivalence gate on the json+toml packs.

use std::ops::Range;

use incremental::Adapter;
use incremental::ranges::changed_ranges_brute_force;
use incremental::{Edit, Engine, Snapshot, context_hash};

/// The JSON adapter: lexgen scanner (with the real `relex_one` fast
/// path) over the shared fixture grammar.
#[derive(Clone)]
struct JsonAdapter {
    scanner: lexgen::Scanner,
}

impl JsonAdapter {
    fn new() -> Self {
        let grammar = harness::metrics::json_fixture();
        Self {
            scanner: lexgen::compile(&grammar).expect("json tokens compile"),
        }
    }
}

impl incremental::Adapter for JsonAdapter {
    fn scan(&self, source: &str) -> Vec<(u16, Range<u32>)> {
        self.scanner
            .scan(source)
            .into_iter()
            .filter_map(|lexeme| match lexeme {
                lexgen::Lexeme::Token {
                    kind,
                    range,
                    trivia,
                } if !trivia => Some((kind, range.start..range.end)),
                _ => None,
            })
            .collect()
    }

    fn relex_one(&self, source: &str, offset: u32) -> Option<(u16, Range<u32>)> {
        self.scanner
            .relex_one(source, offset as usize)
            .map(|lexeme| match lexeme {
                lexgen::Lexeme::Token { kind, range, .. } => (kind, range.start..range.end),
                lexgen::Lexeme::Error { range } => (u16::MAX - 1, range.start..range.end),
            })
    }
}

/// The TOML adapter: the pack's own scan (via `parse_events`), no
/// single-token entry point (documented — the fast path stays JSON's).
struct TomlAdapter;

impl incremental::Adapter for TomlAdapter {
    fn scan(&self, source: &str) -> Vec<(u16, Range<u32>)> {
        packs_toml::reference::parse_events(source).1
    }
}

fn edit(source: &str, start: usize, end: usize, text: &str) -> (Edit, String) {
    let mut new = String::with_capacity(source.len());
    new.push_str(&source[..start]);
    new.push_str(text);
    new.push_str(&source[end..]);
    (
        Edit {
            range: u32::try_from(start).expect("small")..u32::try_from(end).expect("small"),
            text: text.to_owned(),
        },
        new,
    )
}

#[test]
fn fast_path_triggers_on_in_token_digit_edit() {
    let adapter = JsonAdapter::new();
    let source = "{\"a\": 12, \"b\": 3}";
    let mut engine = Engine::new(adapter, source);
    let (edit, new) = edit(source, 6, 8, "13"); // 12 → 13: same token shape
    let snap = engine.edit(&edit, &new);
    assert_eq!(snap.stats.fast_path_relex, 1, "fast path hit");
    assert_eq!(
        snap.tokens,
        JsonAdapter::new().scan(&new),
        "token equivalence"
    );
}

#[test]
fn fast_path_misses_when_the_token_kind_changes() {
    let adapter = JsonAdapter::new();
    let source = "{\"a\": 12}";
    let mut engine = Engine::new(adapter, source);
    // 12 → "s": the token becomes a string — the fast path must not claim it.
    let (edit, new) = edit(source, 7, 9, "\"s\"");
    let snap = engine.edit(&edit, &new);
    assert_eq!(
        snap.stats.fast_path_relex, 0,
        "kind change rejects the fast path"
    );
    assert_eq!(
        snap.tokens,
        JsonAdapter::new().scan(&new),
        "token equivalence"
    );
}

#[test]
fn reuse_predicates_reject_cross_block_edits() {
    let adapter = JsonAdapter::new();
    // An edit that changes the bytes outside the reused block's span
    // (here: the block is the whole document — every edit inside it
    // invalidates the boundary context when tokens vanish).
    let source = "{\"a\": [1, 2], \"b\": {\"c\": 3}}";
    let mut engine = Engine::new(adapter.clone(), source);
    let (edit, new) = edit(source, 7, 14, "[9, 8, 7, 6, 5, 4, 3, 2, 1]");
    let snap = engine.edit(&edit, &new);
    // Whatever the strategy, the token list is exact.
    assert_eq!(
        snap.tokens,
        JsonAdapter::new().scan(&new),
        "token equivalence"
    );
    // And the changed ranges agree with brute force.
    let old_tokens = adapter.scan(source);
    let brute = changed_ranges_brute_force(&old_tokens, &snap.tokens);
    assert_eq!(snap.changed, brute);
}

#[test]
fn changed_ranges_match_brute_force_on_edits() {
    let cases: &[(&str, usize, usize, &str)] = &[
        ("{\"a\": 1}", 7, 8, "42"),
        ("[1, 2, 3]", 4, 5, ""),
        ("{\"a\": [1]}", 5, 5, "\"k\": "),
        ("[true, null]", 1, 5, "false"),
    ];
    for (source, start, end, text) in cases {
        let (edit, new) = edit(source, *start, *end, text);
        let mut engine = Engine::new(JsonAdapter::new(), source);
        let snap: Snapshot = engine.edit(&edit, &new);
        let old_tokens = JsonAdapter::new().scan(source);
        let brute = changed_ranges_brute_force(&old_tokens, &snap.tokens);
        assert_eq!(
            snap.changed, brute,
            "changed ranges vs brute force on {source:?}"
        );
    }
}

#[test]
fn context_hash_discriminates_kinds_text_and_neighbors() {
    let base = context_hash(5, "x", Some(1), Some(2));
    assert_ne!(base, context_hash(6, "x", Some(1), Some(2)), "kind");
    assert_ne!(base, context_hash(5, "y", Some(1), Some(2)), "text");
    assert_ne!(base, context_hash(5, "x", Some(3), Some(2)), "prev kind");
    assert_ne!(base, context_hash(5, "x", Some(1), Some(3)), "next kind");
    assert_eq!(base, context_hash(5, "x", Some(1), Some(2)), "stable");
}

#[test]
fn toml_edits_reuse_and_stay_exact() {
    let source = "a = 1\nb = \"two\"\n[c]\nd = 3.5\n";
    let mut engine = Engine::new(TomlAdapter, source);
    let (edit, new) = edit(source, 4, 5, "42");
    let snap = engine.edit(&edit, &new);
    assert_eq!(snap.tokens, TomlAdapter.scan(&new), "token equivalence");
    assert!(packs_toml::parse(&new).is_ok(), "tree integration");
    assert_eq!(
        packs_toml::parse(&new)
            .expect("tree")
            .render(&runtime_core::input::Source::from_str_unchecked(&new)),
        new,
        "lossless"
    );
}

// ---------------------------------------------------------------------------
// The fuzz equivalence gate (issue 030): seeded random edits over the
// json+toml packs — incremental tokens == fresh scan, every time.
// ---------------------------------------------------------------------------

struct XorShift(u64);

impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n as u64)).unwrap_or(0)
    }
}

const JSON_DOCS: &[&str] = &[
    "{\"a\": 1}",
    "[1, 2, 3]",
    "{\"list\": [true, null, \"s\"], \"n\": -2.5}",
    "{\"deep\": {\"deeper\": [1, {\"x\": 0}]}}",
];

const TOML_DOCS: &[&str] = &[
    "a = 1\nb = \"two\"\n",
    "[table]\nx = 1\ny = [1, 2, 3]\n",
    "flag = true\nrate = 3.5\nname = 'literal'\n",
];

fn run_fuzz<A: incremental::Adapter>(adapter: A, docs: &[&str], seed: u64, rounds: usize) {
    let mut rng = XorShift(seed);
    for doc in docs {
        let mut engine = Engine::new(adapter_ref(&adapter), doc);
        let mut current = (*doc).to_owned();
        for _round in 0..rounds {
            if current.is_empty() {
                break;
            }
            // Random splice: replace a small range with one of a few
            // tokens' worth of text.
            let start = rng.below(current.len());
            let end = (start + rng.below(4)).min(current.len());
            let text = match rng.below(5) {
                0 => "1",
                1 => "\"s\"",
                2 => "[",
                3 => "",
                _ => ",",
            };
            let (edit, new) = edit(&current, start, end, text);
            let snap = engine.edit(&edit, &new);
            // THE EQUIVALENCE GATE.
            assert_eq!(
                snap.tokens,
                adapter_ref(&adapter).scan(&new),
                "incremental == fresh on {new:?}"
            );
            current = new;
        }
    }
}

fn adapter_ref<A: incremental::Adapter>(adapter: &A) -> &A {
    adapter
}

#[test]
fn fuzz_equivalence_json_pack() {
    run_fuzz(JsonAdapter::new(), JSON_DOCS, 0x5EED_F00D, 60);
}

#[test]
fn fuzz_equivalence_toml_pack() {
    run_fuzz(TomlAdapter, TOML_DOCS, 0xC0FF_EE12, 60);
}
