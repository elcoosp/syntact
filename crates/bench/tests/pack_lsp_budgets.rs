//! Pack-LSP allocation budget gates (issue 042): dhat-counted tests
//! over the pack-server paths, machine-independent (counters, never
//! wall-clock), so they run in CI.
//!
//! Gates (400-pair JSON fixture, ~25 KiB):
//! - `Document::open` (parse + outline + every view): <= 450 blocks
//!   per KiB of source — the bulk is the pack's own parse (the packs
//!   carry their own budgets); the gate catches LSP-side regressions
//!   on top.
//! - didChange rebuild: the same budget (same pipeline; the bulk is
//!   the pack parse, which carries its own pack-level budgets).
//! - semantic-token build: <= 2 blocks per 100 tokens (the output Vec
//!   plus growth) beyond the token clones the API hands over.
//! - `highlight` with no matches: zero allocations.
//! - `selection_chain`: <= 3 blocks (the chain Vec + the LSP ranges).
//! - folding/diagnostics/semantic reads: zero allocations (prebuilt).

use std::fmt::Write as _;

use lsp::LineIndex;
use lsp::pack::document::{Document, semantic_token_data};
use lsp::pack::registry::PackRegistry;

/// dhat global allocator (allocation counting).
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// dhat counters are process-global: budget tests serialize so parallel
/// fixtures never pollute a measured window.
static GATE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn gate() -> std::sync::MutexGuard<'static, ()> {
    GATE.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Run `f` under a heap profiler; returns the output plus the number of
/// allocations made inside `f`.
fn measured<T>(f: impl FnOnce() -> T) -> (T, u64, u64) {
    let profiler = dhat::Profiler::new_heap();
    let before = dhat::HeapStats::get();
    let out = f();
    let after = dhat::HeapStats::get();
    drop(profiler);
    (
        out,
        after.total_blocks - before.total_blocks,
        after.total_bytes - before.total_bytes,
    )
}

/// A deterministic JSON document with `pairs` top-level entries shaped
/// like a real config (the bench fixture).
fn json_document(pairs: usize) -> String {
    let mut out = String::from("{\n");
    for index in 0..pairs {
        let _ = writeln!(
            out,
            "  \"key-{index}\": {{\"id\": {index}, \"tags\": [\"a{index}\", \"b{index}\"], \"ok\": {}}},",
            index % 2 == 0
        );
    }
    out.push_str("  \"tail\": null\n}\n");
    out
}

const PAIRS: usize = 400;
const OPEN_BLOCKS_PER_KIB: u64 = 450;
const CHANGE_BLOCKS_PER_KIB: u64 = 450;

#[test]
fn open_budget_per_kib() {
    let _guard = gate();
    let text = json_document(PAIRS);
    let registry = PackRegistry::reference();
    let entry = *registry.get("json").expect("json");
    let kib = (text.len() / 1024).max(1) as u64;
    let (_document, blocks, bytes) = measured(|| Document::open("mem", "json", entry.parse, &text));
    assert!(
        blocks <= OPEN_BLOCKS_PER_KIB * kib,
        "open allocations: {blocks} blocks ({bytes} bytes) for {kib} KiB — budget {}",
        OPEN_BLOCKS_PER_KIB * kib
    );
}

#[test]
fn did_change_budget_per_kib() {
    let _guard = gate();
    let text = json_document(PAIRS);
    let registry = PackRegistry::reference();
    let entry = *registry.get("json").expect("json");
    let mut document = Document::open("mem", "json", entry.parse, &text);
    let span = text.find("\"id\": 0").expect("needle") + 7;
    let kib = (text.len() / 1024).max(1) as u64;
    let ((), blocks, bytes) = measured(|| {
        document.edit(entry.parse, &[(Some(span..span + 1), "9")]);
    });
    assert!(
        blocks <= CHANGE_BLOCKS_PER_KIB * kib,
        "didChange allocations: {blocks} blocks ({bytes} bytes) for {kib} KiB — budget {}",
        CHANGE_BLOCKS_PER_KIB * kib
    );
}

#[test]
fn semantic_token_build_budget() {
    let _guard = gate();
    let text = json_document(PAIRS);
    let registry = PackRegistry::reference();
    let entry = *registry.get("json").expect("json");
    let document = Document::open("mem", "json", entry.parse, &text);
    let tokens = document.outline.tokens.clone();
    let line_index = LineIndex::new(&text);
    let token_count = tokens.len() as u64;
    let (data, blocks, _) = measured(|| semantic_token_data(&text, &line_index, &tokens));
    assert_eq!(data.len() % 5, 0, "delta-encoded quintuples");
    assert!(
        blocks <= 2 + token_count / 50,
        "semantic-token build allocations: {blocks} blocks for {token_count} tokens"
    );
}

#[test]
fn highlight_without_matches_is_zero_alloc() {
    let _guard = gate();
    let text = json_document(PAIRS);
    let registry = PackRegistry::reference();
    let entry = *registry.get("json").expect("json");
    let document = Document::open("mem", "json", entry.parse, &text);
    // A whitespace byte between tokens matches nothing.
    let offset = text.find(": ").expect("colon") + 1;
    let (_, blocks, _) = measured(|| document.highlight(offset));
    assert_eq!(blocks, 0, "no-match highlight allocates nothing");
}

#[test]
fn selection_chain_budget() {
    let _guard = gate();
    let text = json_document(PAIRS);
    let registry = PackRegistry::reference();
    let entry = *registry.get("json").expect("json");
    let document = Document::open("mem", "json", entry.parse, &text);
    let offset = text.find("\"key-7\"").expect("needle");
    let (_, blocks, _) = measured(|| document.selection_chain(offset));
    assert!(
        blocks <= 3,
        "selection chain allocations: {blocks} blocks (chain Vec + LSP ranges)"
    );
}

#[test]
fn prebuilt_view_reads_are_zero_alloc() {
    let _guard = gate();
    let text = json_document(PAIRS);
    let registry = PackRegistry::reference();
    let entry = *registry.get("json").expect("json");
    let document = Document::open("mem", "json", entry.parse, &text);
    let (_, fold_blocks, _) = measured(|| document.folding.len());
    let (_, diag_blocks, _) = measured(|| document.diagnostics.len());
    let (_, semantic_blocks, _) = measured(|| document.semantic_data.len());
    assert_eq!(fold_blocks, 0, "folding read allocates nothing");
    assert_eq!(diag_blocks, 0, "diagnostics read allocates nothing");
    assert_eq!(semantic_blocks, 0, "semantic read allocates nothing");
}
