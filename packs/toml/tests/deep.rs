//! Deep-nesting robustness (bug-hunt pass): container recursion in the
//! reference parser must be depth-bounded — nesting bombs (`[[[[…`,
//! `{{{{…`) are a classic stack-smash `DoS`. The parser must fail cleanly
//! (error events, lossless recovery), never SIGSEGV.

use runtime_core::ERROR_KIND;

/// Bomb size: far beyond any legal document, deep enough to smash an
/// unprotected stack in debug builds.
const BOMB_N: usize = 100_000;

fn lossless_check(src: &str) {
    let (events, tokens, _errors) = packs_toml::reference::parse_events(src);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, runtime_core::events::Event::Error { .. })),
        "a nesting bomb must surface diagnostics"
    );
    // Ranges stay ordered and in-bounds (clean + error regions).
    let mut last_end = 0u32;
    let len = u32::try_from(src.len()).unwrap_or(u32::MAX);
    for (_, range) in &tokens {
        assert!(range.start >= last_end, "ranges must be ordered");
        assert!(range.end <= len, "ranges must be in-bounds");
        last_end = range.end;
    }
    // The full tree path survives and stays byte-faithful.
    let tree = packs_toml::reference::parse(src).expect("tree builds");
    assert_eq!(
        tree.render(&runtime_core::input::Source::from_str_unchecked(src)),
        src,
        "render(parse(bomb)) == bomb (losslessness)"
    );
}

#[test]
fn nested_array_bomb_terminates_cleanly() {
    let src = format!("a = {}{}", "[".repeat(BOMB_N), "]".repeat(BOMB_N));
    let (events, _tokens, _errors) = packs_toml::reference::parse_events(&src);
    assert!(
        events.iter().any(|e| matches!(
            e,
            runtime_core::events::Event::Token {
                kind: ERROR_KIND,
                ..
            }
        )),
        "over-budget bytes ride ERROR regions"
    );
    lossless_check(&src);
}

#[test]
fn nested_inline_table_bomb_terminates_cleanly() {
    let src = format!("a = {}x{}", "{".repeat(BOMB_N), "}".repeat(BOMB_N));
    lossless_check(&src);
}
