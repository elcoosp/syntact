//! Example: viewport parsing over the event protocol (roadmap §8's
//! `parse_partial`, the lezer `stopAt` analogue of §2.8). The JSON
//! pack's raw event stream is cut at a visible-end offset: the
//! truncated stream closes its open `Start`s, builds the byte-faithful
//! prefix tree, and names the resume point — the first token beyond the
//! viewport. A full-viewport cut reproduces the fresh parse exactly.
//!
//! Run: `cargo run -p runtime-core --example partial_viewport`

use packs_json::reference::{ROOT_KIND, parse_events};
use runtime_core::input::Source;
use runtime_core::partial::{build, cut_events};

fn main() {
    // Whitespace rides TriviaRecords (ADR-008), so the demo document is
    // whitespace-free: the byte-faithful prefix needs no trivia input.
    let document = r#"{"name":"syntact","viewport":[1,2,3],"tail":true}"#;
    let (events, tokens, _errors) = parse_events(document);

    // Cut before the "tail" pair: everything from there on is dropped,
    // the still-open nodes close, and the prefix tree renders exactly.
    let visible_end =
        u32::try_from(document.find(r#""tail""#).expect("tail pair")).expect("offset fits u32");
    let cut = cut_events(&events, &tokens, visible_end);

    let source = Source::from_str_unchecked(document);
    let partial = build(ROOT_KIND, &cut, &[], &source).expect("well-formed cut");
    let prefix = &document[..cut.resume_offset as usize];

    println!("document: {document}");
    println!("visible end: {visible_end}");
    println!(
        "truncated: {} — resume at byte {} (the next token starts there)",
        cut.truncated, cut.resume_offset
    );
    println!("kept tokens: {}", cut.tokens.len());
    println!("prefix tree renders: {prefix:?}");
    assert_eq!(
        partial.tree.render(&Source::from_str_unchecked(prefix)),
        prefix,
        "the partial tree is byte-faithful over the covered prefix"
    );

    // The full-viewport cut reproduces the fresh parse exactly.
    let full = cut_events(
        &events,
        &tokens,
        u32::try_from(document.len()).expect("fits"),
    );
    let full_parse = build(ROOT_KIND, &full, &[], &source).expect("complete stream");
    let fresh = packs_json::parse(document).expect("fresh parse");
    assert_eq!(full_parse.tree, fresh, "partial == fresh at full viewport");
    println!("full-viewport cut reproduces the fresh parse: true");
}
