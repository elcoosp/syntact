//! Criterion suite for the kind-statistics sink (§7.2 consumer table):
//! [`StatsSink`] driven through `drive_sink` over the packed
//! steady-state JSON stream — solo, and zipped with [`ValidationSink`]
//! via [`TeeSink`] (the one-pass validate-and-profile composition).
//! Criterion defaults (the full-parse suites in `json.rs`/`lsp.rs` set the run-fast config).

use std::ops::Range;

use criterion::{Criterion, criterion_group, criterion_main};
use packs_json::reference;
use runtime_core::events::{self, Event};
use runtime_core::sink::{StatsSink, TeeSink, ValidationSink, drive_sink};

fn stats_sink(c: &mut Criterion) {
    let source = bench::steady_state_document();
    let (mut events, ranges, _errors) = reference::parse_events(&source);
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(Event::Start {
        kind: reference::ROOT_KIND,
        forward_parent: None,
    });
    wrapped.append(&mut events);
    wrapped.push(Event::Finish);
    let output = events::process(&wrapped).expect("pack stream validates");
    let plain: Vec<Range<u32>> = ranges.iter().map(|(_, r)| r.clone()).collect();

    c.bench_function("stats_sink/over_drive", |b| {
        b.iter(|| {
            let mut sink = StatsSink::default();
            drive_sink(&output, &plain, &mut sink).expect("drives");
            sink.into_stats()
        });
    });

    c.bench_function("stats_sink/tee_with_validation", |b| {
        b.iter(|| {
            let mut tee = TeeSink::new(ValidationSink::default(), StatsSink::default());
            drive_sink(&output, &plain, &mut tee).expect("drives");
            (tee.first.summary(), tee.second.into_stats())
        });
    });
}

criterion_group!(benches, stats_sink);
criterion_main!(benches);
