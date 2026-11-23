//! Criterion suite for the gated driver (§7.2 step-gated early exit):
//! plain [`drive_sink`] vs a pass-through [`DriveGate`] (per-step gate
//! overhead) vs [`MaxErrors`] halting early on an error-injected stream
//! (the fail-fast win against draining it). Run-fast config — trends
//! only.

use std::ops::Range;

use criterion::{Criterion, criterion_group, criterion_main};
use packs_json::reference;
use runtime_core::events::{self, Event};
use runtime_core::sink::{MaxErrors, ValidationSink, drive_sink, drive_sink_until};

fn gated_drives(c: &mut Criterion) {
    let source = bench::steady_state_document();
    let (mut events_vec, ranges, _errors) = reference::parse_events(&source);
    let mut wrapped = Vec::with_capacity(events_vec.len() + 2);
    wrapped.push(Event::Start {
        kind: reference::ROOT_KIND,
        forward_parent: None,
    });
    wrapped.append(&mut events_vec);
    wrapped.push(Event::Finish);
    let output = events::process(&wrapped).expect("pack stream validates");
    let plain: Vec<Range<u32>> = ranges.iter().map(|(_, r)| r.clone()).collect();

    // An error-injected stream: one deferred diagnostic every 200 words,
    // so MaxErrors(0) halts at ~word 200 instead of draining ~30k steps.
    let mut injected = Vec::with_capacity(output.stream.len() + output.stream.len() / 200 + 1);
    for (index, &word) in output.stream.iter().enumerate() {
        if index % 200 == 199 {
            injected.push(events::Output::pack_error(0));
        }
        injected.push(word);
    }
    let malformed = events::Output { stream: injected };

    let mut group = c.benchmark_group("drive_gate");
    group.bench_function("drive_sink_baseline", |b| {
        b.iter(|| {
            let mut sink = ValidationSink::default();
            let _ = drive_sink(&output, &plain, &mut sink);
            sink.summary()
        });
    });
    group.bench_function("pass_through_gate", |b| {
        b.iter(|| {
            let mut sink = ValidationSink::default();
            let mut gate =
                |_: &runtime_core::sink::FilterStep<'_>| std::ops::ControlFlow::<()>::Continue(());
            drive_sink_until(&output, &plain, &mut gate, &mut sink)
        });
    });
    group.bench_function("max_errors_early_stop", |b| {
        b.iter(|| {
            let mut sink = ValidationSink::default();
            let mut gate = MaxErrors::new(0);
            drive_sink_until(&malformed, &plain, &mut gate, &mut sink)
        });
    });
    group.bench_function("error_injected_full_drain", |b| {
        b.iter(|| {
            let mut sink = ValidationSink::default();
            let _ = drive_sink(&malformed, &plain, &mut sink);
            sink.summary()
        });
    });
    group.finish();
}

criterion_group!(
    name = gated_suites;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(std::time::Duration::from_millis(200))
        .measurement_time(std::time::Duration::from_secs(2));
    targets = gated_drives
);
criterion_main!(gated_suites);
