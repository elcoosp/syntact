//! Changed-range diffing (issue 030): the maximal byte ranges where the
//! old and new token lists disagree. The fuzz suite diffs this against a
//! brute-force reimplementation.

use std::ops::Range;

/// The maximal changed byte ranges between two token lists over the same
/// family of sources. Tokens equal in kind, start and end count as
/// unchanged; everything between the first and last mismatch (plus any
/// tail difference) collapses into one range per gap.
#[must_use]
pub fn changed_ranges(old: &[(u16, Range<u32>)], new: &[(u16, Range<u32>)]) -> Vec<Range<u32>> {
    let mut ranges = Vec::new();
    let mut idx = 0_usize;
    while idx < old.len().max(new.len()) {
        match (old.get(idx), new.get(idx)) {
            (Some((ok, or)), Some((nk_, nr))) if ok == nk_ && or == nr => {
                idx += 1; // unchanged token
            }
            (Some((_, or)), Some((_, nr))) => {
                // Mismatch: the span covers both sides' tokens.
                let start = or.start.min(nr.start);
                let end = or.end.max(nr.end);
                push_merged(&mut ranges, start..end);
                idx += 1;
            }
            (Some((_, or)), None) => {
                push_merged(&mut ranges, or.clone());
                idx += 1;
            }
            (None, Some((_, nr))) => {
                push_merged(&mut ranges, nr.clone());
                idx += 1;
            }
            (None, None) => break,
        }
    }
    ranges
}

fn push_merged(ranges: &mut Vec<Range<u32>>, range: Range<u32>) {
    if let Some(last) = ranges.last_mut() {
        if range.start <= last.end {
            last.end = last.end.max(range.end);
            return;
        }
    }
    ranges.push(range);
}

/// The brute-force reference: token-by-token comparison collecting the
/// union of differing spans (used by the differential tests).
#[must_use]
pub fn changed_ranges_brute_force(
    old: &[(u16, Range<u32>)],
    new: &[(u16, Range<u32>)],
) -> Vec<Range<u32>> {
    let mut spans: Vec<Range<u32>> = Vec::new();
    for idx in 0..old.len().max(new.len()) {
        match (old.get(idx), new.get(idx)) {
            (Some((ok, or)), Some((nk_, nr))) if ok == nk_ && or == nr => {}
            (Some((_, or)), Some((_, nr))) => {
                spans.push(or.start.min(nr.start)..or.end.max(nr.end));
            }
            (Some((_, or)), None) => spans.push(or.clone()),
            (None, Some((_, nr))) => spans.push(nr.clone()),
            (None, None) => break,
        }
    }
    // Merge overlapping/adjacent spans.
    spans.sort_by_key(|r| r.start);
    let mut merged: Vec<Range<u32>> = Vec::new();
    for span in spans {
        match merged.last_mut() {
            Some(last) if span.start <= last.end => {
                last.end = last.end.max(span.end);
            }
            _ => merged.push(span),
        }
    }
    merged
}
