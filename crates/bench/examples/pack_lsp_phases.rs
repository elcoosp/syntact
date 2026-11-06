//! Phase attribution for the pack-LSP document pipeline (dev tool):
//! pack parse → outline DFS → line index → view build, each timed
//! separately so optimizations target the phase that actually costs.
//!
//! Run: `cargo run -p bench --example pack_lsp_phases --release`

use std::fmt::Write as _;
use std::time::Instant;

use lsp::pack::document::Document;
use lsp::pack::outline::Outline;
use lsp::pack::registry::PackRegistry;

/// A deterministic JSON document with `pairs` top-level entries.
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

fn main() {
    const ITERS: u32 = 100;
    let registry = PackRegistry::reference();
    let entry = *registry.get("json").expect("json");
    let parse = entry.parse;
    let text = json_document(400);
    println!("source: {} bytes", text.len());

    // Warm.
    let _ = Document::open("m", "json", parse, &text);

    let mut parse_ns = 0u128;
    let mut outline_ns = 0u128;
    let mut index_ns = 0u128;
    let mut views_ns = 0u128;
    for _ in 0..ITERS {
        let t0 = Instant::now();
        let tree = parse(&text).expect("parses");
        let t1 = Instant::now();
        let positioned = Outline::build(&tree, &text);
        let t2 = Instant::now();
        let line_index = lsp::LineIndex::new(&text);
        let t3 = Instant::now();
        let mut document = Document {
            uri: "m".into(),
            pack: "json",
            text: text.clone(),
            tree,
            line_index,
            outline: positioned,
            diagnostics: Vec::new(),
            symbols: Vec::new(),
            folding: Vec::new(),
            semantic_data: Vec::new(),
        };
        document.build_views();
        let t4 = Instant::now();
        parse_ns += (t1 - t0).as_nanos();
        outline_ns += (t2 - t1).as_nanos();
        index_ns += (t3 - t2).as_nanos();
        views_ns += (t4 - t3).as_nanos();
    }
    let iters = u128::from(ITERS);
    println!(
        "parse: {} us | outline: {} us | line_index: {} us | views: {} us",
        parse_ns / iters / 1000,
        outline_ns / iters / 1000,
        index_ns / iters / 1000,
        views_ns / iters / 1000
    );
}
