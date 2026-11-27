# Incremental reparsing

`incremental` answers the editor question: after an edit, which parts
of the old token stream survive? The engine:

1. Normalizes the `Edit` (insert/delete/replace) against the old source.
2. Splits the old tokens at the edit byte position — the *left* and
   *right* flanks — everything between is the damaged window.
3. **Fast path**: at most one damaged token → relex only the margin
   window (a few tokens around the edit) and splice.
4. **Fallback**: otherwise lex the new source fresh. Both paths then
   compute the changed ranges for downstream consumers.
5. **Tree granularity**: beyond the token stream, `changed_tree_ranges`
   walks the old and new green trees in parallel and classifies every
   subtree as matching, descending, or differing — the §7.7
   changed-ranges walk for downstream invalidation, with sibling
   precision the token-level diff cannot express.

A pack plugs in through the `Lexer` trait — the ranges side table of a
whole-document parse already *is* a token stream (`ParseLexer`). The
recipe keeps a tiny word lexer so you can see the mechanics:

```rust
// Recipe: incremental reparsing with the reuse engine.
//
// The engine shifts an edit against the old token stream, relexes only
// the damaged margin window (fast path), and falls back to a full lex
// when the fast-path predicate rejects. Here a minimal word lexer
// keeps the recipe self-contained — a real pack plugs its side table
// in through `ParseLexer`.

use std::ops::Range;

use incremental::{Edit, Engine, Lexer};

/// Token kinds for the recipe lexer.
const WORD: u16 = 1;

/// Lex runs of non-space characters; whitespace is skipped (trivia).
struct WordLexer;

impl Lexer for WordLexer {
    fn lex(&self, source: &str) -> Vec<(u16, Range<u32>)> {
        let bytes = source.as_bytes();
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < bytes.len() {
            if bytes[pos].is_ascii_whitespace() {
                pos += 1;
                continue;
            }
            let start = pos;
            while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            out.push((
                WORD,
                u32::try_from(start).expect("fits u32")
                    ..u32::try_from(pos).expect("fits u32"),
            ));
        }
        out
    }
}

#[test]
fn one_token_edit_takes_the_fast_path() {
    let source = "hello world";
    let lexer = WordLexer;
    let old_tokens = lexer.lex(source);

    let engine = Engine { lexer: &lexer };
    let outcome = engine.incremental_edit(
        source,
        &old_tokens,
        Edit::replace(6, 11, "syntact"),
    );

    // The edit damaged exactly one token: the fast path relexed only
    // the margin window and reused the rest.
    assert!(outcome.rejection.is_none(), "{outcome:?}");
    assert!(outcome.stats.fast_path_hits >= 1);
    assert_eq!(outcome.stats.reused_tokens, 1);

    // The result is indistinguishable from a fresh lex of the new
    // source — the engine's equivalence contract.
    assert_eq!(outcome.tokens, lexer.lex("hello syntact"));
}

#[test]
fn multi_token_edits_fall_back_gracefully() {
    let source = "aaa bbb ccc";
    let lexer = WordLexer;
    let old_tokens = lexer.lex(source);

    // Replacing two whole tokens with one is outside the fast path's
    // one-damaged-token window: the engine falls back to a fresh lex.
    let engine = Engine { lexer: &lexer };
    let outcome = engine.incremental_edit(
        source,
        &old_tokens,
        Edit::replace(4, 11, "zz"),
    );

    assert!(outcome.rejection.is_some() || outcome.stats.fast_path_hits == 0);
    assert_eq!(outcome.tokens, lexer.lex("aaa zz"));
}
```


The equivalence contract is the interesting part: for *every* edit,
`reparse(edit(x))` equals a fresh parse of `x` at the token-stream
level — the integration fuzz module hammers exactly that property on
the json and toml packs with seeded mutations.

Next: [queries](queries.md) — asking questions of parse results.

## Rope input (§7.1)

The plan accepts `ropey::Rope` as an alternative input for editor
integrations: *"the incremental engine consumes rope deltas as edit
descriptors (§7.7)"*. An editor holds rope state; the transition
between the old and new rope converts into the engine's `Edit`
descriptor (`incremental::rope::edit_between` — common prefix and
suffix over characters, the middle span as the replacement), and the
descriptor applies back onto the rope (`incremental::rope::apply_edit`)
— the §8 *"ropey-compatible"* round trip. A multi-hunk rope change
yields one wide replacement span whose splice still reconstructs the
new text exactly, so the engine's fast-path predicates and fallback
stay correct; the engine-on-descriptor stream is equivalence-gated
against a fresh full lex in `crates/incremental/tests/rope.rs`.

```rust
// Recipe: feed the incremental engine from editor ropes.
//
// Roadmap §7.1: "ropey::Rope accepted as an alternative input for
// editor integrations — the incremental engine consumes rope deltas as
// edit descriptors (§7.7)". An editor holds rope state; the transition
// between the old and new rope converts into the engine's `Edit`
// descriptor (common prefix/suffix, the middle span as the
// replacement), and the descriptor applies back onto the rope — the
// §8 "ropey-compatible" round trip.

use incremental::rope::{apply_edit, edit_between};
use incremental::Rope;

#[test]
fn edit_descriptors_from_ropes() {
    let old_rope = Rope::from_str("{\"name\": \"syntact\", \"version\": 1}");
    let new_rope = Rope::from_str("{\"name\": \"syntact\", \"version\": 2}");

    // The state transition becomes one replacement descriptor: the
    // ropes' common prefix and suffix frame the changed middle span.
    let edit = edit_between(&old_rope, &new_rope).expect("the ropes differ");
    assert_eq!(edit.start, 31);
    assert_eq!(edit.end, 32);
    assert_eq!(edit.text, "2");
    assert_eq!(edit.apply(&old_rope.to_string()), new_rope.to_string());

    // The engine consumes rope-derived descriptors like any hand-built
    // edit; its output stream is equivalence-gated against a fresh full
    // lex in `crates/incremental/tests/rope.rs`.

    // And the descriptor applies back onto the rope (§8 ropey-compatible).
    let applied = apply_edit(&old_rope, &edit);
    assert_eq!(applied.to_string(), new_rope.to_string());

    // Equal ropes decline: there is no edit, nothing to classify.
    assert!(edit_between(&old_rope, &old_rope.clone()).is_none());
}
```
