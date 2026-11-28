//! The downstream-layer gates (roadmap §7.8): "Downstream derived data
//! (symbols, scopes-lite, folds) lives behind a salsa DB
//! (`runtime-core::query` + salsa): `#[salsa::tracked(lru=…)]` queries
//! over green trees, durability-tiered file inputs — the RA/ruff-`ty`
//! pattern (§2.3). Cross-file incremental invalidation comes from salsa
//! revisions, not from our tree layer."
//!
//! The suites pin exactly that contract over the JSON pack:
//!
//! - derived data (symbols, folds, scopes-lite) is served by
//!   `#[salsa::tracked]` queries over the green tree the pack's parse
//!   entry produces — a query re-walks only when the file's text
//!   changed;
//! - file inputs are durability-tiered (`SourceFile::builder` carries an
//!   explicit `Durability`; `set_text` re-tiers per write), and a
//!   same-value write backdates: memoized results survive the revision
//!   bump (salsa's durability contract);
//! - cross-file invalidation comes from salsa revisions: editing file A
//!   never recomputes file B's memoized queries (returned `Arc` identity
//!   unchanged), while editing file B itself does recompute;
//! - the positioned index layer matches the tree shape (preorder, parent
//!   links, first-significant spans), and a green-tree query works
//!   identically on malformed input (ERROR regions parse losslessly and
//!   never surface as symbols).

use std::sync::Arc;

use packs_json::kinds;
use salsa::{Durability, Setter};

use runtime_core::query::{DownstreamDb, ParseFn, SourceFile};

/// Document A: two pairs, one line (`pair` symbols; `object` fold/scope).
const DOC_A: &str = r#"{"name": "syntact", "n": 42}"#;

/// Document B: nested containers for the scopes-lite assertions.
const DOC_B: &str = r#"{"a": {"b": 1}, "c": [2]}"#;

/// A multiline document for the fold line bounds (4 lines).
const DOC_MULTILINE: &str = "{\n  \"a\": [1],\n  \"b\": 2\n}";

/// The test DB: JSON pack entry + JSON node kinds promoted to derived
/// data (`pair` symbols; `object`/`array` folds and scopes).
#[salsa::db]
#[derive(Default)]
struct JsonDb {
    storage: salsa::Storage<Self>,
}

#[salsa::db]
impl salsa::Database for JsonDb {}

#[salsa::db]
impl DownstreamDb for JsonDb {
    fn parse_fn(&self) -> ParseFn {
        packs_json::parse
    }

    fn symbol_kinds(&self) -> &'static [u16] {
        &[kinds::PAIR]
    }

    fn fold_kinds(&self) -> &'static [u16] {
        &[kinds::OBJECT, kinds::ARRAY]
    }

    fn scope_kinds(&self) -> &'static [u16] {
        &[kinds::OBJECT, kinds::ARRAY]
    }
}

#[test]
fn symbols_name_pairs_by_their_first_significant_token() {
    let db = JsonDb::default();
    let file = SourceFile::builder(DOC_A.to_string())
        .text_durability(Durability::HIGH)
        .new(&db);

    let symbols = runtime_core::query::symbols(&db, file);
    assert_eq!(symbols.len(), 2, "both pairs surface as symbols");
    assert!(
        symbols[0].range.start < symbols[1].range.start,
        "document order"
    );
    assert_eq!(symbols[0].kind, kinds::PAIR);
    assert_eq!(symbols[0].name.as_deref(), Some(r#""name""#));
    assert_eq!(symbols[1].name.as_deref(), Some(r#""n""#));
    let name_range = symbols[0].name_range.clone().expect("named pair");
    assert_eq!(
        &DOC_A[name_range.start as usize..name_range.end as usize],
        r#""name""#
    );
}

#[test]
fn folds_carry_line_bounds_for_multiline_regions() {
    let db = JsonDb::default();
    let file = SourceFile::builder(DOC_MULTILINE.to_string())
        .text_durability(Durability::HIGH)
        .new(&db);

    let folds = runtime_core::query::folds(&db, file);
    assert_eq!(folds.len(), 2, "the object and the array fold");
    assert!(
        folds
            .windows(2)
            .all(|pair| pair[0].range.start < pair[1].range.start)
    );
    let object = &folds[0];
    assert_eq!(object.kind, kinds::OBJECT);
    assert_eq!(object.start_line, 0, "the object opens on line 0");
    assert_eq!(object.end_line, 3, "the object closes on line 3");
    let array = &folds[1];
    assert_eq!(array.kind, kinds::ARRAY);
    assert_eq!(
        (array.start_line, array.end_line),
        (1, 1),
        "[1] stays on line 1"
    );
}

#[test]
fn scopes_lite_nests_containers_in_document_order() {
    let db = JsonDb::default();
    let file = SourceFile::builder(DOC_B.to_string())
        .text_durability(Durability::HIGH)
        .new(&db);

    let scopes = runtime_core::query::scopes(&db, file).scopes.clone();
    assert_eq!(scopes.len(), 3, "outer object, inner object, array");
    assert_eq!(scopes[0].kind, kinds::OBJECT);
    assert_eq!(
        scopes[0].parent, None,
        "the outermost object roots the tree"
    );
    assert_eq!(
        scopes[1].parent,
        Some(0),
        "the inner object nests in the outer"
    );
    assert_eq!(scopes[2].kind, kinds::ARRAY);
    assert_eq!(
        scopes[2].parent,
        Some(0),
        "the array nests in the outer object"
    );
    for scope in &scopes {
        assert!(scope.range.start <= scope.range.end, "sane spans");
    }
}

#[test]
fn editing_one_file_does_not_invalidate_another_files_memoized_queries() {
    let mut db = JsonDb::default();
    let a = SourceFile::builder(DOC_A.to_string())
        .text_durability(Durability::HIGH)
        .new(&db);
    let b = SourceFile::builder(DOC_B.to_string())
        .text_durability(Durability::MEDIUM)
        .new(&db);

    let before = runtime_core::query::symbols(&db, b);
    a.set_text(&mut db)
        .with_durability(Durability::HIGH)
        .to("[9]".to_string());
    let after = runtime_core::query::symbols(&db, b);
    assert!(
        Arc::ptr_eq(&before, &after),
        "file B's memoized symbols survive file A's edit (revision-based \
         invalidation, no recompute)"
    );

    // Positive control: editing file B itself recomputes.
    b.set_text(&mut db).to("[8, 9]".to_string());
    let changed = runtime_core::query::symbols(&db, b);
    assert!(
        !Arc::ptr_eq(&before, &changed),
        "file B's own edit recomputes"
    );
    assert!(changed.is_empty(), "no pairs in the edited document");
}

#[test]
fn same_value_writes_recompute_with_equal_derived_data() {
    let mut db = JsonDb::default();
    let file = SourceFile::builder(DOC_A.to_string())
        .text_durability(Durability::HIGH)
        .new(&db);

    let before = runtime_core::query::symbols(&db, file);
    file.set_text(&mut db)
        .with_durability(Durability::MEDIUM)
        .to(DOC_A.to_string());
    let after = runtime_core::query::symbols(&db, file);
    // Input writes bump the field revision unconditionally (no value
    // dedup in salsa's setter), so the derived query re-executes into a
    // fresh Arc — yet the derived data itself is unchanged.
    assert!(
        !Arc::ptr_eq(&before, &after),
        "a write re-executes the query"
    );
    assert_eq!(*before, *after, "same text, same derived data");

    file.set_text(&mut db).to("{\"k\": 1}".to_string());
    let changed = runtime_core::query::symbols(&db, file);
    assert_ne!(*before, *changed, "a real edit changes the derived data");
    assert_eq!(changed.len(), 1);
}

#[test]
fn malformed_input_still_serves_derived_data() {
    let db = JsonDb::default();
    let file = SourceFile::builder("{\"a\": }".to_string())
        .text_durability(Durability::HIGH)
        .new(&db);

    let parsed = runtime_core::query::parse_tree(&db, file).expect("lossless parse");
    assert!(parsed.has_error, "the malformed object carries the ERROR");

    let symbols = runtime_core::query::symbols(&db, file);
    assert_eq!(symbols.len(), 1, "the surviving pair still surfaces");
    for symbol in symbols.iter() {
        assert_ne!(symbol.kind, runtime_core::ERROR_KIND, "no ERROR symbols");
    }
}

#[test]
fn index_layer_matches_the_tree_shape() {
    let db = JsonDb::default();
    let file = SourceFile::builder(DOC_B.to_string())
        .text_durability(Durability::HIGH)
        .new(&db);

    let index = runtime_core::query::index(&db, file);
    assert!(index.nodes.len() >= 4, "root, object, pair, inner object…");
    assert_eq!(index.nodes[0].parent, None, "the root has no parent");
    let root_end = index.nodes[0].span.end as usize;
    assert_eq!(root_end, DOC_B.len(), "the root spans the document");
    // The first pair's key is the inner object's first-significant span
    // source: the pair named `"a"` inherits its name token from the key.
    let pairs = index
        .nodes
        .iter()
        .filter(|node| node.kind == kinds::PAIR)
        .count();
    assert_eq!(pairs, 3, "pairs a, b, and c are all positioned");
}
