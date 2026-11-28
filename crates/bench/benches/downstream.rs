//! Criterion suites (roadmap §7.8 downstream layer): the salsa tracked
//! queries over the JSON pack's green tree — what the first computation
//! costs (parse + one DFS) and what the memoized re-query and the
//! revision-driven recompute after an edit cost. The cross-file reuse
//! contract is conformance-gated by `crates/runtime-core/tests/
//! query_downstream.rs`; these benches measure the layer's economics.

use std::sync::Arc;

use std::fmt::Write as _;

use criterion::{Criterion, criterion_group, criterion_main};
use packs_json::kinds;
use runtime_core::query::{DownstreamDb, ParseFn, SourceFile};
use salsa::{Durability, Setter};

/// The bench DB: JSON pack entry, JSON kinds promoted to derived data.
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

/// A deterministic nested document: `pairs` top-level pairs whose values
/// are small arrays, serialized as JSON (no rand dependency).
fn nested_document(pairs: usize) -> String {
    let mut document = String::with_capacity(pairs * 32);
    document.push('{');
    for index in 0..pairs {
        if index > 0 {
            document.push(',');
        }
        let _ = write!(document, r#""key{index}": [1, 2, {index}]"#);
    }
    document.push('}');
    document
}

/// One file in a fresh DB (the "cold" per-iteration state).
fn cold_file(text: &str) -> (JsonDb, SourceFile) {
    let db = JsonDb::default();
    let file = SourceFile::builder(text.to_string())
        .text_durability(Durability::HIGH)
        .new(&db);
    (db, file)
}

fn downstream(c: &mut Criterion) {
    let document = nested_document(500);

    c.bench_function("downstream/parse_tree/cold", |b| {
        b.iter(|| {
            let (db, file) = cold_file(&document);
            Arc::clone(&runtime_core::query::parse_tree(&db, file).expect("parses"))
        });
    });
    c.bench_function("downstream/parse_tree/memoized", |b| {
        let (db, file) = cold_file(&document);
        let _ = runtime_core::query::parse_tree(&db, file);
        b.iter(|| Arc::clone(&runtime_core::query::parse_tree(&db, file).expect("parses")));
    });
    c.bench_function("downstream/symbols/cold", |b| {
        b.iter(|| {
            let (db, file) = cold_file(&document);
            runtime_core::query::symbols(&db, file).len()
        });
    });
    c.bench_function("downstream/symbols/memoized", |b| {
        let (db, file) = cold_file(&document);
        let _ = runtime_core::query::symbols(&db, file);
        b.iter(|| runtime_core::query::symbols(&db, file).len());
    });
    c.bench_function("downstream/index/memoized", |b| {
        let (db, file) = cold_file(&document);
        let _ = runtime_core::query::index(&db, file);
        b.iter(|| runtime_core::query::index(&db, file).nodes.len());
    });
    c.bench_function("downstream/symbols/after_edit", |b| {
        let (db, file) = cold_file(&document);
        let mut db = db;
        let _ = runtime_core::query::symbols(&db, file);
        let mut generation = 0_u32;
        b.iter(|| {
            generation += 1;
            // A real edit: the revision bumps and the derived query
            // re-executes (parse + DFS + filter) on the new text.
            file.set_text(&mut db)
                .to(format!(r#"{{"key{generation}": [1]}}"#));
            runtime_core::query::symbols(&db, file).len()
        });
    });
}

criterion_group!(benches, downstream);
criterion_main!(benches);
