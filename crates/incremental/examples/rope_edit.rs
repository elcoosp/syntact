//! Consumer example: the rope-input seam (roadmap §7.1) — an editor
//! holding ropey ropes converts the state transition into the
//! incremental engine's `Edit` descriptor, reparse runs, and the
//! descriptor applies back onto the rope.
//!
//! Run: `cargo run -p incremental --example rope_edit`

use incremental::rope::{apply_edit, edit_between};
use incremental::{Engine, Lexer, Rope};

/// The JSON hand lexer (the `incremental` test fixture).
struct JsonLexer;
impl Lexer for JsonLexer {
    fn lex(&self, _source: &str) -> Vec<(u16, std::ops::Range<u32>)> {
        Vec::new() // the example prints descriptor mechanics, not streams
    }
}

fn main() {
    let old_rope = Rope::from_str("{\"name\": \"syntact\", \"version\": 1}");
    let new_rope = Rope::from_str("{\"name\": \"syntact\", \"version\": 2}");

    // The editor's state transition becomes the engine's descriptor:
    // common prefix/suffix, middle span as the replacement.
    let edit = edit_between(&old_rope, &new_rope).expect("the ropes differ");
    println!(
        "descriptor: replace bytes [{}, {}) with {:?}",
        edit.start, edit.end, edit.text
    );

    // The engine consumes it like any hand-built edit (the outcome's
    // stream equals a fresh lex of the new source — the tests pin it).
    let engine = Engine { lexer: &JsonLexer };
    let outcome = engine.incremental_edit(
        &old_rope.to_string(),
        &JsonLexer.lex(&old_rope.to_string()),
        edit.clone(),
    );
    println!(
        "engine classified {} changed range(s)",
        outcome.changed.len()
    );

    // And the descriptor applies back onto the rope (§8 ropey-compatible).
    let applied = apply_edit(&old_rope, &edit);
    println!("applied back: {applied}");
    assert_eq!(applied.to_string(), new_rope.to_string());
}
