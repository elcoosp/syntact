# Importing tree-sitter grammars

The adoption wedge is the importer (roadmap §9.1 command list, ADR-015
— "the tree-sitter grammar importer is the adoption wedge"): asking
users to rewrite grammars by hand is the adoption killer, so
`syntact import <GRAMMAR.js>` converts a tree-sitter `grammar.js` into
the gir surface through the importer library (issue 037), with `-o
<FILE>` writing the converted KDL to disk. The command layer adds one
obligation the library alone does not enforce for CLI users: the
converted surface must load back through `gir-syntax` before anything
is printed or written — the issue 037 wedge gate ("an import nobody
can load is worthless") applied at the point of delivery, so a broken
conversion can never pose as a deliverable.

The conversion report is the provenance log ADR-015 requires: every
construct the source carries is recorded as mapped (possibly with
sugar), dropped (not representable in the canonical form, with the
reason), or noted (converted with a simplification worth knowing), in
source-traversal order — never map iteration, so the same source
always produces the same report. Extras become trivia tokens with one
provenance entry each; dropped constructs (`word`, `supertypes`,
`inline`, `conflicts`) are listed with their reason instead of
disappearing. `--json` renders the same log as machine-readable
entries plus totals, matching the §9.1 rule that every command
speaks JSON for tooling.

```rust
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
```

On the command line the same flow is two invocations — convert, then
load the result through the pipeline gates the importer conformance
suite (issue 037) already runs over its five fixtures:

```bash
syntact import tree-sitter-grammar.js -o mylang.gir
syntact check mylang.gir
syntact build mylang.gir
```

The importer is deliberately a wedge, not a universal translator: the
supported subset (seq, choice, repeat, optional, field, literal and
regex tokens, `token()` wrappers, extras) converts fully and
deterministically; everything else is a named refusal or a
named drop — never a silent omission — so a converted grammar can be
reviewed against its source with confidence.
