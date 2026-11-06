//! LSP benches (issue 041): the editor-facing hot paths, all on one
//! synthesized grammar of realistic shape and size — document open
//! (full analysis incl. the shadow automaton), incremental reanalysis
//! after one edit, the query features, and the position index.

use std::fmt::Write as _;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use gir_syntax::ByteSpan;
use lsp::analysis::{Document, Workspace};
use lsp::index::LineIndex;

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
        let odd = index % 2 == 0;
        if odd {
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

fn bench(c: &mut Criterion) {
    let text = grammar_source(300);
    let mut group = c.benchmark_group("lsp");
    group.throughput(criterion::Throughput::Bytes(text.len() as u64));

    group.bench_function("line_index_build", |b| {
        b.iter(|| LineIndex::new(std::hint::black_box(&text)));
    });

    group.bench_function("document_open_full_analysis", |b| {
        b.iter(|| Document::open(std::hint::black_box(text.clone())));
    });

    // One incremental edit + full reanalysis (the didChange path).
    let mut document = Document::open(text.clone());
    let start = text.find("call-chain kind=sep").expect("needle") + "call-chain ".len();
    let span = ByteSpan {
        start,
        end: start + 4,
    };
    group.bench_function("did_change_reanalysis", |b| {
        b.iter(|| document.edit(Some(span), "warm"));
    });

    let document = Document::open(text.clone());
    let decl_offset = text.find("call-chain kind=sep").expect("needle") + 2;
    let ref_offset = text.find("element=call").expect("needle") + "element=".len();
    let annot_offset = text.find("allow-trailing").expect("needle") + 2;
    group.bench_function("hover", |b| {
        b.iter(|| document.hover_at(std::hint::black_box(ref_offset)));
    });
    group.bench_function("hover_annotation", |b| {
        b.iter(|| document.hover_at(std::hint::black_box(annot_offset)));
    });
    group.bench_function("completion", |b| {
        b.iter(|| document.completion_at(std::hint::black_box(decl_offset)));
    });
    group.bench_function("definition", |b| {
        b.iter(|| document.definition_at(std::hint::black_box(ref_offset)));
    });
    group.bench_function("rename_plan", |b| {
        b.iter(|| document.rename_at(std::hint::black_box(decl_offset), "renamed"));
    });
    group.bench_function("document_symbols", |b| {
        b.iter(|| document.document_symbols());
    });

    // The workspace open path across several grammar files.
    group.bench_function("workspace_open_x8", |b| {
        b.iter_batched(
            Workspace::new,
            |mut workspace| {
                for index in 0..8 {
                    workspace.open(&format!("file:///{index}.gir"), text.clone());
                }
                workspace
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

criterion_group!(
    name = suites;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(std::time::Duration::from_millis(200))
        .measurement_time(std::time::Duration::from_secs(2));
    targets = bench
);
criterion_main!(suites);
