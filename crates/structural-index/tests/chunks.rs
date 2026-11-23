//! Chunk-plan balance: every chunk a [`plan_chunks`] plan cuts out must
//! be genuinely balanced — each boundary sits at a TRUE depth-0 code
//! position, so each chunk is independently validatable (the
//! region-parallel precondition). The oracle is the validate fast path
//! run on the chunk's own bytes: a balanced chunk never faults.
//!
//! The trap this pins: the region index records the brace depth at a
//! region's END (braces do not split Code regions), so a region like
//! ` } x` records depth 0 while the position it starts at is actually
//! at depth 1 — planning must look at the depth the boundary sits at
//! (the previous region's end depth), not at the region's own record.

use structural_index::{plan_chunks, scan, validate_fast};

/// Deterministic xorshift64* (same generator as the fuzz suite).
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

/// A balanced-but-tricky source: the ` } x` region records depth 0
/// while sitting at depth 1 — the buggy candidate filter picks it as a
/// "depth-0" boundary and cuts `{ "s" ` off as a chunk of its own.
#[test]
fn boundaries_split_at_true_depth_zero() {
    let source = "{ \"s\" } x";
    let index = scan(source);
    for target in [1, 2, 3, 4] {
        for chunk in plan_chunks(source, &index, target) {
            let bytes = &source[chunk.start as usize..chunk.end as usize];
            assert_eq!(
                validate_fast(bytes),
                Ok(()),
                "chunk {chunk:?} of {source:?} (target {target}) is not balanced"
            );
        }
    }
}

/// A valid interior boundary must still be found after the fix: the
/// string here closes at depth 0, so the Code region that follows
/// genuinely starts at depth 0 — target 2 must produce two chunks.
#[test]
fn a_valid_boundary_is_still_found() {
    let source = "{ } \"s\" { } x";
    let index = scan(source);
    let plans = plan_chunks(source, &index, 2);
    assert_eq!(plans.len(), 2, "the depth-0 split after \"s\" is real");
    for chunk in &plans {
        let bytes = &source[chunk.start as usize..chunk.end as usize];
        assert_eq!(validate_fast(bytes), Ok(()), "chunk {chunk:?} unbalanced");
    }
}

/// The balance property over seeded random sources: whenever the whole
/// source is balanced, every planned chunk is balanced too (for every
/// target chunk count).
#[test]
fn clean_sources_plan_into_clean_chunks() {
    let pieces = [
        "code ",
        "{ ",
        "} ",
        "\"str\" ",
        "'c' ",
        "`t` ",
        "// line\n",
        "/* blk */",
        ":: ",
        "x ",
    ];
    let mut rng = Rng::new(0x0C_46_4B_51);
    for _ in 0..512 {
        let n = 2 + rng.below(18);
        let mut source = String::new();
        for _ in 0..n {
            source.push_str(pieces[rng.below(pieces.len())]);
        }
        if validate_fast(&source).is_err() {
            continue; // balance is only promised for balanced sources
        }
        let index = scan(&source);
        for target in [2, 3, 4] {
            for chunk in plan_chunks(&source, &index, target) {
                let bytes = &source[chunk.start as usize..chunk.end as usize];
                assert_eq!(
                    validate_fast(bytes),
                    Ok(()),
                    "chunk {chunk:?} of {source:?} (target {target}) is not balanced"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The chunk-validation seam: `validate_chunks` / `validate_chunks_with` /
// `validate_plan` over the `ChunkValidator` contract.
// ---------------------------------------------------------------------------

use structural_index::{
    ChunkReport, ChunkValidator, StructuralFault, validate_chunks, validate_chunks_with,
    validate_plan,
};

/// The fault oracle (the same shape the streaming tests pin): the
/// whole-source verdict a chunked run must reproduce.
#[test]
fn chunked_clean_matches_whole_source_clean() {
    let sources = [
        "",
        "fn main() { let x = \"}\"; }",
        "// { unbalanced in comment",
        "/* } */",
        "{ } \"s\" { } x",
        "fn a() { x }\nfn b() { y }\nfn c() { z }\nfn d() { w }\n",
    ];
    for source in sources {
        let index = scan(source);
        for target in [1, 2, 3, 4] {
            let report = validate_plan(source, &index, target);
            assert_eq!(
                report.is_clean(),
                validate_fast(source).is_ok(),
                "verdict parity for {source:?} (target {target})"
            );
            assert_eq!(report.chunks, plan_chunks(source, &index, target).len());
        }
    }
}

/// Fault parity: the chunked first fault IS the whole-source fault —
/// same variant, same (global) offset — now with chunk attribution.
#[test]
fn chunked_first_fault_matches_validate_fast() {
    let sources = [
        "let s = \"unterminated",
        "/* open",
        "a } b",
        "a { b",
        "{ \"s\" } } x",
        "x \"y\" { a \"z",
        "fn a() { x } fn b() { \"unterminated }",
    ];
    for source in sources {
        let whole = validate_fast(source).expect_err("malformed by construction");
        let index = scan(source);
        for target in [1, 2, 3, 4] {
            let report = validate_plan(source, &index, target);
            let first = report
                .first_fault()
                .unwrap_or_else(|| panic!("no fault reported for {source:?} (target {target})"));
            assert_eq!(
                first.fault, whole,
                "first-fault parity for {source:?} (target {target})"
            );
            // The attributed chunk must contain the fault's offset.
            let plans = plan_chunks(source, &index, target);
            if let StructuralFault::UnterminatedString(at)
            | StructuralFault::UnterminatedComment(at)
            | StructuralFault::StrayClose(at) = first.fault
            {
                let chunk = plans[first.chunk];
                assert!(
                    at >= chunk.start && at < chunk.end,
                    "fault at {at} outside attributed chunk {chunk:?} of {source:?}"
                );
            }
        }
    }
}

/// Seeded property: verdict parity + first-fault parity on random
/// clean and malformed sources, across target chunk counts.
#[test]
fn seeded_chunked_parity_on_random_sources() {
    let pieces = [
        "code ",
        "{ ",
        "} ",
        "\"str\" ",
        "'c' ",
        "`t` ",
        "// line\n",
        "/* blk */",
        ":: ",
        "\"unterm",
        "/* open",
        "a } b",
    ];
    let mut rng = Rng::new(0x5EED_5EED);
    for _ in 0..768 {
        let n = 2 + rng.below(18);
        let mut source = String::new();
        for _ in 0..n {
            source.push_str(pieces[rng.below(pieces.len())]);
        }
        let index = scan(&source);
        let whole = validate_fast(&source);
        for target in [2, 3] {
            let report = validate_plan(&source, &index, target);
            assert_eq!(
                report.is_clean(),
                whole.is_ok(),
                "verdict parity for {source:?} (target {target})"
            );
            match (report.first_fault(), &whole) {
                (Some(first), Err(expected)) => {
                    assert_eq!(first.fault, *expected, "fault parity for {source:?}");
                }
                (None, Ok(())) => {}
                (other, verdict) => {
                    panic!("shape mismatch: {other:?} vs {verdict:?} for {source:?}")
                }
            }
        }
    }
}

/// A custom validator rides the seam: the driver visits every chunk in
/// plan order, hands each its exact bytes, and rebases the faults the
/// custom implementation reports (here: chunk-local offsets).
struct Counting {
    calls: usize,
    bytes_seen: usize,
    reject_from: Option<usize>,
}

impl ChunkValidator for Counting {
    fn validate_chunk(&mut self, text: &str) -> Result<(), StructuralFault> {
        let idx = self.calls;
        self.calls += 1;
        self.bytes_seen += text.len();
        if self.reject_from == Some(idx) {
            // Report a chunk-local stray close at offset 0.
            return Err(StructuralFault::StrayClose(0));
        }
        Ok(())
    }
}

#[test]
fn custom_validator_rides_the_seam() {
    // Strings closing at depth 0 between the brace-balanced lines give
    // the planner real interior boundaries, so target 2 yields two
    // chunks (a plain brace source is one unbroken Code region).
    let source = "fn a() { x } \"s\"\nfn b() { y } \"s\"\nfn c() { z } \"s\"\nfn d() { w } \"s\"\n";
    let index = scan(source);
    let plan = plan_chunks(source, &index, 2);
    assert_eq!(plan.len(), 2, "the source splits at the line boundaries");

    let mut counting = Counting {
        calls: 0,
        bytes_seen: 0,
        reject_from: None,
    };
    let report = validate_chunks_with(source, &plan, &mut counting);
    assert_eq!(counting.calls, plan.len(), "every chunk visited once");
    assert_eq!(counting.bytes_seen, source.len(), "chunks tile the source");
    assert!(report.is_clean(), "accepting validator => clean report");

    let mut rejecting = Counting {
        calls: 0,
        bytes_seen: 0,
        reject_from: Some(1),
    };
    let report = validate_chunks_with(source, &plan, &mut rejecting);
    let first = report.first_fault().expect("the rejected chunk faults");
    assert_eq!(first.chunk, 1, "attribution follows the rejecting chunk");
    // The driver rebased the chunk-local 0 into source coordinates.
    assert_eq!(
        first.fault,
        StructuralFault::StrayClose(plan[1].start),
        "offsets rebased to source coordinates"
    );
}

/// Degenerate plans: an empty source plans nothing (a clean report
/// with zero chunks); a single-chunk plan behaves like the whole
/// source scan.
#[test]
fn degenerate_plans_still_report() {
    let empty = ChunkReport {
        chunks: 0,
        faults: Vec::new(),
    };
    assert!(empty.is_clean());
    assert_eq!(validate_chunks("", &[]), empty);

    let source = "let s = \"unterminated";
    let index = scan(source);
    let plan = plan_chunks(source, &index, 1);
    assert_eq!(plan.len(), 1);
    let report = validate_chunks(source, &plan);
    assert_eq!(
        report.first_fault().map(|f| f.fault),
        Some(StructuralFault::UnterminatedString(8))
    );
    assert_eq!(report.chunks, 1);
}

/// Multibyte safety: region starts (and therefore chunk boundaries)
/// always follow an ASCII byte, so chunk slices never split a code
/// point — CJK text inside strings and code plans and validates.
#[test]
fn multibyte_sources_plan_and_validate() {
    let sources = [
        "let s = \"日本語のテキスト\"; { \"絵\u{1F600}\" }",
        "fn 中文() { /* 注释 */ \" emoji \u{1F680}\" }",
        "\"日本語\" { \"unterminatedÉ",
    ];
    for source in sources {
        let index = scan(source);
        for target in [1, 2, 3] {
            let report = validate_plan(source, &index, target);
            assert_eq!(
                report.is_clean(),
                validate_fast(source).is_ok(),
                "multibyte parity for {source:?} (target {target})"
            );
        }
    }
}
