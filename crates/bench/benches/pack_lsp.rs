//! Pack-LSP benches (issue 042): the editor-facing hot paths over
//! synthesized pack documents of realistic shape and size — document
//! open (parse + outline + every view), a didChange round trip (edit +
//! reparse + view rebuild), the semantic-token build, and the
//! query-time reads that ride the prebuilt views.

use std::fmt::Write as _;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use lsp::LineIndex;
use lsp::pack::document::{Document, classify_token, semantic_token_data, symbol_name};
use lsp::pack::registry::PackRegistry;

/// A deterministic JSON document with `pairs` top-level entries shaped
/// like a real config: nested objects, arrays, numbers, booleans.
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

/// A deterministic TOML document: `sections` tables with assignments.
fn toml_document(sections: usize) -> String {
    let mut out = String::new();
    for index in 0..sections {
        let _ = writeln!(out, "[section-{index}]");
        let _ = writeln!(out, "name = \"pack-{index}\"");
        let _ = writeln!(out, "weight = {index}.5");
        let _ = writeln!(out, "enabled = {}", index % 2 == 0);
        out.push('\n');
    }
    out
}

/// A deterministic Python document: `fns` functions with bodies.
fn python_document(fns: usize) -> String {
    let mut out = String::new();
    for index in 0..fns {
        let _ = writeln!(out, "def fn_{index}(alpha, beta):");
        let _ = writeln!(out, "    total = alpha + {index}");
        let _ = writeln!(out, "    return total * beta");
        out.push('\n');
    }
    out
}

fn bench(c: &mut Criterion) {
    let registry = PackRegistry::reference();
    let json = json_document(400);
    let toml = toml_document(600);
    let python = python_document(300);
    let mut group = c.benchmark_group("pack_lsp");

    for (name, pack, text) in [
        ("json", "json", &json),
        ("toml", "toml", &toml),
        ("python", "python", &python),
    ] {
        let entry = *registry.get(pack).expect("registered pack");
        group.throughput(criterion::Throughput::Bytes(text.len() as u64));

        // Document open: parse + outline DFS + every prebuilt view.
        group.bench_function(format!("open_{name}"), |b| {
            b.iter(|| Document::open("mem", entry.name, entry.parse, std::hint::black_box(text)));
        });
    }

    // didChange round trip on the json document: one in-place token
    // edit (id bump) through the full server change path — edit, reparse,
    // every view rebuilt.
    let entry = *registry.get("json").expect("json");
    let document = Document::open("mem", "json", entry.parse, &json);
    let needle = "\"id\": 0";
    let span = json.find(needle).expect("needle") + needle.len() - 1;
    group.throughput(criterion::Throughput::Bytes(json.len() as u64));
    group.bench_function("did_change_json", |b| {
        b.iter_batched(
            || document.clone(),
            |mut working| {
                working.edit(entry.parse, &[(Some(span..span + 1), "9")]);
                working
            },
            BatchSize::SmallInput,
        );
    });

    // Semantic-token data build (the delta encoder + classifier).
    let line_index = LineIndex::new(&json);
    let document = Document::open("mem", "json", entry.parse, &json);
    let tokens = document.outline.tokens.clone();
    group.bench_function("semantic_tokens_json", |b| {
        b.iter(|| {
            semantic_token_data(
                std::hint::black_box(&json),
                &line_index,
                std::hint::black_box(&tokens),
            )
        });
    });

    // Query-time reads over the prebuilt views.
    group.bench_function("symbols_serve_json", |b| {
        b.iter(|| std::hint::black_box(&document).symbols.clone());
    });
    group.bench_function("folding_serve_json", |b| {
        b.iter(|| std::hint::black_box(&document).folding.clone());
    });
    group.bench_function("diagnostics_serve_json", |b| {
        b.iter(|| std::hint::black_box(&document).diagnostics.clone());
    });
    let offset = json.find("\"key-1\"").expect("needle");
    group.bench_function("highlight_json", |b| {
        b.iter(|| std::hint::black_box(&document).highlight(offset));
    });
    group.bench_function("selection_chain_json", |b| {
        b.iter(|| std::hint::black_box(&document).selection_chain(offset));
    });
    group.bench_function("line_index_json", |b| {
        b.iter(|| LineIndex::new(std::hint::black_box(&json)));
    });

    // The token classifier over representative shapes.
    group.bench_function("classify_token", |b| {
        b.iter(|| {
            for shape in ["\"string literal\"", "123.45e+2", "==", "identifier_text"] {
                std::hint::black_box(classify_token(std::hint::black_box(shape)));
            }
        });
    });
    group.bench_function("symbol_name_cap", |b| {
        b.iter(|| symbol_name(std::hint::black_box("\"a fairly long string value\"")));
    });

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
