// Recipe: drive the event protocol by hand.
//
// Every pack parser emits the same 8-byte event stream
// (`Start`/`Token`/`Error`/`Finish`). This recipe builds one by hand
// for the empty object `{}`, validates it with `events::process`,
// drives a `TreeBuilder` through `drive_sink`, and renders the green
// tree back to the source.

use runtime_core::events::{self, Event, Output};
use runtime_core::input::Source;
use runtime_core::tree::{NodeCache, TreeBuilder};
use std::ops::Range;
use std::sync::Arc;

use packs_json::kinds as jk;
use packs_json::reference::{ROOT_KIND, token_kinds as jt};

/// Hand-built events for `{}`: an `object` node wrapping its braces.
/// `forward_parent` is only needed for prefix productions; plain nodes
/// just pair `Start` with `Finish`.
fn events_empty_object() -> (Vec<Event>, Vec<Range<u32>>) {
    let events = vec![
        Event::Start {
            kind: ROOT_KIND,
            forward_parent: None,
        },
        Event::Start {
            kind: jk::OBJECT,
            forward_parent: None,
        },
        Event::Token {
            kind: jt::LBRACE,
            n_raw: 1,
        },
        Event::Token {
            kind: jt::RBRACE,
            n_raw: 1,
        },
        Event::Finish,
        Event::Finish,
    ];
    let ranges = vec![0..1, 1..2];
    (events, ranges)
}

#[test]
fn hand_built_stream_validates_and_builds() {
    let (events, ranges) = events_empty_object();

    // `process` checks balance and packs the stream into `Output` steps.
    let output: Output = events::process(&events).expect("balanced");

    // Drive the packed steps into a source-attached tree builder.
    let source = "{}";
    let input = Source::from_str_unchecked(source);
    // NodeCache is !Sync by design (single-threaded parser core); the Arc
    // only satisfies the builder's shared-ownership signature.
    #[allow(clippy::arc_with_non_send_sync)]
    let cache = Arc::new(NodeCache::new());
    let mut builder = TreeBuilder::with_source(cache, &input);
    runtime_core::sink::drive_sink(&output, &ranges, &mut builder).expect("drives");
    let tree = builder.finish().expect("tree");

    // Byte-faithful round trip, and the shape is object-in-root.
    assert_eq!(tree.render(&input), source);
    assert_eq!(tree.kind, ROOT_KIND);
    assert_eq!(tree.node_count(), 2, "root + object");
}
