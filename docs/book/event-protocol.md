# The event protocol

Every parser in the workspace emits the same flat event stream. There
are four events:

- `Start { kind, forward_parent }` — enter a node.
- `Token { kind, n_raw }` — a leaf; one parallel `(kind, byte range)`
  entry per token in the stream's *ranges* side table.
- `Error { diag }` — a syntax error; `diag` indexes a diagnostic side
  table. Every error is immediately followed by
  `Start { kind: ERROR_KIND }` so recovery stays tree-shaped.
- `Finish` — leave the most recent node.

Two invariants make the stream lossless: **trivia never appears as a
token event** (it rides `TriviaRecord`s, ADR-008), and the token ranges
plus trivia records partition `[0, len)` exactly.

## From events to a tree

`events::process` validates the stream (balanced starts/finishes,
dense token steps) and packs it into an `Output`. `drive_sink` then
plays the packed steps — with the ranges table — into any `Sink`. The
tree-shaped one is `TreeBuilder`: give it the source and it produces a
green tree that renders byte-faithfully.

The recipe builds a stream for `{}` by hand so you can see every
moving part:

```rust
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
```


Notice the two `Finish` events (root and object) and that the ranges
table carries exactly one entry per `Token`. Real parsers produce the
same shape — `packs_json::reference::parse_events` returns this very
tuple for any input, with `Error` events riding `ERROR_KIND` nodes
where recovery wrapped the damage.

Next: [packs](packs-walkthrough.md) — how a language pack is put
together.
