//! Example: the downstream query layer (roadmap §7.8) over the JSON
//! pack. Two files live in one salsa DB; symbols, folds, and the
//! scopes-lite tree derive from the tracked parse query over the green
//! tree. Editing file A bumps the revision and re-derives A's data —
//! while file B's memoized symbols survive untouched (the returned `Arc`
//! keeps pointing at the same allocation), which is the layer's
//! revision-based invalidation contract in one runnable page.
//!
//! Run: `cargo run -p runtime-core --example downstream_query`

use std::sync::Arc;

use packs_json::kinds;
use salsa::{Durability, Setter};

use runtime_core::query::{DownstreamDb, ParseFn, SourceFile};

/// The demo DB: JSON pack entry, JSON kinds promoted to derived data.
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

fn main() {
    let mut db = JsonDb::default();
    let settings = SourceFile::builder(r#"{"name": "syntact", "retries": 3}"#.to_string())
        .text_durability(Durability::HIGH)
        .new(&db);
    let data = SourceFile::builder(r#"{"rows": [1, 2, 3]}"#.to_string())
        .text_durability(Durability::MEDIUM)
        .new(&db);

    let symbols = runtime_core::query::symbols(&db, settings);
    println!("settings: {} symbols", symbols.len());
    for symbol in symbols.iter() {
        let name = symbol.name.as_deref().unwrap_or("<anonymous>");
        println!(
            "  pair {name} at {}..{}",
            symbol.range.start, symbol.range.end
        );
    }

    let scopes = runtime_core::query::scopes(&db, data);
    println!("data: {} scopes", scopes.scopes.len());
    for scope in &scopes.scopes {
        println!(
            "  scope kind {} at {}..{} (parent: {:?})",
            scope.kind, scope.range.start, scope.range.end, scope.parent
        );
    }

    let folds = runtime_core::query::folds(&db, data);
    for fold in folds.iter() {
        println!(
            "  fold kind {} lines {}..{}",
            fold.kind, fold.start_line, fold.end_line
        );
    }

    // Edit file A only: A re-derives, B stays memoized (same allocation).
    let data_before = runtime_core::query::symbols(&db, data);
    settings
        .set_text(&mut db)
        .with_durability(Durability::HIGH)
        .to(r#"{"name": "syntact", "retries": 4}"#.to_string());
    let data_after = runtime_core::query::symbols(&db, data);
    let settings_after = runtime_core::query::symbols(&db, settings);

    println!("after the settings edit:");
    println!(
        "  data symbols reused: {}",
        std::sync::Arc::ptr_eq(&data_before, &data_after)
    );
    println!(
        "  settings symbols recomputed: {} (now {})",
        !Arc::ptr_eq(&symbols, &settings_after),
        settings_after.len()
    );
    for symbol in settings_after.iter() {
        println!(
            "  pair {} = {}..{}",
            symbol.name.as_deref().unwrap_or("<anonymous>"),
            symbol.range.start,
            symbol.range.end
        );
    }
}
