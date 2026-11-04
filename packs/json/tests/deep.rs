//! Deep-nesting robustness (bug-hunt pass): a recursive-descent parser is
//! a stack-smash vector unless nesting depth is bounded. JSON bombing
//! inputs (`[[[[…`) are a classic `DoS` — the parser must fail *cleanly*
//! (error events, lossless recovery), never SIGSEGV.

use runtime_core::ERROR_KIND;
use runtime_core::events::Event;

/// The budget the reference parser enforces (mirrors
/// `reference::MAX_DEPTH`).
const MAX_DEPTH: usize = 512;

fn nested(n: usize) -> String {
    format!("{}{}", "[".repeat(n), "]".repeat(n))
}

#[test]
fn nesting_at_the_budget_parses() {
    let src = nested(MAX_DEPTH);
    let (events, tokens, errors) = packs_json::reference::parse_events(&src);
    assert_eq!(errors, 0, "depth-{MAX_DEPTH} input is legal JSON");
    assert_eq!(tokens.len(), MAX_DEPTH * 2, "every bracket is a token");
    assert_eq!(
        events.len(),
        MAX_DEPTH * 4,
        "Start + open-token + close-token + Finish per level"
    );
}

#[test]
fn nesting_over_the_budget_fails_clean_not_fatal() {
    // 10x the budget: must produce error events and terminate, not smash
    // the stack. The whole process surviving this assert IS the test.
    let src = nested(MAX_DEPTH * 10);
    let (events, tokens, _errors) = packs_json::reference::parse_events(&src);
    assert!(
        events.iter().any(|e| matches!(e, Event::Error { .. })),
        "over-budget nesting must surface diagnostics"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::Token {
                kind: ERROR_KIND,
                ..
            }
        )),
        "over-budget bytes must ride ERROR regions (losslessness)"
    );
    // Losslessness: the emitted ranges must account for every byte.
    let mut covered = 0u64;
    let mut last_end = 0u32;
    for (_, range) in &tokens {
        assert!(range.start >= last_end, "ranges must be ordered");
        covered += u64::from(range.end.saturating_sub(range.start));
        last_end = range.end;
    }
    assert_eq!(
        usize::try_from(covered).unwrap_or(usize::MAX),
        src.len(),
        "every byte accounted for"
    );
}

#[test]
fn nesting_over_budget_tree_build_survives() {
    // The full tree path (recursive render/drop live here too — tree depth
    // is bounded by the parse budget, so these must stay alive).
    let src = nested(MAX_DEPTH * 10);
    let tree = packs_json::reference::parse(&src).expect("tree builds");
    assert_eq!(
        tree.render(&runtime_core::input::Source::from_str_unchecked(&src)),
        src
    );
}

#[test]
fn mixed_bomb_does_not_confuse_the_budget() {
    // Objects and arrays interleaved deep — every container level counts.
    let unit = "[{\"a\":".repeat(MAX_DEPTH * 5);
    let src = format!("{unit}1{}", "}]".repeat(MAX_DEPTH * 5));
    let (events, _, _) = packs_json::reference::parse_events(&src);
    assert!(events.iter().any(|e| matches!(e, Event::Error { .. })));
}
