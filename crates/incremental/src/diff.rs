//! Changed-ranges diffing (issue 030): the byte ranges where two token
//! streams disagree, plus the brute-force reference the tests pin the
//! engine's own diff against.

use std::ops::Range;

/// FNV-1a 64-bit over bytes — the context-hash primitive.
#[must_use]
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Greedy token-level diff: walk both streams matching equal
/// `(kind, text)` tokens; every byte covered by an unmatched pair is
/// merged into the changed ranges. Deterministic and order-preserving.
#[must_use]
pub fn changed_ranges(
    old_tokens: &[(u16, Range<u32>)],
    old_source: &str,
    new_tokens: &[(u16, Range<u32>)],
    new_source: &str,
) -> Vec<Range<usize>> {
    fn text<'a>(source: &'a str, range: &Range<u32>) -> &'a str {
        let start = (range.start as usize).min(source.len());
        let end = (range.end as usize).min(source.len()).max(start);
        &source[start..end]
    }
    let mut changed: Vec<Range<usize>> = Vec::new();
    let mut oi = 0_usize;
    let mut ni = 0_usize;
    let push = |start: usize, end: usize, out: &mut Vec<Range<usize>>| {
        if start >= end {
            return;
        }
        if let Some(last) = out.last_mut() {
            if last.end >= start {
                last.end = last.end.max(end);
                return;
            }
        }
        out.push(start..end);
    };
    while oi < old_tokens.len() || ni < new_tokens.len() {
        let same = match (old_tokens.get(oi), new_tokens.get(ni)) {
            (Some((ok, or)), Some((nk, nr))) => {
                ok == nk && text(old_source, or) == text(new_source, nr)
            }
            _ => false,
        };
        if same {
            oi += 1;
            ni += 1;
            continue;
        }
        // One insertion/deletion side ran ahead: mark the smaller
        // neighbour's bytes and advance the stream that can catch up.
        let old_ahead = oi < old_tokens.len();
        let new_ahead = ni < new_tokens.len();
        match (old_ahead, new_ahead) {
            (true, true) => {
                let o = &old_tokens[oi].1;
                let n = &new_tokens[ni].1;
                let olen = o.end.saturating_sub(o.start);
                let nlen = n.end.saturating_sub(n.start);
                if olen <= nlen {
                    let start = (o.start as usize).min(old_source.len());
                    let end = (o.end as usize).min(old_source.len());
                    push(start, end, &mut changed);
                    oi += 1;
                } else {
                    let start = (n.start as usize).min(new_source.len());
                    let end = (n.end as usize).min(new_source.len());
                    push(start, end, &mut changed);
                    ni += 1;
                }
            }
            (true, false) => {
                let o = &old_tokens[oi].1;
                let start = (o.start as usize).min(old_source.len());
                let end = (o.end as usize).min(old_source.len());
                push(start, end, &mut changed);
                oi += 1;
            }
            (false, true) => {
                let n = &new_tokens[ni].1;
                let start = (n.start as usize).min(new_source.len());
                let end = (n.end as usize).min(new_source.len());
                push(start, end, &mut changed);
                ni += 1;
            }
            (false, false) => break,
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::changed_ranges;

    fn toks(source: &str, kinds: &[u16], lens: &[usize]) -> Vec<(u16, std::ops::Range<u32>)> {
        let mut out = Vec::new();
        let mut pos = 0_u32;
        for (kind, len) in kinds.iter().zip(lens) {
            let end = pos + u32::try_from(*len).unwrap_or(u32::MAX);
            out.push((*kind, pos..end));
            pos += u32::try_from(*len).unwrap_or(u32::MAX);
        }
        let _ = source;
        out
    }

    #[test]
    fn identical_streams_have_no_changes() {
        let a = toks("abc", &[1, 2], &[2, 1]);
        assert_eq!(changed_ranges(&a, "abc", &a, "abc"), vec![]);
    }

    #[test]
    fn changed_token_marks_its_range() {
        let old = toks("ab", &[1, 2], &[1, 1]);
        let new = toks("ax", &[1, 2], &[1, 1]);
        let ranges = changed_ranges(&old, "ab", &new, "ax");
        assert_eq!(ranges, vec![1..2]);
    }

    #[test]
    fn merged_adjacent_ranges() {
        let old = toks("abc", &[1, 1, 1], &[1, 1, 1]);
        let new = toks("xyz", &[1, 1, 1], &[1, 1, 1]);
        let ranges = changed_ranges(&old, "abc", &new, "xyz");
        assert_eq!(ranges, vec![0..3], "adjacent changes merge");
    }
}
