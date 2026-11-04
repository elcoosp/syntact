//! # persistent — versioned red-tree handles (issue 036, §7.9)
//!
//! **Persistent-red mode**: every reparse produces a NEW immutable
//! version; old versions stay valid forever (version retention), and
//! unchanged subtrees are shared by `Arc` (structural sharing via
//! copy-on-write splice). There is **no `&mut` read API** — a version
//! is `&self` from construction to drop.
//!
//! **Single-writer / multi-reader invariants** (documented contract):
//! - exactly ONE writer owns the [`VersionLog`] (`&mut self` on
//!   `commit` is the only mutating entry point);
//! - readers hold [`RedVersion`] handles (or the `Arc<GreenNode>`
//!   root directly) and read through `&self` with no locks: `Arc`
//!   loads are atomic and the pointees are immutable;
//! - a committed version is never modified or uncommitted — the log
//!   is append-only, so a reader's handle outlives any number of
//!   later commits;
//! - sharing is observable: [`splice_child`] clones the unchanged
//!   child `Arc`s, so `Arc::ptr_eq(old_child, new_child)` holds for
//!   every untouched branch (the sharing measurement in the tests).

use std::ops::Range;
use std::sync::Arc;

use crate::input::Source;
use crate::tree::GreenNode;

/// One immutable version of the red tree (a persistent handle).
#[derive(Debug)]
pub struct RedVersion {
    version: u64,
    parent: Option<u64>,
    root: Arc<GreenNode>,
}

impl RedVersion {
    /// This version's id (monotonic per [`VersionLog`]).
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version
    }

    /// The parent version id (`None` for the initial version).
    #[must_use]
    pub fn parent(&self) -> Option<u64> {
        self.parent
    }

    /// The version's root tree (`Arc` — share it freely across
    /// threads; reads need no writer).
    #[must_use]
    pub fn root(&self) -> &Arc<GreenNode> {
        &self.root
    }

    /// Byte-faithful render (the read path: `&self`, no mutation).
    #[must_use]
    pub fn render(&self, source: &Source<'_>) -> String {
        self.root.render(source)
    }
}

/// The append-only version log — the single-writer side.
#[derive(Debug, Default)]
pub struct VersionLog {
    versions: Vec<Arc<RedVersion>>,
}

impl VersionLog {
    /// An empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The current head (readers clone the `Arc` and read forever).
    #[must_use]
    pub fn head(&self) -> Option<Arc<RedVersion>> {
        self.versions.last().cloned()
    }

    /// A historical version — retention is unconditional (old
    /// handles stay valid after any number of edits).
    #[must_use]
    pub fn version(&self, version: u64) -> Option<Arc<RedVersion>> {
        self.versions.get(usize::try_from(version).ok()?).cloned()
    }

    /// The number of retained versions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.versions.len()
    }

    /// Whether the log retained no versions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.versions.is_empty()
    }

    /// The writer's only mutation: commit a new version derived from
    /// `base` with the (possibly spliced) root.
    #[must_use]
    pub fn commit(
        &mut self,
        base: Option<&Arc<RedVersion>>,
        root: Arc<GreenNode>,
    ) -> Arc<RedVersion> {
        let version = u64::try_from(self.versions.len()).unwrap_or(u64::MAX);
        let handle = Arc::new(RedVersion {
            version,
            parent: base.map(|handle| handle.version()),
            root,
        });
        self.versions.push(Arc::clone(&handle));
        handle
    }
}

/// Copy-on-write splice: replace `root.children[branch]` with
/// `replacement`, sharing every other child by `Arc` clone. The new
/// root is a fresh node; untouched branches remain pointer-identical
/// to the old tree (the sharing measurement).
#[must_use]
pub fn splice_child(
    root: &GreenNode,
    branch: usize,
    replacement: &Arc<GreenNode>,
) -> Arc<GreenNode> {
    let mut children = Vec::with_capacity(root.children.len());
    for (idx, child) in root.children.iter().enumerate() {
        if idx == branch {
            children.push(crate::tree::GreenChild::Node(replacement.clone()));
        } else {
            children.push(child.clone());
        }
    }
    let text_len = children.iter().map(crate::tree::GreenChild::text_len).sum();
    let has_error = children.iter().any(crate::tree::GreenChild::has_error);
    Arc::new(GreenNode {
        kind: root.kind,
        text_len,
        has_error,
        children,
    })
}

/// Count the pointer-identical subtree `Arc`s shared between two
/// trees (the sharing measurement: walk both trees in lockstep).
#[must_use]
pub fn shared_arcs(old: &GreenNode, new: &GreenNode) -> usize {
    fn walk(a: &GreenNode, b: &GreenNode, shared: &mut usize) {
        for (ca, cb) in a.children.iter().zip(b.children.iter()) {
            if let (crate::tree::GreenChild::Node(x), crate::tree::GreenChild::Node(y)) = (ca, cb) {
                if Arc::ptr_eq(x, y) {
                    *shared += 1;
                    walk(x, y, shared);
                }
            }
        }
    }
    let mut shared = 0;
    walk(old, new, &mut shared);
    shared
}

/// Sum of text lengths covered by pointer-shared subtrees (bytes
/// reused old→new).
#[must_use]
pub fn shared_bytes(old: &GreenNode, new: &GreenNode) -> u32 {
    fn walk(a: &GreenNode, b: &GreenNode, bytes: &mut u32) {
        for (ca, cb) in a.children.iter().zip(b.children.iter()) {
            if let (crate::tree::GreenChild::Node(x), crate::tree::GreenChild::Node(y)) = (ca, cb) {
                if Arc::ptr_eq(x, y) {
                    *bytes += x.text_len;
                    walk(x, y, bytes);
                }
            }
        }
    }
    let mut bytes = 0;
    walk(old, new, &mut bytes);
    bytes
}

/// The byte ranges of a tree's top-level node children (splice
/// branch lookup: which child covers `offset`).
#[must_use]
pub fn branch_covering(root: &GreenNode, offset: u32) -> Option<usize> {
    let mut cursor = 0_u32;
    for (idx, child) in root.children.iter().enumerate() {
        let len = child.text_len();
        if offset < cursor + len {
            return Some(idx);
        }
        cursor += len;
    }
    None
}

/// Chunk-plan type alias: a chunk is a half-open byte range (the
/// shape `plan_chunks` produces over the `SoA` region index).
pub type ChunkPlan = Vec<Range<u32>>;
