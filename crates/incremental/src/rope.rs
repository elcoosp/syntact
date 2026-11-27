//! Rope integration (roadmap §7.1, §7.7, §8): the editor's rope state
//! pair converts into the engine's [`Edit`] descriptor, and an [`Edit`]
//! applies back onto a rope.
//!
//! §7.1's plan line: *"`ropey::Rope` accepted as an alternative input for
//! editor integrations — the incremental engine consumes rope deltas as
//! edit descriptors (§7.7)"*. The deltas an editor produces are rope
//! state transitions (`old rope`, `new rope`); [`edit_between`] derives
//! the single-splice cover the engine's [`Edit`] models — common prefix
//! and suffix over characters, the middle span becomes the replacement —
//! and [`apply_edit`] closes the loop the other way (§8's
//! *"ropey-compatible"* annotation on the incremental sketch).
//!
//! The cover is conservative, never wrong: a multi-hunk rope change
//! yields one wide replacement span whose splice still reconstructs the
//! new text exactly (round-trip pinned by tests), so the engine's
//! damage classification only ever widens — its fast-path predicates and
//! fallback keep the stream correct. [`Rope`] is re-exported so
//! consumers pin one ropey version through this crate (the ADR-012
//! thin-wrapper posture).

use crate::edit::Edit;

pub use ropey::Rope;

/// The single-splice cover of the transition `old → new`: the common
/// character prefix and suffix, with the middle span of `new` as the
/// replacement text. `None` when the ropes are equal — there is no edit,
/// and the engine has nothing to classify.
///
/// The span is char-aligned by construction (prefix and suffix are
/// matched over `char`s, which ropey guarantees are the rope's
/// boundaries), so the replacement text is always a valid `&str`.
#[must_use]
pub fn edit_between(old: &Rope, new: &Rope) -> Option<Edit> {
    // 1. Common character prefix (tracks the byte offset in both ropes).
    let mut prefix_chars = 0_usize;
    let mut old_prefix_bytes = 0_usize;
    let mut new_prefix_bytes = 0_usize;
    let max = old.len_chars().min(new.len_chars());
    while prefix_chars < max {
        let old_char = old.char(prefix_chars);
        if old_char != new.char(prefix_chars) {
            break;
        }
        prefix_chars += 1;
        old_prefix_bytes += old_char.len_utf8();
        new_prefix_bytes += old_char.len_utf8();
    }

    // 2. Common character suffix (never past the prefix).
    let mut suffix_chars = 0_usize;
    let mut old_suffix_bytes = 0_usize;
    let mut new_suffix_bytes = 0_usize;
    while suffix_chars + prefix_chars < old.len_chars()
        && suffix_chars + prefix_chars < new.len_chars()
    {
        let old_char = old.char(old.len_chars() - 1 - suffix_chars);
        if old_char != new.char(new.len_chars() - 1 - suffix_chars) {
            break;
        }
        suffix_chars += 1;
        old_suffix_bytes += old_char.len_utf8();
        new_suffix_bytes += old_char.len_utf8();
    }

    let old_len_bytes = old.len_bytes();
    let new_len_bytes = new.len_bytes();
    if prefix_chars == old.len_chars() && prefix_chars == new.len_chars() {
        return None; // equal ropes
    }

    // 3. The middle span: `[old prefix end, old suffix start)` replaced
    //    by the new middle. `Edit::normalize` clips an inverted span
    //    (deletions that reach into the prefix) to a valid range.
    let start = old_prefix_bytes.min(old_len_bytes);
    let end = old_len_bytes.saturating_sub(old_suffix_bytes).max(start);
    let text_len = new_len_bytes - new_prefix_bytes - new_suffix_bytes;
    let text = new
        .slice(prefix_chars..new.len_chars() - suffix_chars)
        .to_string();
    debug_assert_eq!(text.len(), text_len, "the middle is the replacement");
    Some(Edit::replace(start, end, text))
}

/// Apply an engine [`Edit`] descriptor to `rope`: the `[start, end)` byte
/// span is removed and `text` inserted at `start`. Byte offsets are
/// resolved to ropey's character indices once per boundary (a linear
/// walk — rope positions are character-addressed).
///
/// # Panics
/// When `edit`'s byte offsets fall inside a multi-byte character of
/// `rope` (an engine descriptor always lands on the old source's token
/// boundaries; a foreign descriptor is rejected here rather than
/// silently misapplied).
#[must_use]
pub fn apply_edit(rope: &Rope, edit: &Edit) -> Rope {
    let start_char = byte_to_char(rope, edit.start);
    let end_char = byte_to_char(rope, edit.end);
    let mut out = rope.clone();
    if end_char > start_char {
        out.remove(start_char..end_char);
    }
    out.insert(start_char, &edit.text);
    out
}

/// The character index of `byte_offset` (a linear walk over `rope`).
///
/// # Panics
/// When the offset lands inside a multi-byte character.
fn byte_to_char(rope: &Rope, byte_offset: usize) -> usize {
    let mut seen = 0_usize;
    for (char_index, ch) in rope.chars().enumerate() {
        if seen == byte_offset {
            return char_index;
        }
        seen += ch.len_utf8();
        assert!(
            seen <= byte_offset,
            "byte offset {byte_offset} splits a character"
        );
    }
    if byte_offset == seen {
        return rope.len_chars();
    }
    panic!("byte offset {byte_offset} past the rope end ({seen})");
}
