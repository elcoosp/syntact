//! The `SoA` region index: four parallel vectors, ~16 B/region budget.

/// The classification of one source region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RegionKind {
    /// Plain code bytes.
    Code = 0,
    /// A `"…"` string.
    StrDouble = 1,
    /// A `'…'` string.
    StrSingle = 2,
    /// A backtick string.
    StrBacktick = 3,
    /// A `// …` line comment (terminator: the newline, excluded).
    LineComment = 4,
    /// A `/* … */` block comment (delimiters included).
    BlockComment = 5,
}

impl RegionKind {
    /// From the stored byte.
    #[must_use]
    pub fn from_u8(byte: u8) -> Self {
        match byte {
            1 => Self::StrDouble,
            2 => Self::StrSingle,
            3 => Self::StrBacktick,
            4 => Self::LineComment,
            5 => Self::BlockComment,
            _ => Self::Code,
        }
    }
}

/// Flat structure-of-arrays region index: `kinds[i]` covers
/// `[starts[i], ends[i])` at brace depth `depths[i]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegionIndex {
    /// Region classification bytes ([`RegionKind`]).
    pub kinds: Vec<u8>,
    /// Region start offsets.
    pub starts: Vec<u32>,
    /// Region end offsets (exclusive).
    pub ends: Vec<u32>,
    /// Brace depth at the region's start.
    pub depths: Vec<u8>,
}

impl RegionIndex {
    /// An empty index.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one region.
    pub fn push(&mut self, kind: RegionKind, start: u32, end: u32, depth: u8) {
        self.kinds.push(kind as u8);
        self.starts.push(start);
        self.ends.push(end);
        self.depths.push(depth);
    }

    /// The region count.
    #[must_use]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Whether nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// The `i`-th region: `(kind, start, end, depth)`.
    #[must_use]
    pub fn get(&self, index: usize) -> Option<(RegionKind, u32, u32, u8)> {
        let kind = RegionKind::from_u8(*self.kinds.get(index)?);
        Some((
            kind,
            *self.starts.get(index)?,
            *self.ends.get(index)?,
            *self.depths.get(index)?,
        ))
    }

    /// Iterate all regions as `(kind, start, end, depth)` tuples.
    ///
    /// # Panics
    /// Never: the range is bounded by [`Self::len`] and `get` succeeds
    /// for every index below it.
    pub fn iter(&self) -> impl Iterator<Item = (RegionKind, u32, u32, u8)> + '_ {
        (0..self.len()).map(|i| self.get(i).expect("index < len"))
    }

    /// The brace depth at `offset` (the depth of the region containing
    /// it — the scan order guarantees ascending starts).
    #[must_use]
    pub fn depth_at(&self, offset: u32) -> u8 {
        let mut depth = 0;
        for (i, start) in self.starts.iter().enumerate() {
            if *start > offset {
                break;
            }
            depth = self.depths[i];
        }
        depth
    }

    /// Actual bytes per region including vector capacities — the
    /// ~16 B/region budget this crate asserts.
    #[must_use]
    pub fn bytes_per_region(&self) -> usize {
        if self.is_empty() {
            return 0;
        }
        let total = self.kinds.capacity()
            + self.starts.capacity() * std::mem::size_of::<u32>()
            + self.ends.capacity() * std::mem::size_of::<u32>()
            + self.depths.capacity();
        total / self.len()
    }
}

#[cfg(test)]
mod tests {
    use super::{RegionIndex, RegionKind};

    #[test]
    fn round_trip_preserves_regions() {
        let mut index = RegionIndex::new();
        index.push(RegionKind::Code, 0, 5, 0);
        index.push(RegionKind::StrDouble, 5, 11, 0);
        index.push(RegionKind::Code, 11, 12, 1);
        let round: Vec<_> = index.iter().collect();
        assert_eq!(
            round,
            vec![
                (RegionKind::Code, 0, 5, 0),
                (RegionKind::StrDouble, 5, 11, 0),
                (RegionKind::Code, 11, 12, 1),
            ]
        );
        assert_eq!(index.get(1), Some((RegionKind::StrDouble, 5, 11, 0)));
        assert_eq!(index.get(3), None);
        assert_eq!(index.depth_at(7), 0);
        assert_eq!(index.depth_at(11), 1);
    }
}
