//! structural-index integration tests (issue 032 TDD): classification
//! vs the byte-by-byte reference on random inputs, the `SoA` round-trip,
//! fast-path skips, contiguous balanced chunk plans, and the ~16
//! B/region budget.

use structural_index::{
    RegionIndex, RegionKind, StructuralFault, plan_chunks, scan, scan_reference, validate_fast,
};

/// Deterministic xorshift64*.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            0
        } else {
            let b = u64::try_from(bound).unwrap_or(u64::MAX);
            usize::try_from(self.next() % b).unwrap_or(0)
        }
    }
}

#[test]
fn classification_matches_reference_on_random_inputs() {
    let pieces = [
        "code",
        "{",
        "}",
        "(",
        ")",
        "\"str\"",
        "'str'",
        "`tpl`",
        "\\",
        "// line\n",
        "/* block */",
        "\"unterminated",
        "/* open",
        "a\\\"b",
        "::",
        "<>",
        "x = 1;\n",
    ];
    let mut rng = Rng::new(0x5CA1_AB1E);
    for round in 0..300 {
        let len = rng.below(12);
        let mut source = String::new();
        for _ in 0..len {
            source.push_str(pieces[rng.below(pieces.len())]);
        }
        let block = scan(&source);
        let reference = scan_reference(&source);
        assert_eq!(
            block, reference,
            "round {round}: block scan diverges on {source:?}"
        );
    }
}

#[test]
fn long_sources_cross_many_blocks() {
    // >64-byte runs exercise the block loop and cross-block states.
    let source = format!(
        "{}\"{}\"{}",
        "x".repeat(200),
        "y".repeat(130),
        "{".repeat(70)
    );
    let block = scan(&source);
    let reference = scan_reference(&source);
    assert_eq!(block, reference, "block-crossing states agree");
    // The long code run collapsed into ONE region (the fast-path skip).
    let code_regions = block
        .iter()
        .filter(|(kind, _, _, _)| *kind == RegionKind::Code)
        .count();
    assert!(code_regions <= 3, "long runs batch: {code_regions}");
}

#[test]
fn soa_round_trip_and_depth() {
    let source = "fn f() {\n  let s = \"text\";\n  // note\n}\n";
    let index = scan(source);
    let reference = scan_reference(source);
    assert_eq!(index, reference);
    // Round-trip via get/iter.
    for (i, region) in index.iter().enumerate() {
        assert_eq!(index.get(i), Some(region));
    }
    // The string region carries the inner depth.
    let strings: Vec<_> = index
        .iter()
        .filter(|(kind, _, _, _)| *kind == RegionKind::StrDouble)
        .collect();
    assert_eq!(strings.len(), 1);
    assert_eq!(strings[0].3, 1, "the string sits at depth 1");
}

#[test]
fn fast_path_skips_region_building_for_clean_code() {
    // A source with no quotes/comments yields exactly ONE code region —
    // the whole file batched by the block scan.
    let source = "a".repeat(5000);
    let index = scan(&source);
    assert_eq!(index.len(), 1, "one batched region: {index:?}");
    assert_eq!(index.get(0).map(|(_, s, e, _)| (s, e)), Some((0, 5000)));
    assert!(validate_fast(&source).is_ok());
}

#[test]
fn budget_is_about_16_bytes_per_region() {
    let source = "fn a() { let s = \"x\"; } // c\n".repeat(200);
    let index = scan(&source);
    assert!(index.len() > 400, "many regions: {}", index.len());
    let per_region = index.bytes_per_region();
    assert!(
        per_region <= 16,
        "budget: {per_region} B/region over {} regions",
        index.len()
    );
}

#[test]
fn validate_finds_faults() {
    assert_eq!(validate_fast("fn f() { ok }"), Ok(()));
    assert_eq!(
        validate_fast("let s = \"oops"),
        Err(StructuralFault::UnterminatedString(8))
    );
    assert_eq!(
        validate_fast("} stray"),
        Err(StructuralFault::StrayClose(0))
    );
    assert_eq!(
        validate_fast("{ unclosed"),
        Err(StructuralFault::UnclosedBraces(1))
    );
}

#[test]
fn chunk_plans_are_contiguous_balanced_and_deterministic() {
    let source = "fn a() { x }\nlet s = \"{}\";\nfn b() { y }\n// {}\nfn c() { z }\nfn d() { w }\n";
    let index = scan(source);
    let plans = plan_chunks(source, &index, 3);
    assert_ne!(plans.len(), 0, "a plan exists");
    assert_eq!(plans[0].start, 0);
    assert_eq!(
        plans.last().map(|p| p.end),
        Some(u32::try_from(source.len()).unwrap_or(u32::MAX))
    );
    for pair in plans.windows(2) {
        assert_eq!(pair[0].end, pair[1].start, "contiguous chunks");
    }
    // Boundaries never cut a non-code region.
    assert!(plan_depths_zero(&plans), "every chunk starts at depth 0");
    // No chunk boundary cuts a string/comment region: every non-code
    // region lies fully inside exactly one chunk.
    for (kind, start, end, _depth) in index.iter() {
        if kind != RegionKind::Code {
            assert!(
                plans
                    .iter()
                    .any(|plan| start >= plan.start && end <= plan.end),
                "region {start}..{end} is not contained in one chunk"
            );
        }
    } // Deterministic: the same inputs, the same plan.
    let again = plan_chunks(source, &index, 3);
    assert_eq!(plans, again);
    // target 1 = one whole-file chunk.
    let one = plan_chunks(source, &index, 1);
    assert_eq!(one.len(), 1);
    assert_eq!(
        (one[0].start, one[0].end),
        (0, u32::try_from(source.len()).unwrap_or(0))
    );
}

#[test]
fn empty_source_plans_nothing() {
    let index = RegionIndex::new();
    assert_eq!(plan_chunks("", &index, 4), vec![]);
}

fn plan_depths_zero(plans: &[structural_index::ChunkPlan]) -> bool {
    plans.iter().all(|p| p.depth == 0)
}
