//! Criterion suite for the pack-registry seam (issue 046): lookup cost
//! on the three axes, and parse dispatch through an entry vs the direct
//! pack call (the fn-pointer seam must be free). Criterion defaults — the
//! budgets live in the dhat-counted tests, benches watch trends.

use criterion::{Criterion, criterion_group, criterion_main};
use pack_registry::PackRegistry;

fn lookups(c: &mut Criterion) {
    let registry = PackRegistry::reference();
    c.bench_function("pack_registry/get/first", |b| {
        b.iter(|| registry.get("json"));
    });
    c.bench_function("pack_registry/get/last", |b| {
        b.iter(|| registry.get("markdown"));
    });
    c.bench_function("pack_registry/for_language/last", |b| {
        b.iter(|| registry.for_language("markdown"));
    });
    c.bench_function("pack_registry/for_extension/multi_ext_pack", |b| {
        b.iter(|| registry.for_extension("h"));
    });
}

fn parse_dispatch(c: &mut Criterion) {
    let registry = PackRegistry::reference();
    let entry = registry.get("json").expect("json registered");
    let source = bench::steady_state_document();
    c.bench_function("pack_registry/parse_events/via_entry", |b| {
        b.iter(|| (entry.parse_events)(&source));
    });
    c.bench_function("pack_registry/parse_events/direct", |b| {
        b.iter(|| packs_json::reference::parse_events(&source));
    });
}

criterion_group!(benches, lookups, parse_dispatch);
criterion_main!(benches);
