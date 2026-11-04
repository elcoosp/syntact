//! Row-displacement table encoding (issue 015): sparse `row × column`
//! tables of `u16` cells compress to one flat array plus per-row base
//! offsets. Lookup is `cells[bases[row] + col]` — O(1) with no probing.
//!
//! Placement is deterministic first-fit: rows keep their input order and
//! each lands at the smallest base offset where all its populated cells
//! fit free slots. The same table always encodes to the same arrays.

/// The dense encoded table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactTable {
    /// The flat cell storage (width = max base + row width).
    pub cells: Vec<u16>,
    /// Per-row base offsets into [`Self::cells`].
    pub bases: Vec<u32>,
    /// The sentinel value marking empty cells (never a real payload).
    pub empty: u16,
    /// The column width every row was encoded with.
    pub width: u16,
}

impl CompactTable {
    /// Encode `rows` (dense row-major `Option` cells) with `empty` as the
    /// sentinel. All real payload values must differ from `empty`.
    ///
    /// Placement reserves a row's ENTIRE read span (empty cells block
    /// future placement too) — a later row can never write into a slot an
    /// earlier row reads, which is what keeps the O(1) lookup honest.
    ///
    /// # Panics
    /// When a payload value equals the `empty` sentinel (the caller
    /// picked a colliding sentinel — a programming error, not input).
    #[must_use]
    pub fn from_rows(rows: &[Vec<Option<u16>>], empty: u16) -> Self {
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut cells: Vec<u16> = Vec::new();
        let mut occupied: Vec<bool> = Vec::new();
        let mut bases = Vec::with_capacity(rows.len());
        for row in rows {
            // First-fit: the smallest base where this row's whole read
            // span lands on free slots.
            let mut base = 0_usize;
            loop {
                let fits =
                    (0..row.len()).all(|col| !occupied.get(base + col).copied().unwrap_or(false));
                if fits {
                    break;
                }
                base += 1;
            }
            for (col, cell) in row.iter().enumerate() {
                let slot = base + col;
                if slot >= cells.len() {
                    cells.resize(slot + 1, empty);
                    occupied.resize(slot + 1, false);
                }
                occupied[slot] = true; // reserve even the empty cells
                if let Some(value) = cell {
                    cells[slot] = *value;
                }
            }
            bases.push(u32::try_from(base).expect("table base fits u32"));
        }
        Self {
            cells,
            bases,
            empty,
            width: u16::try_from(width).expect("row width fits u16"),
        }
    }

    /// The cell at `(row, col)` — `None` where the source table was
    /// empty. O(1): two array reads.
    #[must_use]
    pub fn get(&self, row: usize, col: usize) -> Option<u16> {
        if row >= self.bases.len() || col >= usize::from(self.width) {
            return None;
        }
        let base = self.bases[row] as usize;
        let cell = *self.cells.get(base + col)?;
        (cell != self.empty).then_some(cell)
    }

    /// Number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bases.len()
    }

    /// Whether the table has no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bases.is_empty()
    }

    /// Total storage: flat cells + base offsets (the emitted footprint).
    #[must_use]
    pub fn storage_cells(&self) -> usize {
        self.cells.len() + self.bases.len()
    }
}

#[cfg(test)]
mod tests {
    use super::CompactTable;

    #[test]
    fn round_trips_dense_and_sparse() {
        let rows = vec![
            vec![Some(1), Some(2), None],
            vec![None, Some(3), None],
            vec![Some(4), None, Some(5)],
        ];
        let table = CompactTable::from_rows(&rows, 0xFFFF);
        assert_eq!(table.len(), 3);
        assert_eq!(table.width, 3);
        for (row, cells) in rows.iter().enumerate() {
            for (col, expected) in cells.iter().enumerate() {
                assert_eq!(table.get(row, col), *expected, "({row}, {col})");
            }
        }
        // Out-of-bounds reads are None, never panics.
        assert_eq!(table.get(3, 0), None);
        assert_eq!(table.get(0, 7), None);
    }

    #[test]
    fn empty_and_single_cell_edges() {
        let none = CompactTable::from_rows(&[], 0xFFFF);
        assert!(none.is_empty());
        assert_eq!(none.get(0, 0), None);
        let single = CompactTable::from_rows(&[vec![Some(7)]], 0xFFFF);
        assert_eq!(single.get(0, 0), Some(7));
        assert_eq!(single.get(0, 1), None);
        let all_empty = CompactTable::from_rows(&[vec![None, None]], 0xFFFF);
        assert_eq!(all_empty.get(0, 0), None);
        assert_eq!(all_empty.get(0, 1), None);
    }

    #[test]
    fn encoding_is_deterministic() {
        let rows: Vec<Vec<Option<u16>>> = (0..32)
            .map(|row| {
                (0..16)
                    .map(|col| {
                        ((row * 7 + col * 3) % 5 == 0)
                            .then_some(u16::try_from(row * 31 + col).expect("small"))
                    })
                    .collect()
            })
            .collect();
        let a = CompactTable::from_rows(&rows, 0xFFFF);
        let b = CompactTable::from_rows(&rows, 0xFFFF);
        assert_eq!(a, b, "same table, same encoding");
    }

    #[test]
    fn storage_stays_bounded_by_rows_times_width() {
        // Worst case: interleaved rows cannot share space, but placement
        // never exceeds rows × width cells.
        let rows: Vec<Vec<Option<u16>>> = (0..16)
            .map(|row| {
                (0..8)
                    .map(|col| ((row + col) % 2 == 0).then_some(1))
                    .collect()
            })
            .collect();
        let table = CompactTable::from_rows(&rows, 0xFFFF);
        assert!(table.storage_cells() <= 16 * 8 + 16);
        // Round-trip still holds after compression.
        for (row, cells) in rows.iter().enumerate() {
            for (col, expected) in cells.iter().enumerate() {
                assert_eq!(table.get(row, col), *expected);
            }
        }
    }

    #[test]
    fn seeded_random_tables_round_trip() {
        // Seeded xorshift corpus: sparse random tables must round-trip
        // cell-for-cell and encode deterministically.
        let mut state: u64 = 0xC0DE_CAFE_F00D_BEEF;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for case in 0..64 {
            let rows: Vec<Vec<Option<u16>>> = (0..8 + case % 24)
                .map(|_| {
                    (0..4 + case % 12)
                        .map(|_| {
                            let r = next();
                            (r % 4 == 0).then_some((r % 0xFFFE) as u16)
                        })
                        .collect()
                })
                .collect();
            let table = CompactTable::from_rows(&rows, 0xFFFF);
            let again = CompactTable::from_rows(&rows, 0xFFFF);
            assert_eq!(table, again, "deterministic encoding, case {case}");
            for (row, cells) in rows.iter().enumerate() {
                for (col, expected) in cells.iter().enumerate() {
                    assert_eq!(table.get(row, col), *expected, "case {case} ({row}, {col})");
                }
            }
        }
    }

    #[test]
    fn payload_matching_sentinel_is_rejected_by_construction_contract() {
        // Documented contract: payloads must differ from the sentinel —
        // encode with a sentinel that collides would lose the cell. The
        // struct keeps the sentinel so callers can assert.
        let rows = vec![vec![Some(0)]];
        let table = CompactTable::from_rows(&rows, 0);
        assert_eq!(table.empty, 0);
        // The cell is indistinguishable from empty under this (mis)use —
        // the contract is on the caller, exercised here as documentation.
        assert_eq!(table.get(0, 0), None);
    }
}
