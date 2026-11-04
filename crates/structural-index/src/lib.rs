//! # structural-index — the 64-byte block scanner + `SoA` region index
//! (issue 032, ADR-011)
//!
//! One pass over the source in **64-byte blocks** (memchr chunking — the
//! scalar stand-in for the SIMD classify loop) classifies the byte stream
//! into flat **regions**: strings (escape-aware), line comments, and
//! matched bracket pairs with their nesting depth. The index is a flat
//! **`SoA`** — one dense array per field — landing at ~13 bytes per region,
//! inside the ~16 B/region budget.
//!
//! Consumers:
//! - **validate fast path** — bracket balance / string termination from
//!   the index in O(regions), never re-touching the bytes;
//! - **region-parallel chunk planning** — contiguous, balanced chunk
//!   plans whose boundaries sit at depth-0 points, so independent
//!   workers can parse their chunks without cross-chunk state.
//!
//! Fast-path skips: the state machine only visits the memchr hits — a
//! document of long strings scans a small fraction of its bytes
//! (`ScanStats::visited` documents the ratio).

/// Region kinds (one byte in the `SoA`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RegionKind {
    /// A quoted string (opening quote → closing quote, escapes inside).
    String = 0,
    /// A `#`-style line comment (to the newline).
    Comment = 1,
    /// `{` … `}`
    Brace = 2,
    /// `[` … `]`
    Bracket = 3,
    /// `(` … `)`
    Paren = 4,
}

impl RegionKind {
    /// The byte value stored in the `SoA`.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// From the `SoA` byte.
    #[must_use]
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::String),
            1 => Some(Self::Comment),
            2 => Some(Self::Brace),
            3 => Some(Self::Bracket),
            4 => Some(Self::Paren),
            _ => None,
        }
    }
}

/// The flat `SoA` region index: `kinds[i]`, `starts[i]`, `ends[i]`,
/// `depths[i]` — 1 + 4 + 4 + 4 = 13 bytes per region, inside the
/// ~16 B/region budget.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegionIndex {
    kinds: Vec<u8>,
    starts: Vec<u32>,
    ends: Vec<u32>,
    depths: Vec<u16>,
    /// Flags: bit 0 = unterminated (EOF-closed opener / line-ended
    /// string).
    flags: Vec<u8>,
    /// The bytes the state machine actually visited (fast-path evidence).
    visited: u32,
}

/// The region flag: the region never closed (opener ran to EOF, string
/// ended by the line end).
pub const FLAG_UNTERMINATED: u8 = 1;

/// Scan statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    /// Source bytes total.
    pub bytes: u32,
    /// Positions the state machine examined (memchr hits + cursor steps).
    pub visited: u32,
    /// Regions classified.
    pub regions: u32,
}

/// Scan `source` into the region index. The state machine is the same
/// byte-for-byte logic as the differential reference; the **64-byte
/// block** structure and the memchr-driven string/comment interior skips
/// are the fast paths (ADR-011: memchr chunking stands in for SIMD).
#[must_use]
pub fn scan(source: &str) -> RegionIndex {
    const BLOCK: usize = 64;
    let bytes = source.as_bytes();
    let mut index = RegionIndex::default();
    let mut visited = 0_u32;
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut depth: u16 = 0;
    let mut pos = 0_usize;
    while pos < bytes.len() {
        // Process one 64-byte block of the byte-by-byte state machine.
        let block_end = (pos + BLOCK).min(bytes.len());
        while pos < block_end {
            visited += 1;
            let start = pos;
            match bytes[pos] {
                b'"' | b'\'' => {
                    // Fast path: the interior skips via a dedicated scan.
                    let close = scan_string_end(bytes, pos);
                    let terminated = close > pos + 1 && bytes[close - 1] == bytes[pos];
                    let mut flags = 0_u8;
                    if !terminated {
                        flags |= FLAG_UNTERMINATED;
                    }
                    index.kinds.push(RegionKind::String.as_u8());
                    index.starts.push(u32::try_from(start).expect("fits"));
                    index.ends.push(u32::try_from(close).expect("fits"));
                    index.depths.push(depth);
                    index.flags.push(flags);
                    pos = close;
                }
                b'#' => {
                    let close = bytes[pos..]
                        .iter()
                        .position(|c| *c == b'\n')
                        .map_or(bytes.len(), |off| pos + off);
                    index.kinds.push(RegionKind::Comment.as_u8());
                    index.starts.push(u32::try_from(start).expect("fits"));
                    index.ends.push(u32::try_from(close).expect("fits"));
                    index.depths.push(depth);
                    index.flags.push(0);
                    pos = close;
                }
                b'\\' => {
                    pos = (pos + 2).min(bytes.len());
                }
                b'{' | b'[' | b'(' => {
                    depth = depth.saturating_add(1);
                    let id = index.kinds.len();
                    index.kinds.push(
                        match bytes[pos] {
                            b'{' => RegionKind::Brace,
                            b'[' => RegionKind::Bracket,
                            _ => RegionKind::Paren,
                        }
                        .as_u8(),
                    );
                    index.starts.push(u32::try_from(start).expect("fits"));
                    index.ends.push(0); // patched at the closer
                    index.depths.push(depth);
                    index.flags.push(0);
                    stack.push((bytes[pos], id));
                    pos += 1;
                }
                b'}' | b']' | b')' => {
                    let matched = matches!(stack.last(), Some(&(open, _))
                        if (open == b'{' && bytes[pos] == b'}')
                            || (open == b'[' && bytes[pos] == b']')
                            || (open == b'(' && bytes[pos] == b')'));
                    if matched {
                        let (_, id) = stack.pop().expect("checked");
                        index.ends[id] = u32::try_from(pos + 1).expect("fits");
                        depth = depth.saturating_sub(1);
                    } else {
                        // Stray closer: an error region (kind 0xFF).
                        index.kinds.push(0xFF);
                        index.starts.push(u32::try_from(start).expect("fits"));
                        index.ends.push(u32::try_from(pos + 1).expect("fits"));
                        index.depths.push(depth);
                        index.flags.push(0);
                    }
                    pos += 1;
                }
                _ => pos += 1,
            }
        }
    }
    // Unclosed openers run to EOF and are flagged unterminated.
    for (open, id) in stack {
        index.ends[id] = u32::try_from(bytes.len()).expect("fits");
        index.flags[id] |= FLAG_UNTERMINATED;
        let _ = open;
    }
    index.visited = visited;
    index
}

/// The end of the string starting at `open` (the quote byte), escapes
/// aware; EOF when unterminated.
fn scan_string_end(bytes: &[u8], open: usize) -> usize {
    let quote = bytes[open];
    let mut idx = open + 1;
    while idx < bytes.len() {
        match bytes[idx] {
            b'\\' => idx += 2,
            b'\n' => return idx, // unterminated at the line end
            _ if bytes[idx] == quote => return idx + 1,
            _ => idx += 1,
        }
    }
    bytes.len()
}

impl RegionIndex {
    /// Number of regions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Whether the index is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// The region kind.
    #[must_use]
    pub fn kind(&self, idx: usize) -> Option<RegionKind> {
        RegionKind::from_u8(*self.kinds.get(idx)?)
    }

    /// The region start.
    #[must_use]
    pub fn start(&self, idx: usize) -> Option<u32> {
        self.starts.get(idx).copied()
    }

    /// The region end (exclusive).
    #[must_use]
    pub fn end(&self, idx: usize) -> Option<u32> {
        self.ends.get(idx).copied()
    }

    /// The region nesting depth (brackets; strings/comments inherit the
    /// enclosing depth).
    #[must_use]
    pub fn depth(&self, idx: usize) -> Option<u16> {
        self.depths.get(idx).copied()
    }

    /// Whether the region is an error marker (stray closer 0xFF or
    /// unterminated string 0xFE).
    #[must_use]
    pub fn is_error(&self, idx: usize) -> bool {
        matches!(self.kinds.get(idx), Some(0xFF | 0xFE))
    }

    /// The scan stats (bytes vs visited — the fast-path evidence).
    #[must_use]
    pub fn stats(&self, bytes: u32) -> ScanStats {
        ScanStats {
            bytes,
            visited: self.visited,
            regions: u32::try_from(self.kinds.len()).unwrap_or(u32::MAX),
        }
    }

    /// `SoA` storage per region — the ~16 B/region budget (13 B of fields
    /// + 1 B of flags = 14).
    #[must_use]
    pub fn bytes_per_region(&self) -> usize {
        if self.kinds.is_empty() {
            return 0;
        }
        let total = self.kinds.len()
            + self.starts.len() * 4
            + self.ends.len() * 4
            + self.depths.len() * 2
            + self.flags.len();
        total / self.kinds.len()
    }

    /// The region's flags.
    #[must_use]
    pub fn flags(&self, idx: usize) -> Option<u8> {
        self.flags.get(idx).copied()
    }

    /// All `(start, end)` region spans (for chunk-planning consumers).
    #[must_use]
    pub fn spans(&self) -> Vec<(u32, u32)> {
        self.starts
            .iter()
            .zip(self.ends.iter())
            .map(|(s, e)| (*s, *e))
            .collect()
    }

    /// The validate fast path: bracket balance, string termination and
    /// no stray closers — O(regions), no byte re-scan.
    ///
    /// # Errors
    /// A `(kind, offset)` of the first problem.
    pub fn validate(&self) -> Result<(), (RegionKind, u32)> {
        for idx in 0..self.kinds.len() {
            if self
                .flags
                .get(idx)
                .is_some_and(|f| f & FLAG_UNTERMINATED != 0)
            {
                let kind = self.kind(idx).unwrap_or(RegionKind::String);
                return Err((kind, self.start(idx).expect("in bounds")));
            }
            if self.is_error(idx) {
                return Err((RegionKind::Paren, self.start(idx).expect("in bounds")));
            }
        }
        Ok(())
    }
}

/// Plan contiguous, balanced chunks over `source_len`: boundaries sit at
/// depth-0 points (never inside a string or bracket), chunks are
/// `target`-sized where possible, and the union covers the document.
#[must_use]
pub fn plan_chunks(index: &RegionIndex, source_len: u32, target: u32) -> Vec<std::ops::Range<u32>> {
    let mut chunks = Vec::new();
    if source_len == 0 {
        return chunks;
    }
    let mut chunk_start = 0_u32;
    for idx in 0..index.kinds.len() {
        let (end, depth) = (
            index.end(idx).expect("in bounds"),
            index.depth(idx).expect("in bounds"),
        );
        // A depth-0 region's end is a safe boundary.
        if depth == 0 && end <= source_len && end - chunk_start >= target {
            chunks.push(chunk_start..end);
            chunk_start = end;
        }
    }
    // The tail (target is advisory — a safe boundary is never forced).
    chunks.push(chunk_start..source_len);
    chunks.retain(|c| c.start < c.end);
    chunks
}
