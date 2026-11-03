//! Expected-token masks (issue 022): `(state → expected token set)`
//! consulted by recovery planners and generated parsers, stored as a
//! static table over packed [`TokenSet`] slots with O(1) hashed lookup.
//!
//! Construction is a deterministic displacement scheme: keys group into
//! buckets by `mix(key, seed) % buckets`, each bucket searches the
//! smallest displacement that lands all its keys in free slots, buckets
//! are placed largest-first, and the seed/table grow on failure until
//! every bucket places. Lookups are O(1) with exact-key verification;
//! [`ExpectedMasks::lookup_linear`] is the linear-scan reference the
//! parity tests diff against.

use crate::token_set::TokenSet;

/// One expected-set entry: an automaton state and the tokens valid there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The state key.
    pub state: u32,
    /// The tokens expected in that state.
    pub expected: TokenSet,
}

/// The static expected-mask table.
#[derive(Debug, Clone)]
pub struct ExpectedMasks {
    slots: Vec<Option<Entry>>,
    displacement: Vec<u32>,
    buckets: usize,
    seed: u64,
    table_len: usize,
}

/// The table could not be built (input degenerate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaskError;

impl std::fmt::Display for MaskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("expected-mask table construction failed")
    }
}

impl std::error::Error for MaskError {}

/// Splitmix64 finalizer — the deterministic key hash.
fn mix(key: u32, seed: u64) -> u64 {
    let mut x = u64::from(key) ^ seed;
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Narrow a hash remainder to `usize` (remainders are bounded by the
/// table length, which is itself allocation-bound far below `usize::MAX`).
fn narrow(v: u64) -> usize {
    usize::try_from(v).expect("hash remainder fits usize")
}

impl ExpectedMasks {
    /// Build the table over `entries` (duplicate states folded, last wins).
    ///
    /// # Errors
    /// [`MaskError`] when the input is empty or construction exhausts its
    /// growth budget (degenerate key distributions).
    pub fn build(entries: &[Entry]) -> Result<Self, MaskError> {
        // Fold duplicates: one slot per distinct state.
        let mut folded: Vec<Entry> = Vec::new();
        for entry in entries {
            if let Some(existing) = folded.iter_mut().find(|e| e.state == entry.state) {
                existing.expected = entry.expected;
            } else {
                folded.push(entry.clone());
            }
        }
        if folded.is_empty() {
            return Err(MaskError);
        }
        let count = folded.len();
        // ~75% load factor: the displacement method places reliably there,
        // so the first seeds succeed instead of grinding through retries.
        let mut table_len = count * 4 / 3 + 4;
        let mut buckets = (count / 4).max(1);
        for seed in 0..=24_u64 {
            if let Some(table) = Self::try_build(&folded, table_len, buckets, seed) {
                // A candidate is only accepted when every entry resolves
                // through the hashed path to itself (exact verification).
                if folded
                    .iter()
                    .all(|entry| table.lookup(entry.state).is_some_and(|e| e == entry))
                {
                    return Ok(table);
                }
            }
            // Grow and retry on failure (adversarial distributions).
            if seed % 4 == 3 {
                table_len += count / 2 + 1;
                buckets *= 2;
            }
        }
        Err(MaskError)
    }

    fn try_build(entries: &[Entry], table_len: usize, buckets: usize, seed: u64) -> Option<Self> {
        // Group keys by bucket, place hard (largest) buckets first, and
        // carry each bucket's ORIGINAL hash index through the sort —
        // displacement must be written back to the bucket lookups use.
        let mut by_bucket: Vec<(usize, Vec<usize>)> =
            (0..buckets).map(|b| (b, Vec::new())).collect();
        for (idx, entry) in entries.iter().enumerate() {
            let bucket = narrow(mix(entry.state, seed) % buckets as u64);
            by_bucket[bucket].1.push(idx);
        }
        by_bucket.sort_by_key(|(_, members)| std::cmp::Reverse(members.len()));
        let mut occupied = vec![false; table_len];
        let mut slots: Vec<Option<Entry>> = vec![None; table_len];
        let mut displacement = vec![0_u32; buckets];
        for (bucket, members) in &by_bucket {
            if members.is_empty() {
                continue;
            }
            // Smallest displacement landing every member in a free slot
            // (fail-fast per seed; `build` grows and retries).
            let mut placed = false;
            'search: for d in 0_usize..table_len {
                let mut targets = Vec::with_capacity(members.len());
                for &idx in members {
                    let slot =
                        narrow(mix(entries[idx].state, seed).wrapping_add(d as u64)) % table_len;
                    if occupied[slot] {
                        continue 'search;
                    }
                    targets.push((slot, idx));
                }
                for (slot, idx) in targets {
                    occupied[slot] = true;
                    slots[slot] = Some(entries[idx].clone());
                }
                displacement[*bucket] = u32::try_from(d).ok()?;
                placed = true;
                break;
            }
            if !placed {
                return None;
            }
        }
        Some(Self {
            slots,
            displacement,
            buckets,
            seed,
            table_len,
        })
    }

    fn slot_of(&self, state: u32) -> usize {
        let hash = mix(state, self.seed);
        let bucket = narrow(hash % self.buckets as u64);
        narrow(hash.wrapping_add(u64::from(self.displacement[bucket]))) % self.table_len
    }

    /// The expected set for `state`, O(1), exact-key verified.
    #[must_use]
    pub fn lookup(&self, state: u32) -> Option<&Entry> {
        let slot = self.slot_of(state);
        self.slots.get(slot)?.as_ref().filter(|e| e.state == state)
    }

    /// The linear-scan reference: same answer as [`Self::lookup`], O(n).
    /// The differential tests hold `lookup == lookup_linear` for every
    /// key and for absent keys.
    #[must_use]
    pub fn lookup_linear(&self, state: u32) -> Option<&Entry> {
        self.slots
            .iter()
            .flatten()
            .find(|entry| entry.state == state)
    }

    /// The number of stored entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// Whether the table stores no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(std::option::Option::is_none)
    }

    /// All entries in slot order.
    #[must_use]
    pub fn entries(&self) -> Vec<Entry> {
        self.slots.iter().flatten().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Entry, ExpectedMasks};
    use crate::token_set::TokenSet;

    fn sample(count: u32) -> Vec<Entry> {
        (0..count)
            .map(|state| Entry {
                state,
                expected: TokenSet::from_kinds([
                    u16::try_from(state).expect("small state"),
                    u16::try_from(state * 7 % 100).expect("small state"),
                ]),
            })
            .collect()
    }

    #[test]
    fn lookup_matches_linear_reference() {
        let entries = sample(300);
        let table = ExpectedMasks::build(&entries).expect("builds");
        for entry in &entries {
            assert_eq!(table.lookup(entry.state), table.lookup_linear(entry.state));
            assert_eq!(
                table.lookup(entry.state).expect("present").expected,
                entry.expected
            );
        }
        assert_eq!(table.lookup(1_000), None);
        assert_eq!(table.lookup_linear(1_000), None);
        assert_eq!(table.len(), 300);
        assert!(!table.is_empty());
    }

    #[test]
    fn duplicate_states_fold_last_wins() {
        let entries = vec![
            Entry {
                state: 5,
                expected: TokenSet::from_kinds([1]),
            },
            Entry {
                state: 5,
                expected: TokenSet::from_kinds([2, 3]),
            },
            Entry {
                state: 6,
                expected: TokenSet::from_kinds([4]),
            },
        ];
        let table = ExpectedMasks::build(&entries).expect("builds");
        assert_eq!(table.len(), 2);
        assert_eq!(
            table.lookup(5).expect("present").expected.kinds(),
            vec![2, 3]
        );
    }

    #[test]
    fn empty_input_is_an_error() {
        assert!(ExpectedMasks::build(&[]).is_err());
    }

    #[test]
    fn skewed_keys_still_build() {
        // Odd multiples of the golden constant stress one hash family.
        let entries: Vec<Entry> = (0..48)
            .map(|i| Entry {
                state: (u32::try_from(i).expect("small i"))
                    .wrapping_mul(0x9E37_79B9)
                    .wrapping_mul(2)
                    .wrapping_add(1),
                expected: TokenSet::from_kinds([u16::try_from(i).expect("small i")]),
            })
            .collect();
        let table = ExpectedMasks::build(&entries).expect("builds despite skew");
        for entry in &entries {
            assert_eq!(table.lookup(entry.state), Some(entry));
        }
    }
}
