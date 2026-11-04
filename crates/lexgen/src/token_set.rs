//! Packed token sets (issue 022): a 192-bit bitmask over token kinds
//! backing the expected-token masks. Three `u64` words cover 192 kinds —
//! far above any real grammar's token table — while staying `Copy`-sized
//! and cache-friendly inside the PHF slot table.

/// A packed 192-bit set of token kinds (`0..=191`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct TokenSet([u64; 3]);

/// Kinds above this do not fit in the packed representation.
pub const TOKEN_SET_CAP: u16 = 192;

impl TokenSet {
    /// The empty set.
    #[must_use]
    pub const fn new() -> Self {
        Self([0; 3])
    }

    /// Insert one kind. No-op past the [`TOKEN_SET_CAP`] limit.
    pub fn insert(&mut self, kind: u16) {
        let word = usize::from(kind / 64);
        if word < 3 {
            self.0[word] |= 1_u64 << (kind % 64);
        }
    }

    /// Whether `kind` is in the set.
    #[must_use]
    pub fn contains(&self, kind: u16) -> bool {
        let word = usize::from(kind / 64);
        word < 3 && self.0[word] & (1_u64 << (kind % 64)) != 0
    }

    /// Build a set from an iterator of kinds.
    #[must_use]
    pub fn from_kinds(kinds: impl IntoIterator<Item = u16>) -> Self {
        let mut set = Self::new();
        for kind in kinds {
            set.insert(kind);
        }
        set
    }

    /// The kinds present, ascending.
    ///
    /// # Panics
    /// Never: the set holds at most 192 bits and every set bit maps to
    /// a `u16` by construction.
    #[must_use]
    pub fn kinds(&self) -> Vec<u16> {
        let mut kinds = Vec::new();
        for (word, bits) in self.0.iter().enumerate() {
            let mut bits = *bits;
            while bits != 0 {
                let bit = bits.trailing_zeros();
                kinds.push(u16::try_from(word * 64 + bit as usize).expect("within 192 bits"));
                bits &= bits - 1;
            }
        }
        kinds
    }

    /// Number of kinds present.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Whether no kind is present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0 == [0; 3]
    }

    /// Set union.
    #[must_use]
    pub fn union(mut self, other: Self) -> Self {
        for (word, other) in self.0.iter_mut().zip(other.0) {
            *word |= other;
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{TOKEN_SET_CAP, TokenSet};

    #[test]
    fn round_trips_kinds() {
        let set = TokenSet::from_kinds([0, 1, 63, 64, 127, 128, 191]);
        assert_eq!(set.kinds(), vec![0, 1, 63, 64, 127, 128, 191]);
        assert_eq!(set.len(), 7);
        assert!(!set.is_empty());
    }

    #[test]
    fn ignores_kinds_above_capacity() {
        let mut set = TokenSet::new();
        set.insert(TOKEN_SET_CAP);
        set.insert(u16::MAX);
        assert!(set.is_empty());
        assert!(!set.contains(u16::MAX));
    }

    #[test]
    fn unions_and_defaults_empty() {
        let a = TokenSet::from_kinds([1, 5]);
        let b = TokenSet::from_kinds([5, 130]);
        assert_eq!(a.union(b).kinds(), vec![1, 5, 130]);
        assert!(TokenSet::new().is_empty());
        assert_eq!(TokenSet::new().len(), 0);
    }
}
