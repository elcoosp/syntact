//! Criterion suites (roadmap §7.1, §7.7): the rope-input seam — deriving
//! the engine's `Edit` descriptor from a rope state pair and running the
//! incremental engine on it, against the same run from a hand-built
//! string edit. The streams are equivalence-gated per edit
//! (`crates/incremental/tests/rope.rs`); these benches measure what the
//! rope conversion adds. Run-fast config — budgets are enforced by
//! tests, benches exist to watch trends.

use std::ops::Range;

use criterion::{Criterion, criterion_group, criterion_main};
use incremental::rope::{Rope, edit_between};
use incremental::{Edit, Engine, Lexer};
use packs_json::kinds as jk;
use packs_json::reference::token_kinds as jt;

/// The JSON hand lexer (the `incremental` test fixture — lexes any
/// substring deterministically).
struct JsonPackLexer;
impl Lexer for JsonPackLexer {
    fn lex(&self, source: &str) -> Vec<(u16, Range<u32>)> {
        let bytes = source.as_bytes();
        let mut out: Vec<(u16, Range<u32>)> = Vec::new();
        let mut pos = 0_usize;
        while pos < bytes.len() {
            let start = pos;
            let kind = match bytes[pos] {
                b' ' | b'\t' | b'\r' | b'\n' => {
                    pos += 1;
                    continue;
                }
                b'{' => {
                    pos += 1;
                    jt::LBRACE
                }
                b'}' => {
                    pos += 1;
                    jt::RBRACE
                }
                b'[' => {
                    pos += 1;
                    jt::LBRACK
                }
                b']' => {
                    pos += 1;
                    jt::RBRACK
                }
                b':' => {
                    pos += 1;
                    jt::COLON
                }
                b',' => {
                    pos += 1;
                    jt::COMMA
                }
                b'"' => {
                    pos += 1;
                    while pos < bytes.len() && bytes[pos] != b'"' {
                        pos += if bytes[pos] == b'\\' { 2 } else { 1 };
                    }
                    pos = (pos + 1).min(bytes.len());
                    jk::STRING
                }
                _ => {
                    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
                        pos += 1;
                    }
                    jk::NUMBER
                }
            };
            out.push((kind, (start as u32)..(pos as u32)));
        }
        out
    }
}

/// A steady-state document and a mid-file key replacement — the editor's
/// typical single-hunk keystroke.
fn corpus() -> (String, String) {
    let mut old = String::from("{\n");
    for index in 0..128 {
        let _ = std::fmt::Write::write_fmt(&mut old, format_args!("  \"key_{index}\": {index},\n"));
    }
    old.push_str("  \"key_last\": \"value\"\n}\n");
    let new = old.replacen("\"key_64\": 64", "\"key_64\": \"rewritten\"", 1);
    (old, new)
}

/// Rope-derived descriptor + engine run vs the hand-built edit.
fn rope_vs_string_edit(c: &mut Criterion) {
    let (old, new) = corpus();
    let engine = Engine {
        lexer: &JsonPackLexer,
    };
    let old_tokens = engine.lexer.lex(&old);
    let old_rope = Rope::from_str(&old);
    let new_rope = Rope::from_str(&new);

    let rope_descriptor =
        edit_between(&old_rope, &new_rope).expect("a single-hunk edit has a cover");
    let hand_edit = Edit::replace(
        old.find("\"key_64\": 64").expect("the key exists"),
        old.find("\"key_64\": 64").expect("the key exists") + "\"key_64\": 64".len(),
        "\"key_64\": \"rewritten\"",
    );

    c.bench_function("rope_edit/descriptor_from_ropes", |b| {
        b.iter(|| edit_between(&old_rope, &new_rope));
    });
    c.bench_function("rope_edit/engine_on_rope_descriptor", |b| {
        b.iter(|| engine.incremental_edit(&old, &old_tokens, rope_descriptor.clone()));
    });
    c.bench_function("rope_edit/engine_on_string_edit", |b| {
        b.iter(|| engine.incremental_edit(&old, &old_tokens, hand_edit.clone()));
    });
    assert_eq!(
        engine
            .incremental_edit(&old, &old_tokens, rope_descriptor)
            .tokens,
        engine.incremental_edit(&old, &old_tokens, hand_edit).tokens,
    );
}

criterion_group!(rope_edit, rope_vs_string_edit);
criterion_main!(rope_edit);
