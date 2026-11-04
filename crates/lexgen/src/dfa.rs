//! DFA construction (issue 014): subset construction over the merged NFA
//! with **char-class compression** — the Unicode scalar space is
//! partitioned into equivalence intervals from the pattern boundaries, and
//! DFA transitions are keyed by class index, keeping tables small and
//! lookups a binary search.

use std::collections::{BTreeMap, BTreeSet};

use crate::nfa::Nfa;

/// The compressed char classes: sorted disjoint intervals partitioning
/// `[0, SCALAR_END)`.
#[derive(Debug, Clone)]
pub struct CharClasses {
    /// Class starts (boundaries); class `i` covers
    /// `[starts[i], starts[i+1] - 1]`.
    pub starts: Vec<u32>,
}

impl CharClasses {
    /// Build the partition from every range boundary in the NFA.
    #[must_use]
    pub fn from_nfa(nfa: &Nfa) -> Self {
        let mut boundaries: BTreeSet<u32> = BTreeSet::from([0]);
        for outgoing in &nfa.transitions {
            for &(lo, hi, _) in outgoing {
                boundaries.insert(lo);
                if hi.saturating_add(1) < crate::regex::SCALAR_END {
                    boundaries.insert(hi + 1);
                }
            }
        }
        Self {
            starts: boundaries.into_iter().collect(),
        }
    }

    /// Number of classes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.starts.len()
    }

    /// Whether the partition is empty (no classes — degenerate).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    /// The class of one scalar (binary search).
    #[must_use]
    pub fn class_of(&self, c: u32) -> usize {
        match self.starts.binary_search(&c) {
            Ok(index) => index,
            Err(insertion) => insertion - 1,
        }
    }

    /// The class interval bounds.
    #[must_use]
    pub fn interval(&self, class: usize) -> (u32, u32) {
        let lo = self.starts[class];
        let hi = self
            .starts
            .get(class + 1)
            .map_or(crate::regex::SCALAR_END - 1, |&next| next - 1);
        (lo, hi)
    }
}

/// The compiled DFA over compressed classes.
#[derive(Debug, Clone)]
pub struct Dfa {
    /// Transition table: `[state][class] → next state`.
    pub transitions: Vec<Vec<Option<u32>>>,
    /// Accepting states → the winning pattern index (lowest declaration
    /// order among the NFA accepts in the subset).
    pub accept: Vec<Option<u16>>,
    /// The start state (always 0).
    pub start: u32,
    /// The compressed classes backing the table columns.
    pub classes: CharClasses,
}

impl Dfa {
    /// Subset construction over `nfa`.
    ///
    /// # Panics
    /// When subset construction produces more than `u32::MAX` DFA
    /// states (index-space exhaustion).
    #[must_use]
    pub fn from_nfa(nfa: &Nfa) -> Self {
        let classes = CharClasses::from_nfa(nfa);
        let start_set = nfa.closure(&BTreeSet::from([nfa.start]));
        let mut subsets: Vec<BTreeSet<u32>> = vec![start_set];
        let mut index: BTreeMap<BTreeSet<u32>, u32> = BTreeMap::new();
        index.insert(subsets[0].clone(), 0);
        let mut accept: Vec<Option<u16>> = vec![nfa.accept_of(&subsets[0])];
        let mut transitions: Vec<Vec<Option<u32>>> = vec![vec![None; classes.len()]];
        let mut queue: std::collections::VecDeque<u32> = std::collections::VecDeque::from([0]);
        while let Some(state) = queue.pop_front() {
            for class in 0..classes.len() {
                let (lo, hi) = classes.interval(class);
                // Move over the whole class interval (all scalars in a
                // class behave identically — that is the compression).
                let moved = nfa.move_set(&subsets[state as usize], lo);
                if moved.is_empty() {
                    continue;
                }
                let closed = nfa.closure(&moved);
                let next = if let Some(&existing) = index.get(&closed) {
                    existing
                } else {
                    let id = u32::try_from(subsets.len()).expect("dfa state fits u32");
                    subsets.push(closed.clone());
                    index.insert(closed, id);
                    accept.push(nfa.accept_of(&subsets[id as usize]));
                    transitions.push(vec![None; classes.len()]);
                    queue.push_back(id);
                    id
                };
                transitions[state as usize][class] = Some(next);
                let _ = hi; // interval end implicit in the partition
            }
        }
        Self {
            transitions,
            accept,
            start: 0,
            classes,
        }
    }

    /// Number of DFA states.
    #[must_use]
    pub fn len(&self) -> usize {
        self.transitions.len()
    }

    /// Whether the DFA has no states (degenerate).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.transitions.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::Dfa;
    use crate::nfa::Nfa;
    use crate::regex::parse_regex;

    #[test]
    fn classes_partition_the_scalar_space() {
        let one = Nfa::build_fragment(&parse_regex("[a-cx-z]").expect("parses"));
        let nfa = Nfa::merge(&[one], &[0]);
        let dfa = Dfa::from_nfa(&nfa);
        // Boundaries: 0, a, d, x, z+1 … classes must tile [0, 0x110000).
        let classes = &dfa.classes;
        assert_eq!(classes.interval(0).0, 0);
        let mut cursor = 0;
        for class in 0..classes.len() {
            let (lo, hi) = classes.interval(class);
            assert_eq!(lo, cursor, "classes are contiguous");
            assert!(hi >= lo);
            cursor = hi + 1;
        }
        assert_eq!(cursor, crate::regex::SCALAR_END);
        assert_eq!(classes.class_of(0), 0);
        assert_eq!(classes.class_of('b' as u32), classes.class_of('a' as u32));
        assert_ne!(classes.class_of('a' as u32), classes.class_of('d' as u32));
    }

    #[test]
    fn subset_states_and_accept_priority() {
        // Two patterns matching the same char: the lower declaration
        // index wins the accept.
        let one = Nfa::build_fragment(&parse_regex("a").expect("parses"));
        let two = Nfa::build_fragment(&parse_regex("a").expect("parses"));
        let nfa = Nfa::merge(&[one, two], &[4, 9]);
        let dfa = Dfa::from_nfa(&nfa);
        let accepting = dfa.accept.iter().flatten().copied().collect::<Vec<_>>();
        assert_eq!(accepting, [4]);
    }
}
