//! # bench — criterion suites and performance budget gates (§10, issues
//! 018 + 024)
//!
//! Budget philosophy: the hot parse path allocates only what the green
//! tree needs. The sink modes are *free*: `validate` must run with zero
//! allocations on a prebuilt event stream, `scan`/`skeleton` stay within
//! two allocations per kilobyte of input. The gates are dhat-counted
//! tests (machine-independent, CI-runnable), never wall-clock.
//!
//! - [`budgets_documented`] — the machine-readable budget table.
//! - [`small_document`] / [`medium_document`] / [`steady_state_document`]
//!   — deterministic fixtures (byte-stable, so allocation counts are
//!   comparable across runs).

/// `validate` mode: the sink is two counters + a depth word — nothing may
/// allocate (per kilobyte of input).
pub const VALIDATE_ALLOC_PER_KIB: usize = 1;

/// `scan` mode (TokenSink): one push per token, amortized growth only.
pub const SCAN_ALLOC_PER_KIB: usize = 2;

/// `skeleton` mode (SkeletonSink): one push per completed node — same
/// amortized bound as scan.
pub const SKELETON_ALLOC_PER_KIB: usize = 2;

/// Absolute allocation cap for the small fixture (any mode): below the
/// amortized regime, pin the constant instead.
pub const SMALL_DOC_MAX_ALLOCS: usize = 16;

/// The steady-state fixture must be at least this many bytes (the
/// per-kB gates need a document large enough that amortization dominates).
pub const STEADY_DOC_MIN_BYTES: usize = 32 * 1024;

/// The full budget table with its documentation, for round-trip checks
/// and for the `syntact bench` report (issue 020).
#[must_use]
pub fn budgets_documented() -> Vec<(&'static str, usize, &'static str)> {
    vec![
        (
            "validate_alloc_per_kib",
            VALIDATE_ALLOC_PER_KIB,
            "validate-mode allocations per KiB of input (0 in practice)",
        ),
        (
            "scan_alloc_per_kib",
            SCAN_ALLOC_PER_KIB,
            "scan-mode (TokenSink) allocations per KiB of input",
        ),
        (
            "skeleton_alloc_per_kib",
            SKELETON_ALLOC_PER_KIB,
            "skeleton-mode (SkeletonSink) allocations per KiB of input",
        ),
        (
            "small_doc_max_allocs",
            SMALL_DOC_MAX_ALLOCS,
            "absolute cap on the small fixture (any mode)",
        ),
        (
            "steady_doc_min_bytes",
            STEADY_DOC_MIN_BYTES,
            "minimum size of the steady-state fixture",
        ),
    ]
}

/// One JSON object of the fixtures: `{"k0": 0, "k1": 1.5, "k2": "s…"}`.
fn object(index: usize) -> String {
    let text = "x".repeat(8 + (index % 7));
    format!("{{\"k0\": {index}, \"k1\": 1.5, \"k2\": \"{text}\"}}")
}

/// Build a document of `count` objects separated by newlines, wrapped in
/// an outer object (deterministic bytes; size grows ~48 B per object).
#[must_use]
pub fn document_of(count: usize) -> String {
    let mut out = String::with_capacity(count * 48 + 2);
    out.push('{');
    for (index, item) in (0..count).map(object).enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('\n');
        out.push('"');
        let _ = std::fmt::Write::write_fmt(&mut out, format_args!("o{index}"));
        out.push_str("\": ");
        out.push_str(&item);
    }
    out.push_str("\n}\n");
    out
}

/// The small fixture (~1 KiB).
#[must_use]
pub fn small_document() -> String {
    document_of(16)
}

/// The medium fixture (~8 KiB).
#[must_use]
pub fn medium_document() -> String {
    document_of(160)
}

/// The steady-state fixture: ≥ [`STEADY_DOC_MIN_BYTES`] bytes, the size
/// the per-kB allocation gates measure against.
#[must_use]
pub fn steady_state_document() -> String {
    let mut document = document_of(512);
    while document.len() < STEADY_DOC_MIN_BYTES {
        document = document_of(document.len().div_ceil(48) * 2);
    }
    document
}

/// Kilobytes, minimum 1 (the per-kB budget divisor).
#[must_use]
pub fn kib(bytes: usize) -> usize {
    (bytes / 1024).max(1)
}

#[cfg(test)]
mod tests {
    use super::{
        STEADY_DOC_MIN_BYTES, budgets_documented, medium_document, small_document,
        steady_state_document,
    };

    #[test]
    fn budgets_documented_round_trips() {
        let table = budgets_documented();
        assert_eq!(table.len(), 5);
        for (name, value, doc) in table {
            assert_ne!(name, "");
            assert!(value > 0);
            assert_ne!(doc, "");
        }
    }

    #[test]
    fn fixtures_are_deterministic_and_sized() {
        assert_eq!(small_document(), small_document());
        assert_eq!(medium_document(), medium_document());
        assert_eq!(steady_state_document(), steady_state_document());
        assert!(small_document().len() < medium_document().len());
        assert!(medium_document().len() < steady_state_document().len());
        assert!(steady_state_document().len() >= STEADY_DOC_MIN_BYTES);
    }
}
