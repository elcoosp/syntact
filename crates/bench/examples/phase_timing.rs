//! Phase attribution for the JSON tree-build pipeline (dev tool):
//! lex → wrap → process → trivia prep → range prep → drive+TreeBuilder
//! → finish, each timed separately over the steady-state corpus so
//! optimizations target the phase that actually costs.
//!
//! Run: `cargo run -p bench --example phase_timing --release`

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use runtime_core::events::{self, Event};
use runtime_core::input::Source;
use runtime_core::sink::{TriviaRecord, drive_sink};
use runtime_core::tree::{NodeCache, TreeBuilder};

/// One lex-phase output: events, `(kind, range)` tokens, trivia records.
type LexOutput = (Vec<Event>, Vec<(u16, Range<u32>)>, Vec<(Range<u32>, bool)>);

fn main() {
    const ITERS: u32 = 200;
    let source = bench::steady_state_document();
    println!("source: {} bytes", source.len());

    // Warm once so allocator caches settle.
    time_lex(&source);
    let total = Instant::now();
    let mut lex = 0u128;
    let mut wrap = 0u128;
    let mut proc = 0u128;
    let mut trivia_t = 0u128;
    let mut ranges_t = 0u128;
    let mut drive = 0u128;
    let mut finish_t = 0u128;

    for _ in 0..ITERS {
        let t0 = Instant::now();
        let (events_v, tokens, trivia) = time_lex(&source);
        lex += t0.elapsed().as_nanos();

        let t1 = Instant::now();
        let mut wrapped = Vec::with_capacity(events_v.len() + 2);
        wrapped.push(Event::Start {
            kind: packs_json::reference::ROOT_KIND,
            forward_parent: None,
        });
        wrapped.extend(events_v.iter().copied());
        wrapped.push(Event::Finish);
        wrap += t1.elapsed().as_nanos();

        let t2 = Instant::now();
        let output = events::process(&wrapped).expect("valid");
        proc += t2.elapsed().as_nanos();

        let t3 = Instant::now();
        let records: Vec<TriviaRecord> = trivia
            .iter()
            .map(|(range, trailing)| TriviaRecord {
                kind: packs_json::reference::token_kinds::WHITESPACE,
                range: range.clone(),
                trailing: *trailing,
            })
            .collect();
        trivia_t += t3.elapsed().as_nanos();

        let t4 = Instant::now();
        let ranges: Vec<Range<u32>> = tokens.iter().map(|(_, r)| r.clone()).collect();
        ranges_t += t4.elapsed().as_nanos();

        let input = Source::from_str_unchecked(&source);
        #[allow(clippy::arc_with_non_send_sync)]
        let cache = Arc::new(NodeCache::new());
        let mut builder = TreeBuilder::with_source(cache, &input);
        builder.attach_trivia_owned(records);

        let t5 = Instant::now();
        drive_sink(&output, &ranges, &mut builder).expect("drives");
        drive += t5.elapsed().as_nanos();

        let t6 = Instant::now();
        let tree = builder.finish().expect("one root");
        finish_t += t6.elapsed().as_nanos();
        std::hint::black_box(&tree);
    }
    let total_n = total.elapsed().as_nanos();
    #[allow(clippy::cast_precision_loss)] // µs display; u128→f64 truncation is display-only
    let f = |n: u128| format!("{:>9.1} µs", n as f64 / f64::from(ITERS) / 1_000.0);
    println!("lex (parse_events):  {}", f(lex));
    println!("wrap events:         {}", f(wrap));
    println!("process:             {}", f(proc));
    println!("trivia prep:         {}", f(trivia_t));
    println!("range prep:          {}", f(ranges_t));
    println!("drive (TreeBuilder): {}", f(drive));
    println!("finish:              {}", f(finish_t));
    println!(
        "SUM:                 {}",
        f(lex + wrap + proc + trivia_t + ranges_t + drive + finish_t)
    );
    println!("total loop:          {}", f(total_n));
}

/// Lex once, returning the three parser outputs.
///
/// The reference crate's `parse_events` returns events + tokens but not
/// trivia (its private parser owns it), so trivia is reconstructed here as
/// the exact complement of the clean ranges — the same records `parse`
/// derives during lexing, up to one extra pass over the token list.
fn time_lex(source: &str) -> LexOutput {
    let (events, tokens, _errors) = packs_json::reference::parse_events(source);
    let mut trivia: Vec<(Range<u32>, bool)> = Vec::new();
    let mut cursor = 0u32;
    let mut trailing = false;
    for (_, r) in &tokens {
        if r.start > cursor {
            trivia.push((cursor..r.start, trailing));
        }
        trailing = true;
        cursor = r.end;
    }
    let len = u32::try_from(source.len()).unwrap_or(u32::MAX);
    if len > cursor {
        trivia.push((cursor..len, trailing));
    }
    (events, tokens, trivia)
}
