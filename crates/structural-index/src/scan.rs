//! The 64-byte block scanner (ADR-011): memchr chunking standing in for
//! SIMD, plus the byte-by-byte reference the tests fuzz against.
//!
//! The state machine drives a [`RegionSink`] (see [`crate::sink`]);
//! `scan` is the index-collecting consumer.

use std::ops::ControlFlow;

use crate::index::{RegionIndex, RegionKind};
use crate::sink::RegionSink;

/// The block size the scanner advances in (ADR-011).
pub const BLOCK: usize = 64;

/// Scan `source` into regions with the 64-byte block scanner.
#[must_use]
pub fn scan(source: &str) -> RegionIndex {
    let mut index = RegionIndex::new();
    let _ = Scanner::new(source, &mut index).run();
    index
}

/// The byte-by-byte reference scanner: same regions, naive loop. The
/// fuzz suite pins the block scanner to this.
#[must_use]
pub fn scan_reference(source: &str) -> RegionIndex {
    let bytes = source.as_bytes();
    let mut index = RegionIndex::new();
    let mut depth: u8 = 0;
    let mut region_start = 0_usize;
    let mut region_kind = RegionKind::Code;
    let mut flush =
        |end: usize, kind: RegionKind, start: usize, depth: u8, out: &mut RegionIndex| {
            if end > start {
                let s = u32::try_from(start).unwrap_or(u32::MAX);
                let e = u32::try_from(end).unwrap_or(u32::MAX);
                out.push(kind, s, e, depth);
            }
        };
    let flush_ref = &mut flush;
    let mut i = 0_usize;
    while i < bytes.len() {
        match region_kind {
            RegionKind::Code => match bytes[i] {
                b'"' => {
                    flush_ref(i, RegionKind::Code, region_start, depth, &mut index);
                    region_start = i;
                    region_kind = RegionKind::StrDouble;
                    i += 1;
                }
                b'\'' => {
                    flush_ref(i, RegionKind::Code, region_start, depth, &mut index);
                    region_start = i;
                    region_kind = RegionKind::StrSingle;
                    i += 1;
                }
                b'`' => {
                    flush_ref(i, RegionKind::Code, region_start, depth, &mut index);
                    region_start = i;
                    region_kind = RegionKind::StrBacktick;
                    i += 1;
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => {
                    flush_ref(i, RegionKind::Code, region_start, depth, &mut index);
                    region_start = i;
                    region_kind = RegionKind::LineComment;
                    i += 2;
                }
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    flush_ref(i, RegionKind::Code, region_start, depth, &mut index);
                    region_start = i;
                    region_kind = RegionKind::BlockComment;
                    i += 2;
                }
                b'{' => {
                    depth = depth.saturating_add(1);
                    i += 1;
                }
                b'}' => {
                    depth = depth.saturating_sub(1);
                    i += 1;
                }
                _ => i += 1,
            },
            RegionKind::StrDouble | RegionKind::StrSingle | RegionKind::StrBacktick => {
                match bytes[i] {
                    b'\\' => i += 2,
                    b if b == quote_of(region_kind) => {
                        i += 1;
                        flush_ref(i, region_kind, region_start, depth, &mut index);
                        region_start = i;
                        region_kind = RegionKind::Code;
                    }
                    _ => i += 1,
                }
            }
            RegionKind::LineComment => match bytes[i] {
                b'\n' => {
                    flush_ref(i, region_kind, region_start, depth, &mut index);
                    region_start = i;
                    region_kind = RegionKind::Code;
                    i += 1;
                }
                _ => i += 1,
            },
            RegionKind::BlockComment => {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    i += 2;
                    flush_ref(i, region_kind, region_start, depth, &mut index);
                    region_start = i;
                    region_kind = RegionKind::Code;
                } else {
                    i += 1;
                }
            }
        }
    }
    flush_ref(bytes.len(), region_kind, region_start, depth, &mut index);
    index
}

fn quote_of(kind: RegionKind) -> u8 {
    match kind {
        RegionKind::StrDouble => b'"',
        RegionKind::StrSingle => b'\'',
        RegionKind::StrBacktick => b'`',
        _ => 0,
    }
}

/// The block-scanning state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Code,
    StrDouble,
    StrSingle,
    StrBacktick,
    LineComment,
    BlockComment,
}

impl State {
    pub(crate) fn kind(self) -> RegionKind {
        match self {
            State::Code => RegionKind::Code,
            State::StrDouble => RegionKind::StrDouble,
            State::StrSingle => RegionKind::StrSingle,
            State::StrBacktick => RegionKind::StrBacktick,
            State::LineComment => RegionKind::LineComment,
            State::BlockComment => RegionKind::BlockComment,
        }
    }
}

/// Find the first byte that changes scanner state: quotes, `/`, braces,
/// backslash. memchr-based, 64-byte-window friendly.
pub(crate) fn find_special(window: &[u8]) -> Option<usize> {
    // memchr over a small candidate set: pick the earliest hit.
    let candidates = [
        memchr::memchr(b'"', window),
        memchr::memchr(b'\'', window),
        memchr::memchr(b'`', window),
        memchr::memchr(b'/', window),
        memchr::memchr(b'{', window),
        memchr::memchr(b'}', window),
        memchr::memchr(b'\\', window),
    ];
    candidates.into_iter().flatten().min()
}

/// The block-scanning state machine, driving a [`RegionSink`] instead
/// of collecting into an index — the single copy of the ADR-011
/// machine; `scan` and every streaming consumer share it.
pub(crate) struct Scanner<'s> {
    bytes: &'s [u8],
    sink: &'s mut dyn RegionSink,
    state: State,
    depth: u8,
    region_start: usize,
}

impl<'s> Scanner<'s> {
    pub(crate) fn new(source: &'s str, sink: &'s mut dyn RegionSink) -> Self {
        Self {
            bytes: source.as_bytes(),
            sink,
            state: State::Code,
            depth: 0,
            region_start: 0,
        }
    }

    /// Flush the region `[region_start, end)` into the sink (skipped
    /// when empty), propagating a sink-side break.
    fn flush(&mut self, end: usize) -> ControlFlow<()> {
        if end > self.region_start {
            let kind = self.state.kind();
            let start = u32::try_from(self.region_start).unwrap_or(u32::MAX);
            let end32 = u32::try_from(end).unwrap_or(u32::MAX);
            let depth = self.depth;
            self.sink.region(kind, start, end32, depth)
        } else {
            ControlFlow::Continue(())
        }
    }

    pub(crate) fn run(mut self) -> ControlFlow<()> {
        let len = self.bytes.len();
        let mut block_start = 0_usize;
        while block_start < len {
            let block_end = (block_start + BLOCK).min(len);
            self.scan_block(block_start, block_end)?;
            block_start = block_end;
        }
        self.flush(len)?;
        ControlFlow::Continue(())
    }

    /// One 64-byte block: in `Code` state, memchr locates the next
    /// interesting byte and the bytes before it batch into the current
    /// code region (the ADR-011 chunking stand-in for SIMD).
    fn scan_block(&mut self, block_start: usize, block_end: usize) -> ControlFlow<()> {
        let mut pos = block_start;
        while pos < block_end {
            match self.state {
                State::Code => {
                    // Candidates: quotes, comment slash, escapes, braces.
                    let window = &self.bytes[pos..block_end];
                    let hit = find_special(window);
                    match hit {
                        None => {
                            pos = block_end; // the whole block is code: skip
                        }
                        Some(offset) => {
                            let absolute = pos + offset;
                            match self.bytes[absolute] {
                                b'"' | b'\'' | b'`' => {
                                    self.flush(absolute)?;
                                    self.region_start = absolute;
                                    self.state = match self.bytes[absolute] {
                                        b'"' => State::StrDouble,
                                        b'\'' => State::StrSingle,
                                        _ => State::StrBacktick,
                                    };
                                    // Strings may run past the block: scan
                                    // to their terminator directly (from
                                    // AFTER the opening quote).
                                    pos = self.scan_string_from(absolute + 1, block_end)?;
                                }
                                b'/' => {
                                    if self.bytes.get(absolute + 1) == Some(&b'/') {
                                        self.flush(absolute)?;
                                        self.region_start = absolute;
                                        self.state = State::LineComment;
                                        pos =
                                            self.scan_line_comment_from(absolute + 2, block_end)?;
                                    } else if self.bytes.get(absolute + 1) == Some(&b'*') {
                                        self.flush(absolute)?;
                                        self.region_start = absolute;
                                        self.state = State::BlockComment;
                                        pos =
                                            self.scan_block_comment_from(absolute + 2, block_end)?;
                                    } else {
                                        pos = absolute + 1; // plain `/`
                                    }
                                }
                                b'{' => {
                                    self.depth = self.depth.saturating_add(1);
                                    pos = absolute + 1;
                                }
                                b'}' => {
                                    self.depth = self.depth.saturating_sub(1);
                                    pos = absolute + 1;
                                }
                                _ => pos = absolute + 1, // `\` in code: rare, byte-step
                            }
                        }
                    }
                }
                State::StrDouble | State::StrSingle | State::StrBacktick => {
                    pos = self.scan_string_from(pos, block_end)?;
                }
                State::LineComment => {
                    pos = self.scan_line_comment_from(pos, block_end)?;
                }
                State::BlockComment => {
                    pos = self.scan_block_comment_from(pos, block_end)?;
                }
            }
        }
        ControlFlow::Continue(())
    }

    /// Scan a string from `pos` (inside the quote); returns the position
    /// to continue at, switching back to `Code` on the closing quote.
    fn scan_string_from(&mut self, pos: usize, block_end: usize) -> ControlFlow<(), usize> {
        let quote = quote_of(self.state.kind());
        let mut i = pos;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b'\\' => i += 2,
                b if b == quote => {
                    i += 1;
                    self.flush(i)?;
                    self.region_start = i;
                    self.state = State::Code;
                    return ControlFlow::Continue(i.min(block_end.max(i)));
                }
                _ => i += 1,
            }
            if i >= block_end {
                return ControlFlow::Continue(i); // continue in the next block, same state
            }
        }
        ControlFlow::Continue(i)
    }

    fn scan_line_comment_from(&mut self, pos: usize, block_end: usize) -> ControlFlow<(), usize> {
        let mut i = pos;
        while i < self.bytes.len() {
            if self.bytes[i] == b'\n' {
                self.flush(i)?;
                self.region_start = i;
                self.state = State::Code;
                return ControlFlow::Continue((i + 1).min(block_end.max(i + 1)));
            }
            i += 1;
            if i >= block_end {
                return ControlFlow::Continue(i);
            }
        }
        ControlFlow::Continue(i)
    }

    fn scan_block_comment_from(&mut self, pos: usize, block_end: usize) -> ControlFlow<(), usize> {
        let mut i = pos;
        while i + 1 < self.bytes.len() {
            if self.bytes[i] == b'*' && self.bytes[i + 1] == b'/' {
                i += 2;
                self.flush(i)?;
                self.region_start = i;
                self.state = State::Code;
                return ControlFlow::Continue(i.min(block_end.max(i)));
            }
            i += 1;
            if i >= block_end {
                return ControlFlow::Continue(i);
            }
        }
        ControlFlow::Continue(self.bytes.len().min(block_end.max(i)))
    }
}
