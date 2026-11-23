//! Criterion suite for the query sink seam (§7.2 consumer table): the
//! push-based [`QuerySink`] driven through `drive_sink` (including the
//! `TeeSink` zipped pass) against the pull-based [`QueryCursor`] over
//! the same packed JSON stream. Criterion defaults — trends only.

use criterion::{Criterion, criterion_group, criterion_main};
use packs_json::reference;
use query::{KindResolver, PredicateArg, QueryCursor, QuerySink, compile};
use runtime_core::events::{self, Event};
use runtime_core::sink::{TeeSink, TokenSink, drive_sink};

/// The JSON pack's node-kind resolver (the pack's dense table).
struct JsonKinds;
impl KindResolver for JsonKinds {
    fn resolve(&self, name: &str) -> Option<u16> {
        use packs_json::kinds as k;
        Some(match name {
            "object" => k::OBJECT,
            "array" => k::ARRAY,
            "pair" => k::PAIR,
            "string" => k::STRING,
            "number" => k::NUMBER,
            "true" => k::TRUE,
            "false" => k::FALSE,
            "null" => k::NULL,
            _ => return None,
        })
    }
}

fn query_sink(c: &mut Criterion) {
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
    let plain: Vec<std::ops::Range<u32>> = ranges.iter().map(|(_, r)| r.clone()).collect();
    let compiled = compile("(pair (string) @key (number) @val)", &JsonKinds)
        .expect("reference query compiles");

    c.bench_function("query_cursor/over_steps", |b| {
        b.iter(|| {
            let mut cursor = QueryCursor::new(&compiled);
            let mut scratch = Vec::new();
            let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
            cursor.matches(&output, &ranges, &source, &mut scratch, &mut predicate)
        });
    });

    c.bench_function("query_sink/over_drive", |b| {
        b.iter(|| {
            let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
            let mut sink = QuerySink::new(&compiled, &source, &ranges, &mut predicate);
            drive_sink(&output, &plain, &mut sink).expect("drives");
            sink.into_matches()
        });
    });

    c.bench_function("query_sink/tee_with_token_sink", |b| {
        b.iter(|| {
            let mut predicate = |_: &str, _: &[PredicateArg<'_>]| true;
            let sink = QuerySink::new(&compiled, &source, &ranges, &mut predicate);
            let mut tee = TeeSink::new(TokenSink::default(), sink);
            drive_sink(&output, &plain, &mut tee).expect("drives");
            tee.second.into_matches()
        });
    });
}

criterion_group!(benches, query_sink);
criterion_main!(benches);
