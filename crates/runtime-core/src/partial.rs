//! # partial — viewport parsing over the event protocol (§8, §2.8)
//!
//! The `parse_partial` seam of the roadmap §8 API sketch — "lezer
//! stopAt analogue": "Partial parsing (`PartialParse::stopAt(pos)`):
//! viewport parsing — parse the visible region first" (§2.8), the
//! `partial` entry of the mode glossary, and the §12.3 P3 gate "partial
//! parse works".
//!
//! The event protocol makes the viewport a stream-level transform: a
//! pack's raw event stream (`parse_events` shape) is cut at a
//! visible-end offset, the still-open `Start`s are closed with
//! synthesized `Finish` events so the truncated stream stays
//! well-formed (`process()`able, tree-buildable), and the covered
//! prefix builds a byte-faithful partial tree. [`PartialParse::
//! resume_offset`] names the first token beyond the viewport — the
//! point an editor's next viewport pass resumes from (the rest of the
//! document streams in later; the full-viewport cut reproduces the
//! fresh parse exactly, which the suites gate).
//!
//! Cut policy: the cut lands on a token boundary (before the first
//! token that crosses the visible end) and never splits a forward-
//! parent chain (the §2.3 precede wrap) — when a kept `Start`'s chain
//! target lies beyond the natural cut, the cut extends past the target
//! so the stream stays processable, keeping the crossing token with it.
//! Trivia (ADR-008) is sliced by the caller's records: every record
//! ending at or before [`PartialEvents::resume_offset`] survives, which
//! keeps the covered prefix byte-faithful through the trailing-space
//! convention.

use std::ops::Range;
use std::sync::Arc;

use crate::events::{Event, process};
use crate::input::Source;
use crate::sink::{TriviaRecord, drive_sink};
use crate::tree::{GreenNode, NodeCache, TreeBuilder};

/// A raw event stream cut at a viewport end: the kept events (well
/// formed — every `Start` closed), the kept token ranges (parallel to
/// the kept `Token` events), and where the viewport ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialEvents {
    /// The kept, closed event stream (root wrap excluded — the caller
    /// wraps per its pack convention, as every pack's `parse` does).
    pub events: Vec<Event>,
    /// Byte ranges for the kept `Token` events, in stream order (the
    /// `drive_sink` range table).
    pub tokens: Vec<Range<u32>>,
    /// One past the last byte the kept stream covers: the start of the
    /// first token beyond the viewport (the resume point), or the last
    /// kept token's end when nothing was dropped.
    pub resume_offset: u32,
    /// Whether any token event was dropped.
    pub truncated: bool,
}

/// A completed viewport parse: the partial tree over the covered
/// prefix plus the resume bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialParse {
    /// The partial tree over `0..resume_offset` (byte-faithful there).
    pub tree: Arc<GreenNode>,
    /// The resume point (see [`PartialEvents::resume_offset`]).
    pub resume_offset: u32,
    /// Whether the stream was truncated.
    pub truncated: bool,
}

/// The final hop target of the forward-parent chain starting at the
/// `Start` event at `index` (`events` bounds the walk; a malformed chain
/// simply stops — `process` reports it).
fn chain_end(events: &[Event], index: usize) -> usize {
    let Event::Start {
        forward_parent: Some(offset),
        ..
    } = events[index]
    else {
        return index;
    };
    let mut base = index;
    let mut hop = Some(offset);
    let mut last = index;
    while let Some(offset) = hop {
        // Hops are relative to the previous hop (rust-analyzer shape).
        let target = base + usize::try_from(offset.get()).unwrap_or(usize::MAX);
        if target >= events.len() {
            break;
        }
        match events[target] {
            Event::Start { forward_parent, .. } => {
                last = target;
                base = target;
                hop = forward_parent;
            }
            _ => break,
        }
    }
    last
}

/// Cut a raw event stream at `visible_end` (the lezer `stopAt`
/// analogue).
///
/// `tokens` is the pack's token side table (parallel to the stream's
/// `Token` events). The cut keeps every event before the first token
/// whose range crosses `visible_end`, extends past any forward-parent
/// chain the prefix still references, and closes the still-open
/// `Start`s.
#[must_use]
pub fn cut_events(
    events: &[Event],
    tokens: &[(u16, Range<u32>)],
    visible_end: u32,
) -> PartialEvents {
    // Token-event indexes in stream order (parallel to `tokens`).
    let token_event_indexes: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, Event::Token { .. }))
        .map(|(index, _)| index)
        .collect();

    // The natural cut: before the first token crossing the viewport.
    let crossing = token_event_indexes
        .iter()
        .zip(tokens.iter())
        .find(|(_, (_, range))| range.end > visible_end)
        .map(|(event_index, _)| *event_index);

    let mut keep_len = match crossing {
        Some(index) => index,
        None => events.len(),
    };
    // Chain extension: a kept Start's forward parent must be kept too
    // (the §2.3 precede wrap lives a hop away, relative to the previous
    // hop). Extending may pull in further references — monotonic until
    // stable.
    loop {
        let pending = (0..keep_len)
            .filter(|index| {
                matches!(
                    events[*index],
                    Event::Start {
                        forward_parent: Some(_),
                        ..
                    }
                )
            })
            .fold(0_usize, |max, index| max.max(chain_end(events, index)));
        if pending < keep_len || keep_len >= events.len() {
            break;
        }
        keep_len = (pending + 1).min(events.len());
    }

    // Kept token ranges, in stream order.
    let kept_tokens = token_event_indexes
        .iter()
        .zip(tokens.iter())
        .take_while(|(event_index, _)| **event_index < keep_len)
        .map(|(_, (_, range))| range.clone())
        .collect::<Vec<_>>();

    // Still-open `Start`s close with synthesized `Finish` events (all
    // `Finish`es are identical, so the open count suffices). The open
    // count mirrors `process`'s flattening: a forward-parent chain folds
    // into ONE node — chain hops (targets) do not open their own.
    let mut folded = vec![false; keep_len];
    for index in 0..keep_len {
        if matches!(
            events[index],
            Event::Start {
                forward_parent: Some(_),
                ..
            }
        ) {
            // Mark every hop of the (now fully kept) chain.
            let mut base = index;
            let mut hop = match events[index] {
                Event::Start { forward_parent, .. } => forward_parent,
                _ => None,
            };
            while let Some(offset) = hop {
                let target = base + usize::try_from(offset.get()).unwrap_or(usize::MAX);
                if target >= keep_len {
                    break;
                }
                match events[target] {
                    Event::Start { forward_parent, .. } => {
                        folded[target] = true;
                        base = target;
                        hop = forward_parent;
                    }
                    _ => break,
                }
            }
        }
    }
    let open = events[..keep_len]
        .iter()
        .enumerate()
        .filter(|(index, _)| !folded[*index])
        .fold(0_isize, |depth, (_, event)| match event {
            Event::Start { .. } => depth + 1,
            Event::Finish => depth - 1,
            _ => depth,
        });

    let truncated = keep_len < events.len() && crossing.is_some();
    let resume_offset = if truncated {
        tokens
            .get(kept_tokens.len())
            .map_or(visible_end, |(_, range)| range.start)
    } else {
        kept_tokens.last().map_or(0, |range| range.end)
    };

    let mut cut_events = events[..keep_len].to_vec();
    for _ in 0..open.max(0) {
        cut_events.push(Event::Finish);
    }

    PartialEvents {
        events: cut_events,
        tokens: kept_tokens,
        resume_offset,
        truncated,
    }
}

/// Build the partial tree over a [`cut_events`] result.
///
/// `root_kind` follows the pack's own implicit-root convention (every
/// pack wraps its document in one root, e.g. `packs_json::reference::
/// ROOT_KIND`); `trivia` is the pack's trivia record list, sliced to
/// the covered prefix here so the partial tree stays byte-faithful.
///
/// # Errors
/// Structural failures propagate from [`process`], [`drive_sink`], and
/// [`TreeBuilder::finish`] (a malformed stream or a resume offset that
/// is not a char boundary).
pub fn build(
    root_kind: u16,
    cut: &PartialEvents,
    trivia: &[TriviaRecord],
    source: &Source<'_>,
) -> Result<PartialParse, String> {
    let resume = cut.resume_offset as usize;
    let text = source
        .text()
        .get(..resume)
        .ok_or_else(|| format!("resume offset {resume} splits a char"))?;
    let sliced = Source::from_str_unchecked(text);

    // The pack-conventional root wrap: one root Start, one root Finish;
    // the cut's own open nodes are already closed.
    let mut wrapped = Vec::with_capacity(cut.events.len() + 2);
    wrapped.push(Event::Start {
        kind: root_kind,
        forward_parent: None,
    });
    wrapped.extend(cut.events.iter().copied());
    wrapped.push(Event::Finish);

    let output = process(&wrapped)?;
    // NodeCache is !Sync by design (single-threaded parser core) — the
    // Arc satisfies the builder's shared-ownership signature, never a
    // thread (the packs' `parse` integration uses the same shape).
    let cache = Arc::new(NodeCache::new());
    let mut builder = TreeBuilder::with_source(cache, &sliced);
    let kept_trivia: Vec<TriviaRecord> = trivia
        .iter()
        .filter(|record| record.range.end <= cut.resume_offset)
        .cloned()
        .collect();
    builder.attach_trivia_records(&kept_trivia);
    drive_sink(&output, &cut.tokens, &mut builder)?;
    let tree = builder.finish()?;
    Ok(PartialParse {
        tree,
        resume_offset: cut.resume_offset,
        truncated: cut.truncated,
    })
}
