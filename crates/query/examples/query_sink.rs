//! The query sink seam in one pass: build the green tree AND match a
//! query over the same driven event stream — the §7.2 consumer table's
//! "one stream, many sinks" contract, with [`TeeSink`] zipping the
//! tree-side [`TreeBuilder`] and the push-based [`QuerySink`].
//!
//! Run: `cargo run -p query --example query_sink`

use std::ops::Range;
use std::sync::Arc;

use packs_json::kinds as k;
use query::{KindResolver, PredicateArg, QuerySink, compile};
use runtime_core::events::{self, Event};
use runtime_core::sink::{TeeSink, drive_sink};
use runtime_core::tree::{GreenNode, TreeBuilder};

/// The JSON pack's node-kind resolver (the pack's dense table).
struct JsonKinds;
impl KindResolver for JsonKinds {
    fn resolve(&self, name: &str) -> Option<u16> {
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

fn main() {
    // Minified JSON: no trivia gaps, so the tree built from the raw
    // events renders byte-for-byte (whitespace-bearing sources need the
    // pack's trivia records, which `parse_events` keeps internal).
    let source = r#"{"name":"syntact","stars":42,"forks":7}"#;
    let (mut events, ranges, _errors) = packs_json::reference::parse_events(source);
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(Event::Start {
        kind: packs_json::reference::ROOT_KIND,
        forward_parent: None,
    });
    wrapped.append(&mut events);
    wrapped.push(Event::Finish);
    let output = events::process(&wrapped).expect("pack stream validates");
    let plain: Vec<Range<u32>> = ranges.iter().map(|(_, range)| range.clone()).collect();

    // The query: pairs whose value is a number — capture key and value.
    let compiled = compile("(pair (string) @key (number) @val)", &JsonKinds)
        .expect("reference query compiles");

    let mut predicate = |name: &str, args: &[PredicateArg<'_>]| {
        // Host-side custom predicate hook (built-ins #eq?/#not-eq? run
        // without it; this demo only counts non-custom dispatches).
        !name.is_empty() && args.len() < 8
    };
    let input = runtime_core::input::Source::from_str_unchecked(source);
    let cache = Arc::new(runtime_core::tree::NodeCache::default());
    let builder = TreeBuilder::with_source(cache, &input);
    let sink = QuerySink::new(&compiled, source, &ranges, &mut predicate);
    let mut tee = TeeSink::new(builder, sink);
    drive_sink(&output, &plain, &mut tee).expect("one pass drives both sinks");

    // Tree half: the built tree renders the source byte-for-byte.
    let built: Arc<GreenNode> = tee.first.finish().expect("single root");
    assert_eq!(built.render(&input), source);
    println!("tree: renders byte-for-byte");

    // Query half: both number-valued pairs matched, key before value.
    for m in tee.second.matches() {
        let key = m.captures.iter().find(|c| c.slot == 0);
        let val = m.captures.iter().find(|c| c.slot == 1);
        let bytes = source.as_bytes();
        let key_text = key
            .map(|c| String::from_utf8_lossy(&bytes[c.span.clone()]).into_owned())
            .unwrap_or_default();
        let val_text = val
            .map(|c| String::from_utf8_lossy(&bytes[c.span.clone()]).into_owned())
            .unwrap_or_default();
        println!(
            "pair match: key={key_text} value={val_text} at {:?}",
            m.span
        );
    }
    assert_eq!(tee.second.matches().len(), 2, "stars and forks matched");

    // The same tree walks like any green tree (query did not disturb it).
    let events_walked = runtime_core::view::walk(&built).count();
    assert!(events_walked > 0, "tree is walkable");
    println!("query + tree in one pass — {events_walked} walk events, no second stream iteration");
}
