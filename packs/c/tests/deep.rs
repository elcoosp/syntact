//! Deep-nesting robustness (bug-hunt pass): block recursion
//! (`parse_stmt → parse_block → parse_stmt`) must be depth-bounded —
//! `{{{{…` bombs are a classic stack-smash `DoS`. Fail cleanly, never
//! SIGSEGV.

use runtime_core::ERROR_KIND;

const BOMB_N: usize = 100_000;

fn lossless_check(src: &str) {
    let (events, tokens, _errors) = packs_c::reference::parse_events(src);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, runtime_core::events::Event::Error { .. })),
        "a nesting bomb must surface diagnostics"
    );
    let mut last_end = 0u32;
    let len = u32::try_from(src.len()).unwrap_or(u32::MAX);
    for (_, range) in &tokens {
        assert!(range.start >= last_end, "ranges must be ordered");
        assert!(range.end <= len, "ranges must be in-bounds");
        last_end = range.end;
    }
    let tree = packs_c::reference::parse(src).expect("tree builds");
    assert_eq!(
        tree.render(&runtime_core::input::Source::from_str_unchecked(src)),
        src,
        "render(parse(bomb)) == bomb (losslessness)"
    );
}

#[test]
fn nested_block_bomb_terminates_cleanly() {
    let src = format!(
        "void f(void){{{}{}}}",
        "{".repeat(BOMB_N),
        "}".repeat(BOMB_N)
    );
    let (events, _tokens, _errors) = packs_c::reference::parse_events(&src);
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
