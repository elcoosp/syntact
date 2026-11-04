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
