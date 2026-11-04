//! Deep-nesting robustness (bug-hunt pass): paren/suite recursion must be
//! depth-bounded — `((((…` bombs are a classic stack-smash `DoS`. Fail
//! cleanly, never SIGSEGV.

use runtime_core::ERROR_KIND;

const BOMB_N: usize = 100_000;

fn lossless_check(src: &str) {
    let (events, tokens, _errors) = packs_python::reference::parse_events(src);
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
    let tree = packs_python::reference::parse(src).expect("tree builds");
    assert_eq!(
        tree.render(&runtime_core::input::Source::from_str_unchecked(src)),
        src,
        "render(parse(bomb)) == bomb (losslessness)"
    );
}

#[test]
fn nested_paren_bomb_terminates_cleanly() {
    let src = format!("x = {}1{}", "(".repeat(BOMB_N), ")".repeat(BOMB_N));
    let (events, _tokens, _errors) = packs_python::reference::parse_events(&src);
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
fn nested_suite_bomb_terminates_cleanly() {
    // `if x:` nested via indentation — suite recursion. Kept at 10k
    // levels: the input is O(n²) bytes (growing indents), and an
    // unguarded stack already smashes around ~2k suite levels — 10k is
    // deep enough to catch the bug without a 5 GB test input.
    const SUITE_N: usize = 10_000;
    let mut src = String::with_capacity(SUITE_N * SUITE_N / 2 + SUITE_N * 8);
    for i in 0..SUITE_N {
        src.push_str(&" ".repeat(i + 1));
        src.push_str("if x:\n");
    }
    lossless_check(&src);
}
