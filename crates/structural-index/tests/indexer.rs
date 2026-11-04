//! Structural-index suite (issue 032 TDD): classification vs a
//! byte-by-byte reference on random inputs, `SoA` round-trip, fast-path
//! skips, contiguous balanced chunk plans, and the ~16 B/region budget.

use std::ops::Range;

use structural_index::{RegionIndex, RegionKind, plan_chunks, scan};

/// The byte-by-byte reference scanner: same classification, no memchr
/// blocking. Returns `(kind, range)` pairs.
fn reference(source: &str) -> Vec<(u8, Range<u32>)> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut pos = 0_usize;
    let mut stack: Vec<(u8, usize)> = Vec::new();
    while pos < bytes.len() {
        let start = pos;
        match bytes[pos] {
            b'"' | b'\'' => {
                // The region includes both delimiters (the scanner's
                // convention); unterminated strings end at the newline.
                let quote = bytes[pos];
                let mut idx = pos + 1;
                while idx < bytes.len() {
                    match bytes[idx] {
                        b'\\' => idx += 2,
                        b'\n' => break,
                        _ if bytes[idx] == quote => break,
                        _ => idx += 1,
                    }
                }
                let end = if idx < bytes.len() && bytes[idx] == quote {
                    idx + 1
                } else {
                    idx.min(bytes.len())
                };
                out.push((
                    RegionKind::String.as_u8(),
                    u32::try_from(start).unwrap()..u32::try_from(end).unwrap(),
                ));
                pos = end;
            }
            b'#' => {
                let close = bytes[pos..]
                    .iter()
                    .position(|c| *c == b'\n')
                    .map_or(bytes.len(), |off| pos + off);
                out.push((
                    RegionKind::Comment.as_u8(),
                    u32::try_from(start).unwrap()..u32::try_from(close).unwrap(),
                ));
                pos = close;
            }
            b'{' | b'[' | b'(' => {
                stack.push((bytes[pos], out.len()));
                out.push((
                    match bytes[pos] {
                        b'{' => RegionKind::Brace.as_u8(),
                        b'[' => RegionKind::Bracket.as_u8(),
                        _ => RegionKind::Paren.as_u8(),
                    },
                    u32::try_from(start).unwrap()..u32::try_from(start + 1).unwrap(),
                ));
                pos += 1;
            }
            b'}' | b']' | b')' => {
                if let Some(&(open, id)) = stack.last() {
                    let matches = (open == b'{' && bytes[pos] == b'}')
                        || (open == b'[' && bytes[pos] == b']')
                        || (open == b'(' && bytes[pos] == b')');
                    if matches {
                        stack.pop();
                        out[id].1.end = u32::try_from(pos + 1).unwrap();
                    }
                }
                pos += 1;
            }
            b'\\' => pos += 2.min(bytes.len() - pos),
            _ => pos += 1,
        }
    }
    // Unclosed openers close at EOF (the scanner's documented choice).
    for (_, id) in stack {
        out[id].1.end = u32::try_from(bytes.len()).unwrap();
    }
    out
}

/// The block scanner's regions as comparable pairs.
fn indexed(index: &RegionIndex) -> Vec<(u8, Range<u32>)> {
    (0..index.len())
        .filter(|i| !index.is_error(*i))
        .map(|i| {
            (
                index.kind(i).expect("kind").as_u8(),
                index.start(i).expect("s")..index.end(i).expect("e"),
            )
        })
        .collect()
}

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn classification_matches_reference_on_random_inputs() {
    let mut rng = XorShift(0xAB1E_5EED);
    for case in 0..200 {
        let len = 4 + (rng.next() % 120) as usize;
        let alphabet: &[u8] = b"ab{}[]()\"'\\# \n,1";
        let source: Vec<u8> = (0..len)
            .map(|_| alphabet[usize::try_from(rng.next() % alphabet.len() as u64).expect("small")])
            .collect();
        let source = String::from_utf8(source).expect("alphabet is ascii");
        let got = indexed(&scan(&source));
        let want = reference(&source);
        assert_eq!(got, want, "case {case}: {source:?}");
    }
}

#[test]
fn soa_round_trips_fields() {
    let source = "x = \"str\"\n# comment\nfn { arr[0] } call(1)\n";
    let index = scan(source);
    for idx in 0..index.len() {
        let (start, end) = (index.start(idx).expect("s"), index.end(idx).expect("e"));
        let kind = index.kind(idx).expect("kind");
        assert!(
            start < end || index.depth(idx) == Some(0),
            "non-empty regions"
        );
        let slice = &source[start as usize..end as usize];
        match kind {
            RegionKind::String => assert!(slice.starts_with('"') || slice.starts_with('\'')),
            RegionKind::Comment => assert!(slice.starts_with('#')),
            RegionKind::Brace => assert!(slice.starts_with('{') && slice.ends_with('}')),
            RegionKind::Bracket => assert!(slice.starts_with('[') && slice.ends_with(']')),
            RegionKind::Paren => assert!(slice.starts_with('(') && slice.ends_with(')')),
        }
        assert!(index.depth(idx).is_some());
    }
}

#[test]
fn fast_path_skips_string_interiors() {
    // A document of long strings: the state machine must visit a small
    // fraction of the bytes.
    let long_strings: String = (0..50)
        .map(|_| "x = \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"\n")
        .collect();
    let index = scan(&long_strings);
    let stats = index.stats(u32::try_from(long_strings.len()).expect("small"));
    assert_eq!(stats.regions, 50, "one string region per line");
    assert!(
        stats.visited * 10 < stats.bytes,
        "visited {} of {} bytes — the fast path skipped the string interiors",
        stats.visited,
        stats.bytes
    );
}

#[test]
fn validate_fast_path_reports_problems() {
    // Balanced document: validate is Ok.
    let index = scan("fn { arr[0] } call(1)\n");
    assert!(index.validate().is_ok());
    // Unterminated string: Err at the quote.
    let index = scan("x = \"oops\n");
    let Err((kind, at)) = index.validate() else {
        panic!("unterminated string must fail");
    };
    assert_eq!(kind, RegionKind::String);
    assert_eq!(at, 4);
    // Unclosed brace: Err at the opener.
    let index = scan("fn { never\n");
    let Err((kind, at)) = index.validate() else {
        panic!("unclosed brace must fail");
    };
    assert_eq!(kind, RegionKind::Brace);
    assert_eq!(at, 3);
    // Stray closer: an error region.
    let index = scan("x = 1 }\n");
    let Err((kind, at)) = index.validate() else {
        panic!("stray closer must fail");
    };
    assert_eq!(
        kind,
        RegionKind::Paren,
        "stray closers report as the error region"
    );
    assert_eq!(at, 6);
}

#[test]
fn chunk_plans_are_contiguous_and_balanced() {
    let source = "{\"a\": [1, 2, 3], \"b\": {\"c\": null}}\n{\"d\": true}\n";
    let index = scan(source);
    let chunks = plan_chunks(&index, u32::try_from(source.len()).expect("small"), 16);
    assert!(!chunks.is_empty(), "chunks planned");
    // Contiguity + coverage.
    let mut cursor = 0_u32;
    for chunk in &chunks {
        assert_eq!(chunk.start, cursor, "chunks are contiguous");
        cursor = chunk.end;
    }
    assert_eq!(
        cursor,
        u32::try_from(source.len()).expect("small"),
        "full coverage"
    );
    // Balance: every chunk starts and ends at depth 0 (its interior
    // brackets close within the chunk).
    for chunk in &chunks {
        let spans = index.spans();
        let crossing = spans
            .iter()
            .filter(|(s, e)| *s < chunk.start || *e > chunk.end)
            .filter(|(s, e)| *e > chunk.start && *s < chunk.end)
            .count();
        assert_eq!(crossing, 0, "no region crosses a chunk boundary");
    }
}

#[test]
fn region_budget_is_about_16_bytes() {
    let source = "fn { a[1] b(2) }\n# c\nx = \"s\"\ny = 't'\n";
    let index = scan(source);
    assert!(index.len() >= 6, "fixture exercises the families");
    let per_region = index.bytes_per_region();
    assert!(
        per_region <= 16,
        "{per_region} B/region exceeds the ~16 B/region budget"
    );
}
