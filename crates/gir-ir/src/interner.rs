//! Symbol interning: stable, dense 32-bit handles for grammar names.
//!
//! Every name that crosses the IR (rule names, token names) is interned
//! into a [`Sym`]; the interner is the single reverse-mapping authority.
//! Dense handles keep IR nodes small and make name equality a u32 compare.

use std::collections::HashMap;
use std::fmt;

/// An interned symbol: an index into an [`Interner`]'s string table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sym(u32);

impl Sym {
    /// The index backing this symbol (stable within one interner).
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }
}

impl fmt::Display for Sym {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Bijective string table: `String → Sym` and `Sym → String`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Interner {
    ids: HashMap<String, Sym>,
    values: Vec<String>,
}

impl Interner {
    /// A fresh empty interner.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `s`, returning the stable [`Sym`] (the same string always
    /// yields the same symbol).
    pub fn intern(&mut self, s: &str) -> Sym {
        if let Some(&sym) = self.ids.get(s) {
            return sym;
        }
        let sym = Sym(u32::try_from(self.values.len()).expect("interner overflows u32 symbols"));
        self.values.push(s.to_owned());
        self.ids.insert(s.to_owned(), sym);
        sym
    }

    /// The string for a previously interned symbol.
    #[must_use]
    pub fn lookup(&self, sym: Sym) -> &str {
        &self.values[usize::try_from(sym.0).expect("sym index fits usize")]
    }

    /// Number of interned strings (also the exclusive upper bound of
    /// valid [`Sym`] indices).
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Whether nothing has been interned yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }
}

#[cfg(test)]
mod interner_tests {
    use super::{Interner, Sym};

    #[test]
    fn same_string_is_same_symbol() {
        let mut it = Interner::new();
        let a = it.intern("expr");
        let b = it.intern("expr");
        assert_eq!(a, b);
        assert_eq!(a.index(), 0);
        assert_eq!(it.lookup(a), "expr");
    }

    #[test]
    fn distinct_strings_get_distinct_dense_indices() {
        let mut it = Interner::new();
        let syms: Vec<Sym> = ["alpha", "beta", "gamma"]
            .iter()
            .map(|s| it.intern(s))
            .collect();
        assert_eq!(
            syms.iter().map(|s| s.index()).collect::<Vec<_>>(),
            vec![0, 1, 2],
            "symbols are dense in insertion order"
        );
        assert_eq!(it.lookup(syms[2]), "gamma");
        assert_eq!(it.len(), 3);
    }

    #[test]
    fn interning_after_lookup_is_stable() {
        let mut it = Interner::new();
        let first = it.intern("rule");
        let second = it.intern("other");
        let again = it.intern("rule");
        assert_eq!(first, again);
        assert_ne!(first, second);
    }

    #[test]
    fn fresh_interner_is_empty() {
        let it = Interner::new();
        assert!(it.is_empty());
        assert_eq!(it.len(), 0);
    }

    #[test]
    fn symbols_order_like_intern_order() {
        let mut it = Interner::new();
        let a = it.intern("a");
        let b = it.intern("b");
        assert!(a < b, "symbols are totally ordered by insertion");
    }
}
