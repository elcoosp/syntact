//! Criterion suite for the predicate-gated sink (§7.2 filtering half):
//! [`FilterSink`] wrapping [`StatsSink`] over the packed steady-state
//! JSON stream — a pass-through gate vs a token-dropping gate, against
//! the unfiltered `stats_sink/over_drive` baseline (the gate's added
//! cost per step, paid or saved). Run-fast config — trends only.

use std::ops::Range;

use criterion::{Criterion, criterion_group, criterion_main};
use packs_json::reference;
use runtime_core::events::{self, Event};
use runtime_core::sink::{FilterSink, FilterStep, StatsSink, drive_sink};

fn filter_sink(c: &mut Criterion) {
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

    c.bench_function("filter_sink/passthrough_over_drive", |b| {
        b.iter(|| {
            let mut sink = FilterSink::new(|_: &FilterStep<'_>| true, StatsSink::default());
            drive_sink(&output, &plain, &mut sink).expect("drives");
            sink.into_inner().into_stats()
        });
    });

    c.bench_function("filter_sink/drop_tokens_over_drive", |b| {
        b.iter(|| {
            let mut sink = FilterSink::new(
                |step: &FilterStep<'_>| !matches!(*step, FilterStep::Token { .. }),
                StatsSink::default(),
            );
            drive_sink(&output, &plain, &mut sink).expect("drives");
            sink.into_inner().into_stats()
        });
    });
}

criterion_group!(benches, filter_sink);
criterion_main!(benches);
