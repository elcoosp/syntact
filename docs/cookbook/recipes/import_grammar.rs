// Recipe: import a tree-sitter grammar.
//
// `syntact import` (roadmap §9.1, ADR-015) converts a tree-sitter
// `grammar.js` into the gir surface through the importer (issue 037),
// gated to load back through `gir-syntax`, with a provenance report
// naming what happened to every construct. This recipe drives the
// library seam (`cli::import`) and pins the wedge gate.

use cli::import;

/// A minimal tree-sitter grammar (the supported subset: seq, repeat,
/// field, regex tokens, extras).
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

#[test]
fn import_converts_and_gates_the_surface() {
    // Convert (pure): the surface must load back through gir-syntax
    // before anything is reported — the issue 037 wedge gate.
    let imported = import::import_document(GRAMMAR_JS).expect("the toy grammar imports");
    assert_eq!(
        gir_syntax::load_grammar(&imported.kdl).expect("loads back").name,
        "toy"
    );

    // The provenance log (ADR-015): every construct is mapped or
    // dropped with the reason, in traversal order — nothing silent.
    assert!(
        imported
            .report
            .has(importer::Action::Mapped, "rule", "pair"),
        "rules are mapped"
    );
    assert_eq!(imported.report.dropped(), 0);

    // The JSON report (`--json`) mirrors the log deterministically:
    // entries in traversal order, then the totals.
    let report = import::render_json(&imported.report);
    assert!(report.contains("\"entries\": ["), "{report}");
    assert!(report.contains("\"action\": \"mapped\""), "{report}");
    assert!(report.contains("\"scope\": \"pair\""), "{report}");
    assert!(report.contains("\"mapped\": "), "{report}");
}
