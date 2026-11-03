//! Expected-mask suites (issue 022): PHF lookups diffed against the
//! linear reference, packed token-set semantics, and skew tolerance.

use lexgen::{Entry, ExpectedMasks, TokenSet};

fn table(count: u32) -> (Vec<Entry>, ExpectedMasks) {
    let entries: Vec<Entry> = (0..count)
        .map(|state| Entry {
            state,
            expected: TokenSet::from_kinds([
                u16::try_from(state).expect("small"),
                u16::try_from(state * 3 % 97).expect("small"),
                u16::try_from(state * 11 % 150).expect("small"),
            ]),
        })
        .collect();
    let table = ExpectedMasks::build(&entries).expect("builds");
    (entries, table)
}

#[test]
fn phf_agrees_with_linear_reference_everywhere() {
    let (entries, masks) = table(500);
    // Every key: hashed lookup == linear lookup == the built entry.
    for entry in &entries {
        assert_eq!(masks.lookup(entry.state), masks.lookup_linear(entry.state));
        assert_eq!(masks.lookup(entry.state), Some(entry));
    }
    // Absent keys agree on absence.
    for absent in [1_000_u32, 65_536, u32::MAX - 1] {
        assert_eq!(masks.lookup(absent), None);
        assert_eq!(masks.lookup_linear(absent), None);
    }
}

#[test]
fn token_sets_are_the_mask_payload() {
    let (entries, masks) = table(64);
    for entry in &entries {
        let found = masks.lookup(entry.state).expect("present");
        assert_eq!(found.expected, entry.expected);
        for kind in entry.expected.kinds() {
            assert!(found.expected.contains(kind));
        }
    }
}

#[test]
fn duplicates_fold_and_grow_on_skew() {
    let entries = vec![
        Entry {
            state: 9,
            expected: TokenSet::from_kinds([1, 2]),
        },
        Entry {
            state: 9,
            expected: TokenSet::from_kinds([7]),
        },
        Entry {
            state: 1_048_576,
            expected: TokenSet::from_kinds([3]),
        },
    ];
    let masks = ExpectedMasks::build(&entries).expect("builds");
    assert_eq!(masks.len(), 2);
    assert_eq!(masks.lookup(9).expect("present").expected.kinds(), vec![7]);

    // Powers of two (worst-case multiplicative skew) still build.
    let skewed: Vec<Entry> = (0..48)
        .map(|i| Entry {
            state: (u32::try_from(i).expect("small"))
                .wrapping_mul(0x9E37_79B9)
                .wrapping_mul(2)
                .wrapping_add(1),
            expected: TokenSet::from_kinds([u16::try_from(i).expect("small")]),
        })
        .collect();
    let masks = ExpectedMasks::build(&skewed).expect("builds despite skew");
    for entry in &skewed {
        assert_eq!(masks.lookup(entry.state), Some(entry));
    }
    assert_eq!(
        ExpectedMasks::build(&[]).unwrap_err().to_string(),
        "expected-mask table construction failed"
    );
}

#[test]
fn packed_set_round_trip_and_union() {
    let mut set = TokenSet::new();
    for kind in [0_u16, 63, 64, 127, 128, 191] {
        set.insert(kind);
    }
    assert_eq!(set.kinds(), vec![0, 63, 64, 127, 128, 191]);
    assert_eq!(set.len(), 6);
    assert!(!set.contains(62));
    let merged = set.union(TokenSet::from_kinds([62, 63]));
    assert_eq!(merged.kinds(), vec![0, 62, 63, 64, 127, 128, 191]);
    // Out-of-capacity kinds are ignored, not truncated.
    let mut wide = TokenSet::new();
    wide.insert(192);
    wide.insert(u16::MAX);
    assert!(wide.is_empty());
}
