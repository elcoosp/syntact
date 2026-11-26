//! Example: import a tree-sitter grammar.
//!
//! `syntact import` (roadmap §9.1, ADR-015) converts a tree-sitter
//! `grammar.js` into the gir surface through the importer (issue 037),
//! gated to load back through `gir-syntax`, with a provenance report
//! naming what happened to every construct. This example drives the
//! library seam (`cli::import`) over an embedded fixture.
//!
//! Run: `cargo run -p cli --example import_grammar`

use cli::import;

/// A minimal tree-sitter `grammar.js` (the importer's supported
/// subset: seq, choice, field, regex tokens).
const GRAMMAR_JS: &str = r"
module.exports = grammar({
  name: 'toy',
  extras: $ => [/\s/],
  rules: {
    source: $ => seq($.pair, repeat(seq('\n', $.pair))),
    pair: $ => seq(field('key', $.ident), ':', field('value', $.ident)),
    ident: $ => /[a-z]+/,
  }
});
";

fn main() {
    // Convert (pure): the surface is gated through gir-syntax before it
    // is returned — an import nobody can load is an error, not a
    // deliverable.
    let imported = import::import_document(GRAMMAR_JS).expect("the toy grammar imports");
    println!("{}— the converted gir surface:", imported.report);
    print!("{}", imported.kdl);

    // The provenance log rides alongside, in JSON too (`--json`).
    println!("---- report as JSON ----");
    print!("{}", import::render_json(&imported.report));
}
