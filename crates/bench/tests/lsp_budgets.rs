//! LSP allocation budget gates (issue 041): dhat-counted tests over the
//! grammar-server paths, machine-independent (counters, never
//! wall-clock), so they run in CI.
//!
//! Gates (300-rule fixture, ~20 KiB):
//! - `definition` lookup: zero allocations.
//! - `hover`: <= 8 blocks (the markdown payload only).
//! - `completion`: <= 2 blocks per returned item + 8 base.
//! - `document_symbols`: <= 2 blocks per declaration.
//! - `rename` plan: <= 2 blocks per rewrite site + 8 base.
//! - `LineIndex::new`: <= 16 blocks on the fixture (growth reallocations).
//! - `Document::open`: <= 600 KiB of heap per KiB of source — the bulk
//!   is the canonical KDL loader (external, ADR-012); the gate catches
//!   LSP-side regressions on top.

use gir_syntax::ByteSpan;
use std::fmt::Write as _;

use lsp::analysis::{Document, Workspace};
use lsp::index::LineIndex;

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

/// A deterministic grammar with `rules` rules and 5 tokens, shaped like
/// a real pack (alts, seqs, separators, annotations).
fn grammar_source(rules: usize) -> String {
    let mut out = String::from(
        "language \"bench\" version=\"1.0.0\" extension=\"bench\"\n\ntokens {\n    number kind=re \"[0-9]+\" node=\"true\"\n    ident kind=re \"[a-z_]+\"\n    plus kind=lit \"+\"\n    lparen kind=lit \"(\"\n    rparen kind=lit \")\"\n}\n\ngrammar {\n",
    );
    out.push_str("    start kind=alt number call-chain node=\"true\"\n");
    out.push_str("    call-chain kind=sep element=call separator=\"+\" allow-trailing=\"true\" reuse=\"true\"\n");
    out.push_str("    call kind=seq ident lparen arg-list rparen node=\"true\"\n");
    out.push_str("    arg-list kind=seq number rep\n");
    for index in 0..rules.saturating_sub(4) {
        if index % 2 == 0 {
            let _ = writeln!(
                out,
                "    extra-{index} kind=alt number ident lparen number rparen node=\"true\""
            );
        } else {
            let _ = writeln!(
                out,
                "    extra-{index} kind=seq number opt extra-{} rep wrap-error=\"true\"",
                index - 1
            );
        }
    }
    out.push_str("}\n");
    out
}

/// The fixture: 300 rules, ~20 KiB, plus the offsets the gates hover
/// over (declaration name, reference site).
fn fixture() -> (String, Document, usize, usize) {
    let text = grammar_source(300);
    let document = Document::open(text.clone());
    let decl_offset = text.find("call-chain kind=sep").expect("needle") + 2;
    let ref_offset = text.find("element=call").expect("needle") + "element=".len();
    (text, document, decl_offset, ref_offset)
}

#[test]
fn definition_lookup_allocates_nothing() {
    let _guard = gate();
    let (_text, document, _decl, ref_offset) = fixture();
    let (_, blocks, _) = measured(|| document.definition_at(ref_offset));
    assert_eq!(blocks, 0, "definition lookup must be allocation-free");
}

#[test]
fn hover_allocates_only_the_payload() {
    let _guard = gate();
    let (_text, document, _decl, ref_offset) = fixture();
    let (_, blocks, _) = measured(|| document.hover_at(ref_offset));
    assert!(blocks <= 8, "hover allocated {blocks} blocks (budget 8)");
}

#[test]
fn completion_allocates_per_item_not_per_rule() {
    let _guard = gate();
    let (text, document, decl, _ref_offset) = fixture();
    // No word under the cursor: the unfiltered list (all 305 decls +
    // 5 markers).
    let brace = text.rfind('}').expect("brace");
    let (items, blocks, _) = measured(|| document.completion_at(brace));
    assert_eq!(items.len(), 310);
    let budget = items.len() as u64 + 8;
    assert!(
        blocks <= budget,
        "completion allocated {blocks} blocks for {} items (budget {budget})",
        items.len()
    );
    // The filtered path stays tiny.
    let (_items, filtered, _) = measured(|| document.completion_at(decl));
    assert!(
        filtered <= 16,
        "filtered completion allocated {filtered} blocks"
    );
}

#[test]
fn document_symbols_allocate_one_block_per_decl() {
    let _guard = gate();
    let (_text, document, _decl, _ref_offset) = fixture();
    let (symbols, blocks, _) = measured(|| document.document_symbols());
    assert_eq!(symbols.len(), 305);
    assert!(
        blocks <= 2 * symbols.len() as u64,
        "symbols allocated {blocks} blocks for {} decls",
        symbols.len()
    );
}

#[test]
fn rename_plan_allocates_per_site_not_per_rule() {
    let _guard = gate();
    let (_text, document, decl, _ref_offset) = fixture();
    let (spans, blocks, _) = measured(|| document.rename_at(decl, "renamed"));
    let spans = spans.expect("renames");
    assert_eq!(spans.len(), 2); // decl + the `start` body ref
    assert!(
        blocks <= 2 * spans.len() as u64 + 8,
        "rename allocated {blocks} blocks"
    );
}

#[test]
fn line_index_allocates_only_growth_blocks() {
    let _guard = gate();
    let (text, _document, _decl, _ref_offset) = fixture();
    let (_, blocks, _) = measured(|| LineIndex::new(&text));
    assert!(
        blocks <= 16,
        "line index allocated {blocks} blocks on {} lines",
        text.lines().count()
    );
}

#[test]
fn document_open_stays_within_the_per_kib_gate() {
    let _guard = gate();
    let text = grammar_source(300);
    let (_document, blocks, bytes) = measured(|| Document::open(text.clone()));
    let per_kib = bytes * 1024 / text.len() as u64;
    let blocks_per_kib = blocks * 1024 / text.len() as u64;
    assert!(
        per_kib <= 600 * 1024,
        "open allocated {per_kib} B/KiB (budget 600 KiB/KiB); {blocks_per_kib} blocks/KiB"
    );
    // Sanity: the workspace open path shares the same gate per file.
    let mut workspace = Workspace::new();
    let (_doc, blocks, _bytes) = measured(|| {
        workspace.open("file:///bench.gir", text.clone());
        workspace
    });
    assert!(
        blocks <= 200_000,
        "workspace open allocated {blocks} blocks"
    );
    let _ = ByteSpan { start: 0, end: 0 }; // keep the import used for doc links
}
