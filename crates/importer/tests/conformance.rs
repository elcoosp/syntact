//! Importer conformance wedge (issue 037 TDD): five tree-sitter
//! `grammar.js` fixtures — json, toml, ini, csv, markdown subset — each
//! imported once and checked three ways:
//!
//! 1. the rendered KDL matches the expected snapshot exactly,
//! 2. the KDL loads and lowers to valid IR through `gir-syntax`
//!    (the pipeline gate: an import nobody can load is worthless),
//! 3. the imported kind multiset matches the expected tree-sitter
//!    node kinds (the wedge: no kinds silently lost or invented).
//!
//! Plus report determinism (same input → same output, twice) and the
//! conversion-report entries the issue requires (mapped/dropped/why).

use std::collections::BTreeMap;

use gir_syntax::{load, load_grammar};
use importer::{Action, ImportError, diff_kinds, import, kdl, kind_multiset};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// tree-sitter-json shaped fixture: choice, seq, repeat-separated pairs,
/// optional lists, field annotations, a `token()`-wrapped regex, extras.
const JSON: &str = r#"
module.exports = grammar({
  name: 'json',
  extras: $ => [/\s/, $.comment],
  rules: {
    value: $ => choice($.object, $.array, $.string, $.number),
    object: $ => seq('{', repeat(seq($.pair, ',')), '}'),
    pair: $ => seq(field('key', $.string), ':', field('value', $.value)),
    array: $ => seq('[', optional(seq($.value, repeat(seq(',', $.value)))), ']'),
    string: $ => /"[^"]*"/,
    number: $ => /-?[0-9]+(\.[0-9]+)?([eE][+-]?[0-9]+)?/,
    comment: $ => token(/#[^\r\n]*/),
  }
});
"#;

/// tree-sitter-toml shaped fixture: `word` and `supertypes` (dropped),
/// `prec.left` (pratt hint), a merged `token(seq(lit, regex))` comment,
/// regex extras.
const TOML: &str = r#"
module.exports = grammar({
  name: 'toml',
  extras: $ => [/\s/, $.comment],
  word: $ => $.bare_key,
  supertypes: $ => [$.expression],
  rules: {
    document: $ => repeat(choice($.table, $.key_value, $.comment)),
    table: $ => seq('[', $.bare_key, ']'),
    key_value: $ => prec.left(2, seq($.bare_key, '=', $.value)),
    bare_key: $ => /[A-Za-z0-9_-]+/,
    value: $ => choice($.string, $.bare_key),
    string: $ => /"[^"]*"/,
    comment: $ => token(seq('#', /[^\r\n]*/)),
  }
});
"#;

/// INI-shaped fixture: an `externals` declaration imported as a
/// placeholder token, inline regexes inside `seq` bodies.
const INI: &str = r"
module.exports = grammar({
  name: 'ini',
  externals: $ => [$.error_sentinel],
  rules: {
    document: $ => repeat(choice($.section, $.setting)),
    section: $ => seq('[', $.name, ']', /\r?\n/),
    setting: $ => seq($.key, '=', $.value),
    name: $ => /[^ \t\r\n\]]+/,
    key: $ => /[^ \t\r\n=]+/,
    value: $ => /[^ \t\r\n]*/,
  }
});
";

/// CSV-shaped fixture: a top-level JS helper function before
/// `module.exports` (skipped with a report note), optional trailing.
const CSV: &str = r#"
function commaSep1(rule) {
  return seq(rule, repeat(seq(',', rule)));
}

module.exports = grammar({
  name: 'csv',
  rules: {
    file: $ => repeat($.row),
    row: $ => seq($.field, repeat(seq(',', $.field)), optional($.terminator)),
    terminator: $ => /\r?\n/,
    field: $ => choice($.quoted, $.bare),
    quoted: $ => /"[^"]*"/,
    bare: $ => /[^,"\r\n]*/,
  }
});
"#;

/// Markdown-subset fixture: `inline` and `conflicts` (dropped),
/// `prec.right` (pratt hint), a regex inside a seq.
const MARKDOWN: &str = r"
module.exports = grammar({
  name: 'markdown-subset',
  inline: $ => [$.heading],
  conflicts: $ => [[$.paragraph, $.heading]],
  rules: {
    document: $ => repeat(choice($.heading, $.paragraph)),
    block: $ => choice($.heading, $.paragraph),
    heading: $ => prec.right(1, seq(/#{1,6} /, $.text)),
    paragraph: $ => seq($.text, optional($.soft_break)),
    soft_break: $ => /\r?\n/,
    text: $ => /[^#\r\n]+/,
  }
});
";

// ---------------------------------------------------------------------------
// 1. KDL snapshots
// ---------------------------------------------------------------------------

#[test]
fn json_kdl_snapshot() {
    pretty_assertions::assert_eq!(
        kdl(JSON).expect("imports"),
        r##"language "json"

tokens {
    string kind=re "\"[^\"]*\"" node="true"
    number kind=re "-?[0-9]+(\\.[0-9]+)?([eE][+-]?[0-9]+)?" node="true"
    comment kind=re "#[^\\r\\n]*" node="true" trivia="true"
    extra1 kind=re "\\s" trivia="true"
}

grammar {
    value kind=alt object array string number node="true"
    object kind=seq "{" object-list1 "}" node="true"
    object-list1 kind=sep element=pair separator="," allow-trailing="true"
    pair kind=seq string ":" value node="true"
    array kind=seq "[" array-item1 opt "]" node="true"
    array-item1 kind=seq value array-item2 rep
    array-item2 kind=seq "," value
}
"##
    );
}

#[test]
fn toml_kdl_snapshot() {
    pretty_assertions::assert_eq!(
        kdl(TOML).expect("imports"),
        r##"language "toml"

tokens {
    bare_key kind=re "[A-Za-z0-9_-]+" node="true"
    string kind=re "\"[^\"]*\"" node="true"
    comment kind=re "#[^\\r\\n]*" node="true" trivia="true"
    extra1 kind=re "\\s" trivia="true"
}

grammar {
    document kind=seq document-item1 rep node="true"
    document-item1 kind=alt table key_value comment
    table kind=seq "[" bare_key "]" node="true"
    key_value kind=seq bare_key "=" value node="true" lbp=2 rbp=3 assoc=left
    value kind=alt string bare_key node="true"
}
"##
    );
}

#[test]
fn ini_kdl_snapshot() {
    pretty_assertions::assert_eq!(
        kdl(INI).expect("imports"),
        r#"language "ini"

tokens {
    section-re1 kind=re "\\r?\\n"
    name kind=re "[^ \\t\\r\\n\\]]+" node="true"
    key kind=re "[^ \\t\\r\\n=]+" node="true"
    value kind=re "[^ \\t\\r\\n]*" node="true"
    error_sentinel kind=lit "<external:error_sentinel>" node="true"
}

grammar {
    document kind=seq document-item1 rep node="true"
    document-item1 kind=alt section setting
    section kind=seq "[" name "]" section-re1 node="true"
    setting kind=seq key "=" value node="true"
}
"#
    );
}

#[test]
fn csv_kdl_snapshot() {
    pretty_assertions::assert_eq!(
        kdl(CSV).expect("imports"),
        r#"language "csv"

tokens {
    terminator kind=re "\\r?\\n" node="true"
    quoted kind=re "\"[^\"]*\"" node="true"
    bare kind=re "[^,\"\\r\\n]*" node="true"
}

grammar {
    file kind=seq row rep node="true"
    row kind=seq field row-item1 rep terminator opt node="true"
    row-item1 kind=seq "," field
    field kind=alt quoted bare node="true"
}
"#
    );
}

#[test]
fn markdown_kdl_snapshot() {
    pretty_assertions::assert_eq!(
        kdl(MARKDOWN).expect("imports"),
        r##"language "markdown-subset"

tokens {
    heading-re1 kind=re "#{1,6} "
    soft_break kind=re "\\r?\\n" node="true"
    text kind=re "[^#\\r\\n]+" node="true"
}

grammar {
    document kind=seq document-item1 rep node="true"
    document-item1 kind=alt heading paragraph
    block kind=alt heading paragraph node="true"
    heading kind=seq heading-re1 text node="true" lbp=1 rbp=1 assoc=right
    paragraph kind=seq text soft_break opt node="true"
}
"##
    );
}

// ---------------------------------------------------------------------------
// 2. Pipeline gate: every import loads and lowers to valid IR
// ---------------------------------------------------------------------------

#[test]
fn every_import_loads_and_lowers() {
    for (name, src) in [
        ("json", JSON),
        ("toml", TOML),
        ("ini", INI),
        ("csv", CSV),
        ("markdown", MARKDOWN),
    ] {
        let text = kdl(src).unwrap_or_else(|e| panic!("{name}: import failed: {e}"));
        let doc = load(&text).unwrap_or_else(|e| panic!("{name}: KDL does not load: {e}"));
        load_grammar(&text).unwrap_or_else(|e| panic!("{name}: KDL does not lower to IR: {e}"));
        assert_eq!(doc.language.name, language_name(name));
    }
}

fn language_name(fixture: &str) -> String {
    match fixture {
        "markdown" => "markdown-subset".to_owned(),
        other => other.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// 3. Kind-multiset wedge: no kinds silently lost or invented
// ---------------------------------------------------------------------------

fn kinds(entries: &[&str]) -> BTreeMap<String, u32> {
    entries.iter().map(|k| ((*k).to_owned(), 1)).collect()
}

#[test]
fn json_kind_multiset_wedge() {
    let out = import(JSON).expect("imports");
    let actual = kind_multiset(&out.doc);
    let expected = kinds(&[
        "value", "object", "pair", "array", "string", "number", "comment", "{", "}", ":", ",", "[",
        "]",
    ]);
    let diff = diff_kinds(&expected, &actual);
    assert!(
        diff.missing.is_empty() && diff.extra.is_empty(),
        "json kinds diverged: missing={:?} extra={:?}",
        diff.missing,
        diff.extra
    );
}

#[test]
fn toml_kind_multiset_wedge() {
    let out = import(TOML).expect("imports");
    let actual = kind_multiset(&out.doc);
    let expected = kinds(&[
        "document",
        "table",
        "key_value",
        "value",
        "bare_key",
        "string",
        "comment",
        "[",
        "]",
        "=",
    ]);
    let diff = diff_kinds(&expected, &actual);
    assert!(
        diff.missing.is_empty() && diff.extra.is_empty(),
        "toml kinds diverged: missing={:?} extra={:?}",
        diff.missing,
        diff.extra
    );
}

#[test]
fn ini_kind_multiset_wedge() {
    let out = import(INI).expect("imports");
    let actual = kind_multiset(&out.doc);
    let expected = kinds(&[
        "document",
        "section",
        "setting",
        "name",
        "key",
        "value",
        "error_sentinel",
        "[",
        "]",
        "=",
    ]);
    let diff = diff_kinds(&expected, &actual);
    assert!(
        diff.missing.is_empty() && diff.extra.is_empty(),
        "ini kinds diverged: missing={:?} extra={:?}",
        diff.missing,
        diff.extra
    );
}

#[test]
fn csv_kind_multiset_wedge() {
    let out = import(CSV).expect("imports");
    let actual = kind_multiset(&out.doc);
    let expected = kinds(&["file", "row", "field", "terminator", "quoted", "bare", ","]);
    let diff = diff_kinds(&expected, &actual);
    assert!(
        diff.missing.is_empty() && diff.extra.is_empty(),
        "csv kinds diverged: missing={:?} extra={:?}",
        diff.missing,
        diff.extra
    );
}

#[test]
fn markdown_kind_multiset_wedge() {
    let out = import(MARKDOWN).expect("imports");
    let actual = kind_multiset(&out.doc);
    let expected = kinds(&[
        "document",
        "block",
        "heading",
        "paragraph",
        "soft_break",
        "text",
    ]);
    let diff = diff_kinds(&expected, &actual);
    assert!(
        diff.missing.is_empty() && diff.extra.is_empty(),
        "markdown kinds diverged: missing={:?} extra={:?}",
        diff.missing,
        diff.extra
    );
}

// ---------------------------------------------------------------------------
// 4. Conversion report: mapped / dropped / why
// ---------------------------------------------------------------------------

#[test]
fn json_report_maps_and_notes() {
    let out = import(JSON).expect("imports");
    assert!(
        out.report.has(Action::Mapped, "rule", "object"),
        "object rule mapped: {:?}",
        out.report.entries
    );
    assert!(
        out.report
            .contains_detail("repeat(seq(x, sep)) → sep sugar (allow-trailing)"),
        "sep sugar is reported: {:?}",
        out.report.entries
    );
    assert!(
        out.report
            .contains_detail("field 'key' dropped (gir has no fields); child kept bare"),
        "fields are reported: {:?}",
        out.report.entries
    );
    assert!(
        out.report.contains_detail("token(/…/) → token comment"),
        "token() wrapper reported: {:?}",
        out.report.entries
    );
    assert!(
        out.report
            .contains_detail("extra $.comment → trivia on token comment"),
        "extras reported: {:?}",
        out.report.entries
    );
    assert_eq!(
        out.report.dropped(),
        0,
        "nothing in the json fixture is dropped: {:?}",
        out.report.entries
    );
}

#[test]
fn toml_report_drops_word_and_supertypes() {
    let out = import(TOML).expect("imports");
    assert!(out.report.has(Action::Dropped, "word", "toml"));
    assert!(out.report.has(Action::Dropped, "supertypes", "toml"));
    assert!(
        out.report
            .contains_detail("prec.left(2) → pratt hint (assoc left, bp 2)"),
        "prec.left mapped: {:?}",
        out.report.entries
    );
    assert!(
        out.report
            .contains_detail("token(seq(lit, regex)) merged to regex"),
        "regex-decided literal merge reported: {:?}",
        out.report.entries
    );
}

#[test]
fn ini_report_places_external_placeholder() {
    let out = import(INI).expect("imports");
    assert!(
        out.report.contains_detail(
            "external $.error_sentinel → placeholder token (manual wiring required)"
        ),
        "externals reported: {:?}",
        out.report.entries
    );
}

#[test]
fn csv_report_notes_top_level_js() {
    let out = import(CSV).expect("imports");
    assert!(
        out.report
            .contains_detail("top-level statement `function commaSep1` skipped (outside grammar)"),
        "top-level JS noted: {:?}",
        out.report.entries
    );
}

#[test]
fn markdown_report_drops_inline_and_conflicts() {
    let out = import(MARKDOWN).expect("imports");
    assert!(out.report.has(Action::Dropped, "inline", "markdown-subset"));
    assert!(
        out.report
            .has(Action::Dropped, "conflicts", "markdown-subset")
    );
    assert!(
        out.report
            .contains_detail("prec.right(1) → pratt hint (assoc right, bp 1)"),
        "prec.right mapped: {:?}",
        out.report.entries
    );
}

// ---------------------------------------------------------------------------
// 5. Report determinism
// ---------------------------------------------------------------------------

#[test]
fn import_is_deterministic() {
    for src in [JSON, TOML, INI, CSV, MARKDOWN] {
        let a = import(src).expect("imports");
        let b = import(src).expect("imports");
        pretty_assertions::assert_eq!(a.doc, b.doc, "doc must be deterministic");
        pretty_assertions::assert_eq!(a.report, b.report, "report must be deterministic");
    }
}

// ---------------------------------------------------------------------------
// 6. Errors
// ---------------------------------------------------------------------------

#[test]
fn unterminated_string_is_a_parse_error() {
    let err = import("module.exports = grammar({ name: 'json,\n").expect_err("bad string");
    match err {
        ImportError::Parse { offset, message } => {
            assert_eq!(offset, 33);
            assert!(message.contains("unterminated string literal"), "{message}");
        }
        ImportError::NoGrammar => panic!("expected Parse, got NoGrammar"),
    }
}

#[test]
fn missing_grammar_is_reported() {
    let err = import("const x = 1;\n").expect_err("no grammar");
    assert_eq!(err, ImportError::NoGrammar);
}

#[test]
fn misplaced_brace_is_a_parse_error() {
    let err = import("module.exports = grammar({ name: 'x', rules: { a: $ => seq(, (' })).\n")
        .expect_err("bad call");
    assert!(matches!(err, ImportError::Parse { .. }), "{err:?}");
}
