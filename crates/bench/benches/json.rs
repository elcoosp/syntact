//! Criterion suites (issue 018): the JSON reference parser across the
//! sink-mode matrix (issue 024). Run-fast config — the budgets are
//! enforced by dhat-counted tests, benches exist to watch trends.

use criterion::{Criterion, criterion_group, criterion_main};
use packs_json::reference;
use runtime_core::sink::{SkeletonSink, TokenSink, ValidationSink, drive_sink};

/// Deterministic JSON corpus: small / medium / steady-state.
fn corpus() -> Vec<(String, &'static str)> {
    vec![
        (bench::small_document(), "small"),
        (bench::medium_document(), "medium"),
        (bench::steady_state_document(), "steady"),
    ]
}

fn reference_parse(c: &mut Criterion) {
    for (source, name) in corpus() {
        c.bench_function(&format!("json/parse_events/{name}"), |b| {
            b.iter(|| reference::parse_events(&source));
        });
    }
}

fn tree_build(c: &mut Criterion) {
    for (source, name) in corpus() {
        c.bench_function(&format!("json/tree_build/{name}"), |b| {
            b.iter(|| reference::parse(&source).expect("parses"));
        });
    }
}

fn token_sink(c: &mut Criterion) {
    let (source, _) = corpus().pop().expect("steady");
    let (events, ranges, _errors) = reference::parse_events(&source);
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(runtime_core::events::Event::Start {
        kind: packs_json::reference::ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(events.iter().copied());
    wrapped.push(runtime_core::events::Event::Finish);
    let output = runtime_core::events::process(&wrapped).expect("valid");
    let plain: Vec<std::ops::Range<u32>> = ranges.into_iter().map(|(_, r)| r).collect();
    c.bench_function("json/token_sink/steady", |b| {
        b.iter(|| {
            let mut sink = TokenSink::default();
            drive_sink(&output, &plain, &mut sink).expect("drives");
            sink.tokens.len()
        });
    });
}

fn modes(c: &mut Criterion) {
    let (source, _) = corpus().pop().expect("steady");
    let (events, ranges, _errors) = reference::parse_events(&source);
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(runtime_core::events::Event::Start {
        kind: packs_json::reference::ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(events.iter().copied());
    wrapped.push(runtime_core::events::Event::Finish);
    let output = runtime_core::events::process(&wrapped).expect("valid");
    let plain: Vec<std::ops::Range<u32>> = ranges.into_iter().map(|(_, r)| r).collect();
    c.bench_function("json/mode/validate", |b| {
        b.iter(|| {
            let mut sink = ValidationSink::default();
            drive_sink(&output, &plain, &mut sink).expect("drives");
            sink.summary()
        });
    });
    c.bench_function("json/mode/skeleton", |b| {
        b.iter(|| {
            let mut sink = SkeletonSink::default();
            drive_sink(&output, &plain, &mut sink).expect("drives");
            sink.nodes.len()
        });
    });
}

criterion_group!(
    name = suites;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(std::time::Duration::from_millis(200))
        .measurement_time(std::time::Duration::from_secs(2));
    targets = reference_parse, tree_build, token_sink, modes
);
criterion_main!(suites);
