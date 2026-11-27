//! Criterion suites (roadmap §7.7 fast path 2, "balanced-block reparse"):
//! the incremental engine's balanced-block fast path against the full
//! relex fallback on the same mid-file block edits — what the block
//! splice saves over relexing everything. The outputs are equivalence-
//! gated per edit (`crates/incremental/tests/block_fast_path.rs`); these
//! benches measure what the accelerator buys. Run-fast config — budgets
//! are enforced by dhat-counted tests, benches exist to watch trends.

use std::fmt::Write as _;
use std::ops::Range;

use criterion::{Criterion, criterion_group, criterion_main};
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
                    while pos < bytes.len() {
                        if bytes[pos] == b'\\' {
                            pos += 2;
                            continue;
                        }
                        if bytes[pos] == b'"' {
                            pos += 1;
                            break;
                        }
                        pos += 1;
                    }
                    jk::STRING
                }
                b'0'..=b'9' | b'-' => {
                    while pos < bytes.len() && (bytes[pos].is_ascii_digit() || bytes[pos] == b'.') {
                        pos += 1;
                    }
                    jk::NUMBER
                }
                _ => {
                    pos += 1;
                    65534 // ERROR_KIND: garbage rides its own token
                }
            };
            let start32 = u32::try_from(start).unwrap_or(u32::MAX);
            let pos32 = u32::try_from(pos).unwrap_or(u32::MAX);
            out.push((kind, start32..pos32));
        }
        out
    }
}

const BRACKETS: &[(u16, u16)] = &[(jt::LBRACE, jt::RBRACE), (jt::LBRACK, jt::RBRACK)];

/// A list edit in the middle of the document: multi-token damage fully
/// inside one block, so the block path and the fallback are both legal
/// answers (equivalence-gated) and their costs are the comparison.
fn mid_block_edit(source: &str) -> (Vec<(u16, Range<u32>)>, Edit, String) {
    let tokens = JsonPackLexer.lex(source);
    let start = source.find("7, 8, 9").expect("the inner list");
    let edit = Edit::replace(start, start + 7, "70, 80, 90, 100, 110");
    let new_source = edit.apply(source);
    (tokens, edit, new_source)
}

fn block_vs_fallback(c: &mut Criterion) {
    for (count, name) in [(8_usize, "small"), (64, "medium"), (256, "large")] {
        let mut source = String::from("{\"rows\": [");
        for index in 0..count {
            if index > 0 {
                source.push(',');
            }
            let _ = write!(source, "{{\"id\": {index}, \"tags\": [7, 8, 9]}}");
        }
        source.push_str("]}");
        let (tokens, edit, new_source) = mid_block_edit(&source);
        let engine = Engine {
            lexer: &JsonPackLexer,
        };
        c.bench_function(&format!("incremental/block_path/{name}"), |b| {
            b.iter(|| {
                engine.incremental_edit_with_brackets(&source, &tokens, edit.clone(), BRACKETS)
            });
        });
        c.bench_function(&format!("incremental/fallback/{name}"), |b| {
            b.iter(|| engine.incremental_edit(&source, &tokens, edit.clone()));
        });
        // Equivalence: both answers are the fresh lex (the gate).
        let with_blocks =
            engine.incremental_edit_with_brackets(&source, &tokens, edit.clone(), BRACKETS);
        let fallback = engine.incremental_edit(&source, &tokens, edit);
        assert_eq!(with_blocks.tokens, fallback.tokens);
        assert_eq!(with_blocks.tokens, JsonPackLexer.lex(&new_source));
    }
}

criterion_group!(benches, block_vs_fallback);
criterion_main!(benches);
