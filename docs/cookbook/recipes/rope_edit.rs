// Recipe: feed the incremental engine from editor ropes.
//
// Roadmap §7.1: "ropey::Rope accepted as an alternative input for
// editor integrations — the incremental engine consumes rope deltas as
// edit descriptors (§7.7)". An editor holds rope state; the transition
// between the old and new rope converts into the engine's `Edit`
// descriptor (common prefix/suffix, the middle span as the
// replacement), and the descriptor applies back onto the rope — the
// §8 "ropey-compatible" round trip.

use incremental::rope::{apply_edit, edit_between};
use incremental::Rope;

#[test]
fn edit_descriptors_from_ropes() {
    let old_rope = Rope::from_str("{\"name\": \"syntact\", \"version\": 1}");
    let new_rope = Rope::from_str("{\"name\": \"syntact\", \"version\": 2}");

    // The state transition becomes one replacement descriptor: the
    // ropes' common prefix and suffix frame the changed middle span.
    let edit = edit_between(&old_rope, &new_rope).expect("the ropes differ");
    assert_eq!(edit.start, 31);
    assert_eq!(edit.end, 32);
    assert_eq!(edit.text, "2");
    assert_eq!(edit.apply(&old_rope.to_string()), new_rope.to_string());

    // The engine consumes rope-derived descriptors like any hand-built
    // edit; its output stream is equivalence-gated against a fresh full
    // lex in `crates/incremental/tests/rope.rs`.

    // And the descriptor applies back onto the rope (§8 ropey-compatible).
    let applied = apply_edit(&old_rope, &edit);
    assert_eq!(applied.to_string(), new_rope.to_string());

    // Equal ropes decline: there is no edit, nothing to classify.
    assert!(edit_between(&old_rope, &old_rope.clone()).is_none());
}
