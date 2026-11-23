//! Region-sink seam tests: [`scan_regions`] drives any
//! [`RegionSink`] consumer over the same regions the block scanner
//! records, the index-collecting impl reproduces [`scan`]
//! bit-for-bit, and the streaming [`FaultScanner`] reproduces the
//! index-driven validator's verdicts — fail-fast, without ever
//! materializing the index.

use std::ops::ControlFlow;

use structural_index::{
    FaultScanner, RegionIndex, RegionKind, RegionSink, StructuralFault, scan, scan_regions,
    validate_fast,
};

/// The index-driven validator oracle: the classification-to-verdict
/// step over a fully materialized index (the pre-seam `validate_fast`
/// shape, kept here as the differential reference).
fn validate_from_index(index: &RegionIndex, source: &str) -> Result<(), StructuralFault> {
    let source_len = u32::try_from(source.len()).unwrap_or(u32::MAX);
    let mut depth: u8 = 0;
    for (kind, start, end, _region_depth) in index.iter() {
        match kind {
            RegionKind::Code => {
                let clipped_end =
                    (end as usize).min(usize::try_from(source_len).unwrap_or(usize::MAX));
                for (offset, byte) in source.as_bytes()[start as usize..clipped_end]
                    .iter()
                    .enumerate()
                {
                    match byte {
                        b'{' => depth = depth.saturating_add(1),
                        b'}' if depth == 0 => {
                            let at = start as usize + offset;
                            return Err(StructuralFault::StrayClose(
                                u32::try_from(at).unwrap_or(0),
                            ));
                        }
                        b'}' => depth = depth.saturating_sub(1),
                        _ => {}
                    }
                }
            }
            RegionKind::StrDouble | RegionKind::StrSingle | RegionKind::StrBacktick => {
                if end == source_len {
                    let bytes = source.as_bytes();
                    let last = bytes.get(end as usize - 1);
                    if last != Some(&quote_byte(kind)) {
                        return Err(StructuralFault::UnterminatedString(start));
                    }
                }
            }
            RegionKind::LineComment => {}
            RegionKind::BlockComment => {
                if end == source_len {
                    let bytes = source.as_bytes();
                    if bytes.get(end as usize - 1) != Some(&b'/') {
                        return Err(StructuralFault::UnterminatedComment(start));
                    }
                }
            }
        }
    }
    if depth != 0 {
        return Err(StructuralFault::UnclosedBraces(depth));
    }
    Ok(())
}

fn quote_byte(kind: RegionKind) -> u8 {
    match kind {
        RegionKind::StrDouble => b'"',
        RegionKind::StrSingle => b'\'',
        RegionKind::StrBacktick => b'`',
        _ => 0,
    }
}

/// The oracle verdict for a source: scan (index build) + validate.
fn oracle(source: &str) -> Result<(), StructuralFault> {
    validate_from_index(&scan(source), source)
}

/// A corpus covering every fault kind and the accepted shapes.
fn corpus() -> Vec<&'static str> {
    vec![
        "",
        "fn main() { let x = \"}\"; }",
        "// { unbalanced in comment",
        "/* } */",
        "let s = \"unterminated",
        "let c = 'unterminated",
        "let b = `unterminated",
        "/* open",
        "a } b",
        "a { b",
        "{}",
        "{ \"nested\": { \"deep\": [true, false, null] } }",
        "\"terminated\" { 'x' /* c */ // d\n}",
        "a \"mixed } quote\" { b }",
        "escape \\\" inside \"a \\\" b\" code { }",
        "} \"x\" // y",
        "back `tick } brace` {",
        "/* multi\nline } comment */ { \"unterminated",
    ]
}

/// The index-collecting [`RegionSink`] reproduces `scan` bit-for-bit: the
/// seam is a pure re-plumbing of the same scanner.
#[test]
fn scan_regions_into_index_matches_scan() {
    for source in corpus() {
        let mut index = RegionIndex::new();
        let flowed = scan_regions(source, &mut index);
        assert!(flowed.is_continue(), "the collector never breaks");
        assert_eq!(&index, &scan(source), "region parity for {source:?}");
    }
}

/// The streaming [`FaultScanner`] reproduces the index-driven verdicts on
/// the whole corpus — same fault kind, same offset.
#[test]
fn fault_scanner_matches_the_index_oracle() {
    for source in corpus() {
        let expected = oracle(source);
        let via_validate = validate_fast(source);
        assert_eq!(via_validate, expected, "validate_fast parity {source:?}");
        let mut scanner = FaultScanner::new(source);
        let flowed = scan_regions(source, &mut scanner);
        let verdict = scanner.finish();
        // Only in-drive faults break the scan; `UnclosedBraces` is the
        // EOF depth verdict, reached after a full `Continue` drive.
        let breaks = matches!(expected, Err(f) if !matches!(f, StructuralFault::UnclosedBraces(_)));
        assert_eq!(
            flowed.is_break(),
            breaks,
            "break/verdict agreement {source:?}"
        );
        assert_eq!(verdict, expected, "streaming parity {source:?}");
    }
}

/// Fail-fast is observable: the scanner stops consuming regions once
/// the fault is found (here: the stray `}` inside the FIRST region,
/// with two more regions the full scan would visit).
#[test]
fn fault_scanner_stops_at_the_first_fault() {
    let source = "} \"x\" // y";
    let full = scan(source);
    assert!(full.len() >= 3, "the full scan visits all regions");
    let mut scanner = FaultScanner::new(source);
    let flowed = scan_regions(source, &mut scanner);
    assert!(flowed.is_break(), "the fault breaks the drive");
    let regions_seen = scanner.regions_seen();
    assert_eq!(scanner.finish(), Err(StructuralFault::StrayClose(0)));
    assert_eq!(regions_seen, 1, "only the first region was consumed");
}

/// The sink drives through a trait object (`&mut dyn RegionSink`) —
/// the seam is dyn-compatible for host-side consumers.
struct Counter(usize);

impl RegionSink for Counter {
    fn region(&mut self, _kind: RegionKind, _start: u32, _end: u32, _depth: u8) -> ControlFlow<()> {
        self.0 += 1;
        ControlFlow::Continue(())
    }
}

#[test]
fn scan_regions_drives_trait_objects() {
    let source = "fn a() { x } fn b() { y }";
    let mut counter = Counter(0);
    let flowed = scan_regions(source, &mut counter as &mut dyn RegionSink);
    assert!(flowed.is_continue());
    assert_eq!(counter.0, scan(source).len(), "one callback per region");
}

/// Deterministic LCG — reproducible without an external RNG (same
/// shape as the other property suites).
struct Lcg(u64);

impl Lcg {
    fn below(&mut self, n: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) % n.max(1)
    }
}

/// Randomized C/JS-shaped sources: the streaming validator agrees with
/// the index oracle on every round, and the region stream matches the
/// materialized index (regions, offsets, depths).
#[test]
fn property_streaming_parity_on_random_sources() {
    let mut rng = Lcg(0x0246_8ACE_1357_9BDF);
    let alphabet: &[u8; 16] = b"ab {}\"'\x60/\\*\n;()=";
    for _ in 0..300 {
        let len = 8 + rng.below(120);
        let mut bytes = Vec::with_capacity(len as usize);
        for _ in 0..len {
            bytes.push(alphabet[rng.below(alphabet.len() as u64) as usize]);
        }
        let source = String::from_utf8(bytes).expect("ascii generator");
        let expected = oracle(&source);

        let via_validate = validate_fast(&source);
        assert_eq!(via_validate, expected, "validate_fast parity {source:?}");

        let mut scanner = FaultScanner::new(&source);
        let flowed = scan_regions(&source, &mut scanner);
        assert_eq!(scanner.finish(), expected, "streaming parity {source:?}");
        let breaks = matches!(expected, Err(f) if !matches!(f, StructuralFault::UnclosedBraces(_)));
        assert_eq!(flowed.is_break(), breaks, "break agreement {source:?}");

        let mut collected = RegionIndex::new();
        assert!(
            scan_regions(&source, &mut collected).is_continue(),
            "the collector never breaks"
        );
        assert_eq!(&collected, &scan(&source), "region parity {source:?}");
    }
}
