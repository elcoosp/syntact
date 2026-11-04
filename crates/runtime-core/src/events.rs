//! The 8-byte event protocol (roadmap §7.1, rust-analyzer lineage).
//!
//! A parser never builds trees directly: it emits a flat stream of
//! [`Event`]s, which [`process`] validates and packs into an [`Output`] of
//! 8-byte words. Consumers flatten the words into [`Step`]s and drive any
//! [`crate::sink::Sink`].
//!
//! # The events
//!
//! - [`Event::Start`] — enter a node of `kind`; `forward_parent` forwards
//!   the node's final kind to a later `Start` (see below).
//! - [`Event::Token`] — a leaf token of `kind` covering `n_raw` raw tokens.
//! - [`Event::Error`] — a deferred diagnostic; `diag` indexes the side
//!   table ([`crate::diagnostic::DiagTable`]).
//! - [`Event::Finish`] — close the innermost open node (1:1 with
//!   [`Step::Exit`]); every `Start` needs exactly one.
//!
//! # Forward parents (kind forwarding)
//!
//! `Start { kind: K1, forward_parent: Some(k) }` at index `i` is a
//! *placeholder*: its node's final kind comes from the `Start` at index
//! `i + k` (following the chain if that event also carries a
//! `forward_parent`). The node is ENTERED at position `i` with the
//! chain-final kind; the chain's intermediate `Start` events are skipped
//! when reached. This is how a parser that discovers the wrapping
//! construct late (`a + b` — the binary node announced only after the
//! operator is seen) keeps a single DFS pass: the content between the
//! placeholder and the wrapper lands inside the (renamed) node, exactly
//! where DFS order wants it.
//!
//! # Word encoding
//!
//! Each [`Output`] word is `u64`: tag in the low 4 bits, payload above.
//! Tags: `1` = enter, `2` = exit, `3` = token (kind: 16 bits, `n_raw`: 8
//! bits), `4` = error (diag: 28 bits — [`Event::Error`] values that do
//! not fit are rejected by [`process`]).

use std::num::NonZeroU32;

/// Tag of an enter (node start) word.
pub const TAG_ENTER: u64 = 1;
/// Tag of an exit (node end) word.
pub const TAG_EXIT: u64 = 2;
/// Tag of a token word.
pub const TAG_TOKEN: u64 = 3;
/// Tag of an error word.
pub const TAG_ERROR: u64 = 4;

/// Maximum diagnostic index the packed word can carry (28-bit payload).
pub const MAX_DIAG: u32 = (1 << 28) - 1;

/// A parse event, pre-packing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Enter a node of `kind`; `forward_parent` forwards the final kind to
    /// the `Start` event `k` events later (1-based relative offset).
    Start {
        /// Node kind (overridden by the forward-parent chain end).
        kind: u16,
        /// Relative offset (≥ 1) to the kind-forwarding parent event.
        forward_parent: Option<NonZeroU32>,
    },
    /// Close the innermost open node.
    Finish,
    /// A leaf token of `kind` covering `n_raw` raw tokens.
    Token {
        /// Token kind.
        kind: u16,
        /// Number of raw lexemes folded into this token.
        n_raw: u8,
    },
    /// A deferred diagnostic (side-table index).
    Error {
        /// Side-table index; must be ≤ [`MAX_DIAG`].
        diag: u32,
    },
}

/// A flattened, unpacked step of an [`Output`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Enter a node of `kind`.
    Enter {
        /// Node kind.
        kind: u16,
    },
    /// Exit the innermost open node.
    Exit,
    /// A leaf token.
    Token {
        /// Token kind.
        kind: u16,
        /// Raw tokens folded in.
        n_raw: u8,
    },
    /// A deferred diagnostic.
    Error {
        /// Side-table index.
        diag: u32,
    },
}

/// A validated, packed event stream: one 8-byte word per step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    /// The packed words (public for zero-copy consumers; always treat via
    /// [`Self::steps`] unless writing tests/fuzzers).
    pub stream: Vec<u64>,
}

impl Output {
    /// Pack an enter step.
    #[must_use]
    pub fn pack_enter(kind: u16) -> u64 {
        (u64::from(kind) << 4) | TAG_ENTER
    }

    /// Pack an exit step.
    #[must_use]
    pub fn pack_exit() -> u64 {
        TAG_EXIT
    }

    /// Pack a token step.
    #[must_use]
    pub fn pack_token(kind: u16, n_raw: u8) -> u64 {
        (u64::from(kind) << 12) | (u64::from(n_raw) << 4) | TAG_TOKEN
    }

    /// Pack an error step.
    #[must_use]
    pub fn pack_error(diag: u32) -> u64 {
        (u64::from(diag) << 4) | TAG_ERROR
    }

    /// Iterate the unpacked steps in DFS order.
    pub fn steps(&self) -> impl Iterator<Item = Step> + '_ {
        self.stream.iter().copied().map(Self::unpack_word)
    }

    /// Unpack one word into a step (tag/payload inverse of the `pack_*`
    /// constructors; words only ever come from [`process`] or test helpers).
    #[must_use]
    pub fn unpack_word(word: u64) -> Step {
        let tag = word & 0xF;
        match tag {
            TAG_ENTER => Step::Enter {
                kind: u16::try_from(word >> 4).unwrap_or(u16::MAX),
            },
            TAG_EXIT => Step::Exit,
            TAG_TOKEN => Step::Token {
                kind: u16::try_from((word >> 12) & 0xFFFF).unwrap_or(u16::MAX),
                n_raw: u8::try_from((word >> 4) & 0xFF).unwrap_or(0),
            },
            _ => Step::Error {
                diag: u32::try_from(word >> 4).unwrap_or(u32::MAX),
            },
        }
    }
}

/// Validate and pack an event stream.
///
/// # Errors
/// A descriptive [`String`] when the stream is malformed: a `Finish` with
/// no open node, a `Finish` left unclosed at stream end, a forward parent
/// pointing out of range or at a non-`Start`, or a diag index over
/// [`MAX_DIAG`].
pub fn process(events: &[Event]) -> Result<Output, String> {
    let mut stream = Vec::with_capacity(events.len());
    // Indices of Start events already folded into a forward-parent chain
    // (they are skipped when reached). Allocated lazily: streams without
    // forward parents (every reference pack) never pay the N-byte zeroing.
    let mut folded: Vec<bool> = Vec::new();
    // Open nodes: index of the Start event (for balance checking).
    let mut open: Vec<usize> = Vec::with_capacity(16);

    for (idx, event) in events.iter().enumerate() {
        if !folded.is_empty() && folded[idx] {
            continue;
        }
        match *event {
            Event::Start {
                kind,
                forward_parent,
            } => {
                // Resolve the kind-forwarding chain: each hop's offset is
                // relative to the *previous* hop (rust-analyzer shape);
                // the chain-final Start's kind wins; intermediates fold.
                let mut final_kind = kind;
                let mut base = idx;
                let mut fp = forward_parent;
                if fp.is_some() && folded.is_empty() {
                    folded.resize(events.len(), false);
                }
                while let Some(offset) = fp {
                    let target = base
                        + usize::try_from(offset.get()).map_err(|_| {
                            format!(
                                "forward-parent offset {} overflows at event {idx}",
                                offset.get()
                            )
                        })?;
                    if target >= events.len() {
                        return Err(format!(
                            "forward parent of event {base} points past the stream ({target})"
                        ));
                    }
                    match events[target] {
                        Event::Start {
                            kind: k2,
                            forward_parent: fp2,
                        } => {
                            final_kind = k2;
                            folded[target] = true;
                            base = target;
                            fp = fp2;
                        }
                        _ => {
                            return Err(format!(
                                "forward parent of event {base} (target {target}) is not a Start"
                            ));
                        }
                    }
                }
                stream.push(Output::pack_enter(final_kind));
                open.push(idx);
            }
            Event::Finish => {
                if open.pop().is_none() {
                    return Err(format!("Finish at event {idx} closes nothing"));
                }
                stream.push(Output::pack_exit());
            }
            Event::Token { kind, n_raw } => stream.push(Output::pack_token(kind, n_raw)),
            Event::Error { diag } => {
                if diag > MAX_DIAG {
                    return Err(format!(
                        "diag index {diag} exceeds the {MAX_DIAG} packed-word bound"
                    ));
                }
                stream.push(Output::pack_error(diag));
            }
        }
    }
    if let Some(&idx) = open.last() {
        return Err(format!(
            "stream ends with {} node(s) still open (first at event {idx})",
            open.len()
        ));
    }
    Ok(Output { stream })
}

#[cfg(test)]
mod event_tests {
    use super::{Event, Step, process};
    use std::num::NonZeroU32;

    fn fp(n: u32) -> Option<NonZeroU32> {
        NonZeroU32::new(n)
    }

    fn enter(kind: u16) -> Event {
        Event::Start {
            kind,
            forward_parent: None,
        }
    }

    fn tok(kind: u16) -> Event {
        Event::Token { kind, n_raw: 1 }
    }

    #[test]
    fn event_is_8_bytes() {
        assert_eq!(
            std::mem::size_of::<Event>(),
            8,
            "the event protocol pins Event at 8 bytes (§7.1)"
        );
    }

    #[test]
    fn flat_tokens_pass_through() {
        let events = vec![tok(1), Event::Token { kind: 2, n_raw: 3 }];
        let output = process(&events).expect("flat stream is well-formed");
        let steps: Vec<Step> = output.steps().collect();
        assert_eq!(
            steps,
            vec![
                Step::Token { kind: 1, n_raw: 1 },
                Step::Token { kind: 2, n_raw: 3 },
            ]
        );
    }

    #[test]
    fn nested_nodes_dfs_order() {
        let events = vec![
            enter(10),
            tok(1),
            enter(11),
            tok(2),
            Event::Finish,
            Event::Finish,
        ];
        let output = process(&events).expect("balanced stream");
        let steps: Vec<Step> = output.steps().collect();
        assert_eq!(
            steps,
            vec![
                Step::Enter { kind: 10 },
                Step::Token { kind: 1, n_raw: 1 },
                Step::Enter { kind: 11 },
                Step::Token { kind: 2, n_raw: 1 },
                Step::Exit,
                Step::Exit,
            ]
        );
    }

    #[test]
    fn forward_parent_wraps_subtree() {
        // `a + b`: the lhs-position placeholder gets its final kind from
        // the BINARY wrapper two events later; the wrapper Start is folded.
        let events = vec![
            Event::Start {
                kind: 1,
                forward_parent: fp(2),
            },
            tok(5),
            enter(20),
            tok(9),
            enter(1),
            tok(5),
            Event::Finish,
            Event::Finish,
        ];
        let output = process(&events).expect("wrapped stream is balanced");
        let steps: Vec<Step> = output.steps().collect();
        assert_eq!(
            steps,
            vec![
                Step::Enter { kind: 20 }, // forwarded kind
                Step::Token { kind: 5, n_raw: 1 },
                // The wrapper Start event was folded (skipped).
                Step::Token { kind: 9, n_raw: 1 },
                Step::Enter { kind: 1 },
                Step::Token { kind: 5, n_raw: 1 },
                Step::Exit,
                Step::Exit,
            ]
        );
    }

    #[test]
    fn deep_forward_parent_chain_nests_three_levels() {
        // Chain: placeholder → wrapper → grand-wrapper; only ONE node is
        // entered, with the grand-wrapper's kind.
        let events = vec![
            Event::Start {
                kind: 1,
                forward_parent: fp(2),
            },
            tok(2),
            Event::Start {
                kind: 2,
                forward_parent: fp(1),
            },
            enter(3),
            Event::Finish,
        ];
        // enter(3-with-chain) ... tok ... finish → balanced: 1 enter, 1 exit.
        let output = process(&events).expect("chain resolves and balances");
        let steps: Vec<Step> = output.steps().collect();
        assert_eq!(
            steps,
            vec![
                Step::Enter { kind: 3 },
                Step::Token { kind: 2, n_raw: 1 },
                Step::Exit,
            ]
        );
    }

    #[test]
    fn forward_parent_out_of_range_is_error() {
        let events = vec![
            Event::Start {
                kind: 1,
                forward_parent: fp(9),
            },
            Event::Finish,
        ];
        assert!(process(&events).is_err());
    }

    #[test]
    fn forward_parent_at_non_start_is_error() {
        let events = vec![
            Event::Start {
                kind: 1,
                forward_parent: fp(1),
            },
            tok(2),
            Event::Finish,
        ];
        assert!(process(&events).is_err());
    }

    #[test]
    fn error_events_reach_validation_sink() {
        let events = vec![enter(1), Event::Error { diag: 7 }, tok(2), Event::Finish];
        let output = process(&events).expect("error-bearing stream is well-formed");
        let steps: Vec<Step> = output.steps().collect();
        assert!(steps.contains(&Step::Error { diag: 7 }));
    }

    #[test]
    fn oversized_diag_index_is_rejected() {
        let events = vec![Event::Error {
            diag: super::MAX_DIAG + 1,
        }];
        let err = process(&events).expect_err("oversized diag must be rejected");
        assert!(err.contains("exceeds"), "{err}");
    }

    #[test]
    fn unbalanced_stream_is_error() {
        let events = vec![enter(1), enter(2)];
        assert!(process(&events).is_err());
        let events = vec![Event::Finish, enter(1), Event::Finish];
        assert!(process(&events).is_err());
    }

    /// Tiny deterministic LCG — property tests stay reproducible without a
    /// RNG dependency.
    struct Lcg(u64);

    impl Lcg {
        fn below(&mut self, n: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (self.0 >> 11) % n.max(1)
        }
    }

    #[test]
    fn property_valid_streams_process_ok_and_balance() {
        let mut rng = Lcg(0x5EED_C0FF_EE12_3456);
        for _ in 0..300 {
            // Build a random *balanced* event list by mirroring each Start
            // with a Finish (a stack discipline on the generator side).
            let mut events: Vec<Event> = Vec::new();
            let mut to_close = 0u32;
            for _ in 0..=rng.below(40) {
                match rng.below(3) {
                    0 => {
                        events.push(enter(u16::try_from(rng.below(40)).expect("kind fits")));
                        to_close += 1;
                    }
                    1 => events.push(tok(u16::try_from(rng.below(20)).expect("kind fits"))),
                    _ if to_close > 0 => {
                        events.push(Event::Error {
                            diag: u32::try_from(rng.below(8)).expect("diag fits"),
                        });
                    }
                    _ => {}
                }
            }
            for _ in 0..to_close {
                events.push(Event::Finish);
            }
            let output = process(&events).expect("generated stream is balanced");
            let steps: Vec<Step> = output.steps().collect();
            assert_eq!(steps.len(), events.len(), "one word per event");
        }
    }

    #[test]
    fn property_mutated_streams_never_panic() {
        let mut rng = Lcg(0xBADC_0DE1_2345_6789);
        for _ in 0..300 {
            let mut events: Vec<Event> = Vec::new();
            for _ in 0..rng.below(20) {
                match rng.below(5) {
                    0 => events.push(Event::Start {
                        kind: u16::try_from(rng.below(40)).expect("fits"),
                        forward_parent: fp(u32::try_from(1 + rng.below(5)).expect("fits")),
                    }),
                    1 => events.push(Event::Token {
                        kind: u16::try_from(rng.below(20)).expect("fits"),
                        n_raw: u8::try_from(1 + rng.below(3)).expect("fits"),
                    }),
                    2 => events.push(Event::Error {
                        diag: u32::try_from(rng.below(16)).expect("fits"),
                    }),
                    3 => events.push(Event::Finish),
                    _ => events.push(enter(u16::try_from(rng.below(40)).expect("fits"))),
                }
            }
            // The only contract: never panic, always a Result.
            let _ = process(&events);
        }
    }
}
