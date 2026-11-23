//! The region-sink seam as a consumer: stream-validate sources through
//! a [`FaultScanner`] + [`scan_regions`], observe the fail-fast stop
//! (`regions_seen` vs the full region count), and compare with the
//! index-building [`scan`] — the streaming shape region-parallel
//! planning (roadmap P4) consumes.
//!
//! Run: `cargo run -p structural-index --example validate_stream`

use std::ops::ControlFlow;

use structural_index::{FaultScanner, RegionIndex, RegionKind, RegionSink, scan, scan_regions};

/// A counting consumer: how many regions closed, and the first
/// `StrDouble` string's span (any per-region inspection a host wants).
struct Probe {
    seen: usize,
    first_string: Option<(u32, u32)>,
}

impl RegionSink for Probe {
    fn region(&mut self, kind: RegionKind, start: u32, end: u32, _depth: u8) -> ControlFlow<()> {
        self.seen += 1;
        if self.first_string.is_none() && kind == RegionKind::StrDouble {
            self.first_string = Some((start, end));
        }
        ControlFlow::Continue(())
    }
}

fn main() {
    let sources = [
        "fn main() { let x = \"}\"; }",
        "let s = \"unterminated",
        "a { b",
        "// balanced } in a comment",
    ];
    for source in sources {
        let verdict = structural_index::validate_fast(source);
        println!("{source:?}: {verdict:?}");

        // Fail-fast observability: a faulting drive stops early.
        let mut scanner = FaultScanner::new(source);
        let flowed = scan_regions(source, &mut scanner);
        let seen = scanner.regions_seen();
        let verdict = scanner.finish();
        let full = scan(source).len();
        let stop = if flowed.is_break() {
            "broke early"
        } else {
            "ran to EOF"
        };
        println!("  streaming: {verdict:?}, {seen}/{full} regions ({stop})");

        // Any host consumer can ride the same drive: region counting +
        // first string span, still without materializing an index.
        let mut probe = Probe {
            seen: 0,
            first_string: None,
        };
        assert!(
            scan_regions(source, &mut probe).is_continue(),
            "the probe never breaks"
        );
        let index: RegionIndex = scan(source);
        assert_eq!(probe.seen, index.len(), "probe sees every region");
        if let Some((start, end)) = probe.first_string {
            assert_eq!(index.get(1).map(|(_, s, e, _)| (s, e)), Some((start, end)));
        }
    }
    println!("streaming validate + region probes — no index required");
}
