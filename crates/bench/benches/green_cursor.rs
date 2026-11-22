//! Criterion suite for the green-tree traversal seam (issue 044): the
//! [`runtime_core::view::GreenCursor`] DFS walk over a parsed JSON green
//! tree. The cursor is the architectural seam typed-view packs build on;
//! this suite guards its per-event cost against the steady-state document.

use criterion::{Criterion, criterion_group, criterion_main};
use packs_json::parse;
use runtime_core::view::{GreenCursor, WalkEvent, walk};

/// Run the full cursor walk over a parsed tree, counting events (mirrors
/// what a typed-view consumer does: descend, project, ascend).
fn walk_events(tree: &std::sync::Arc<runtime_core::tree::GreenNode>) -> usize {
    let mut enters = 0usize;
    let mut leaves = 0usize;
    let mut tokens = 0usize;
    let mut cursor = GreenCursor::new(tree);
    while let Some(event) = cursor.next_event() {
        match event {
            WalkEvent::Enter(_) => enters += 1,
            WalkEvent::Leave(_) => leaves += 1,
            WalkEvent::Token { .. } => tokens += 1,
        }
    }
    enters + leaves + tokens
}

/// The iterator form (`walk`) — the convenience wrapper a consumer reaches
/// for first; guards that it lands in the same order as the manual cursor.
fn walk_iter(tree: &std::sync::Arc<runtime_core::tree::GreenNode>) -> usize {
    walk(tree).count()
}

fn cursor_walk(c: &mut Criterion) {
    for (source, name) in [
        (bench::small_document(), "small"),
        (bench::medium_document(), "medium"),
        (bench::steady_state_document(), "steady"),
    ] {
        let tree = parse(&source).expect("parses");
        c.bench_function(&format!("view/cursor_walk/{name}"), |b| {
            b.iter(|| walk_events(&tree));
        });
        c.bench_function(&format!("view/walk_iter/{name}"), |b| {
            b.iter(|| walk_iter(&tree));
        });
    }
}

criterion_group!(benches, cursor_walk);
criterion_main!(benches);
