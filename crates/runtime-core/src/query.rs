//! # downstream queries — the §7.8 salsa layer
//!
//! The downstream derived-data layer (roadmap §7.8): "Downstream derived
//! data (symbols, scopes-lite, folds) lives behind a salsa DB
//! (`runtime-core::query` + salsa): `#[salsa::tracked(lru=…)]` queries
//! over green trees, durability-tiered file inputs — the RA/ruff-`ty`
//! pattern (§2.3). Cross-file incremental invalidation comes from salsa
//! revisions, not from our tree layer."
//!
//! Concretely:
//!
//! - [`SourceFile`] is the durability-tiered file input: the text is
//!   written once through [`SourceFile::builder`] (with an explicit
//!   [`Durability`]) and re-tiered per write through salsa's `set_text`
//!   setter — never mutated in place, so salsa revisions own all
//!   invalidation.
//! - [`parse_tree`] is the tracked parse query: it calls the DB's pack
//!   parse entry ([`DownstreamDb::parse_fn`], the
//!   `pack_registry::ParseFn` shape kept structural so the kernel does
//!   not depend on the registry) and memoizes the resulting green tree
//!   under an LRU (`lru = 128`, the RA value §2.3 cites).
//! - [`index`] materializes the positioned tree index (one DFS: spans,
//!   parent links, first-significant-token spans) once per text version;
//!   [`symbols`], [`folds`], and [`scopes`] are cheap filters over that
//!   memoized layer, so all three derived queries share the walk.
//!
//! The editor-shaped consumer of the same derived data (outline, folding,
//! highlights over documents that change every keystroke) remains the
//! pack LSP's document model — this module is the programmatic seam:
//! same products, salsa revision semantics, no LSP session.
//!
//! Soundness note: the [`DownstreamDb`] config methods
//! ([`DownstreamDb::parse_fn`], [`DownstreamDb::symbol_kinds`], …) are
//! construction-time constants of a DB (a pack does not change identity
//! mid-session). Reading them inside a tracked query is an untracked
//! read by salsa's rules; a DB that mutates them would under-invalidate.

use std::fmt;
use std::ops::Range;
use std::sync::Arc;

use crate::input::Source;
use crate::tree::{GreenChild, GreenNode};

pub use salsa::Durability;

// In-document offsets are bounded by the input layer's u32 length guard
// (`input::Source` rejects sources beyond u32::MAX), so usize→u32 casts
// of document offsets cannot truncate for any document that parsed at
// all.

/// A pack's lossless parse entry point: the exact shape every shipped
/// pack (and `pack_registry::ParseFn`) exposes — recovery keeps it
/// lossless, so the `Err` side is structural only.
pub type ParseFn = fn(&str) -> Result<Arc<GreenNode>, String>;

/// A structural parse failure (the pack's `Err` side; lossless recovery
/// keeps every pack's tree, so this surfaces only for entries that
/// reject the input outright).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseFailure {
    message: String,
}

impl ParseFailure {
    /// The pack's own structural-failure message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ParseFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse failed: {}", self.message)
    }
}

impl std::error::Error for ParseFailure {}

/// A file input of the downstream layer: the durability-tiered source
/// text one pack entry parses.
///
/// The text is written once through [`SourceFile::builder`] (with an
/// explicit [`Durability`] tier) and changed only through the generated
/// `set_text` setter — the RA pattern of §7.8: never mutated in place,
/// invalidation is owned by salsa revisions.
#[salsa::input(debug)]
pub struct SourceFile {
    /// The full document text (the field declaration's visibility makes
    /// the generated `text` getter and `set_text` setter public).
    pub text: String,
}

/// The pack configuration a downstream DB supplies: the parse entry and
/// the node kinds promoted to each derived product.
///
/// All four methods are construction-time constants of a DB (see the
/// module soundness note). `symbol_kinds`/`fold_kinds`/`scope_kinds` are
/// the pack's own dense node kinds (e.g. `packs_json::kinds`).
#[salsa::db]
pub trait DownstreamDb: salsa::Database {
    /// The pack's lossless parse entry ([`ParseFn`]).
    fn parse_fn(&self) -> ParseFn;

    /// Node kinds promoted to [`Symbol`]s.
    fn symbol_kinds(&self) -> &'static [u16];

    /// Node kinds promoted to [`Fold`]s.
    fn fold_kinds(&self) -> &'static [u16];

    /// Node kinds that open a scope (the scopes-lite product).
    fn scope_kinds(&self) -> &'static [u16];
}

/// The tracked parse query: the pack entry's green tree for `file`,
/// memoized under an LRU (§7.8: `#[salsa::tracked(lru=…)]`).
///
/// # Errors
/// The pack entry's structural `Err` side ([`ParseFailure`]); lossless
/// recovery makes this rare — malformed input normally parses with
/// ERROR nodes instead.
#[salsa::tracked(returns(clone), lru = 128)]
pub fn parse_tree(db: &dyn DownstreamDb, file: SourceFile) -> Result<Arc<GreenNode>, ParseFailure> {
    (db.parse_fn())(file.text(db)).map_err(|message| ParseFailure { message })
}

/// One positioned node of the [`Index`]: byte span, parent link, and the
/// span of the first significant (non-whitespace) token — the symbol
/// name source (the same naming rule the pack LSP's outline uses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexNode {
    /// Node kind (pack-local dense numbering).
    pub kind: u16,
    /// Byte span in the document.
    pub span: Range<u32>,
    /// Parent index into [`Index::nodes`] (`None` for the root).
    pub parent: Option<u32>,
    /// Span of the first token with a non-whitespace byte, if any.
    pub first_significant: Option<Range<u32>>,
}

/// The positioned tree index over one document version: nodes in
/// preorder (children follow their parent), built by one DFS.
///
/// This is the layer all derived queries share — [`symbols`],
/// [`folds`], and [`scopes`] are filters over this memoized walk, so a
/// document's derived data costs one parse and one DFS per text version.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Index {
    /// Nodes in preorder; `nodes[0]` is the root.
    pub nodes: Vec<IndexNode>,
}

impl Index {
    /// Build the index for `tree` over `text` (the text supplies the
    /// significance test for token spans).
    #[must_use]
    pub fn build(tree: &Arc<GreenNode>, text: &str) -> Self {
        let mut nodes = Vec::new();
        visit(&mut nodes, tree, None, 0, text);
        Self { nodes }
    }

    /// The nearest scope-kind ancestor of `index`, if any (`index`'s own
    /// kind is not consulted — the caller decided it is a scope).
    fn nearest_scope(&self, index: usize, scope_kinds: &[u16]) -> Option<u32> {
        let mut cursor = self.nodes[index].parent;
        while let Some(parent) = cursor {
            if scope_kinds.contains(&self.nodes[parent as usize].kind) {
                return Some(parent);
            }
            cursor = self.nodes[parent as usize].parent;
        }
        None
    }
}

/// Preorder DFS: enters nodes, advances the offset over tokens, and
/// records `(kind, span, parent, first-significant)` per node. Returns
/// the node's end offset (post-order completion).
fn visit(
    nodes: &mut Vec<IndexNode>,
    node: &GreenNode,
    parent: Option<u32>,
    start: usize,
    text: &str,
) -> usize {
    let index = nodes.len();
    nodes.push(IndexNode {
        kind: node.kind,
        span: start as u32..start as u32,
        parent,
        first_significant: None,
    });
    let mut offset = start;
    for child in &node.children {
        match child {
            GreenChild::Node(inner) => {
                let child_index = nodes.len();
                offset = visit(nodes, inner, Some(index as u32), offset, text);
                // A node's first significant token is the subtree's
                // first: a pair inherits its name token from its key.
                if nodes[index].first_significant.is_none() {
                    let inherited = nodes[child_index].first_significant.clone();
                    nodes[index].first_significant = inherited;
                }
            }
            GreenChild::Token { range, .. } => {
                let span =
                    (range.start as usize).min(text.len())..(range.end as usize).min(text.len());
                let slice = text.get(span.start..span.end).unwrap_or("");
                let significant = slice.bytes().any(|byte| !byte.is_ascii_whitespace());
                if significant && nodes[index].first_significant.is_none() {
                    nodes[index].first_significant = Some(span.start as u32..span.end as u32);
                }
                offset = span.end;
            }
        }
    }
    let end = u32::try_from(offset).unwrap_or(u32::MAX);
    nodes[index].span = start as u32..end;
    offset
}

/// The tracked positioned-index query (one DFS per text version).
#[salsa::tracked(returns(clone))]
pub fn index(db: &dyn DownstreamDb, file: SourceFile) -> Arc<Index> {
    match parse_tree(db, file) {
        Ok(tree) => Arc::new(Index::build(&tree, file.text(db))),
        Err(_) => Arc::default(),
    }
}

/// One derived symbol: a node whose kind the DB promotes
/// ([`DownstreamDb::symbol_kinds`]), named by its first significant
/// token (the pack LSP outline's naming rule).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    /// Node kind.
    pub kind: u16,
    /// Byte span of the whole node.
    pub range: Range<u32>,
    /// The name text (the first significant token's slice), when the
    /// node carries one.
    pub name: Option<String>,
    /// The name's byte span (the [`Symbol::name`] source), when present.
    pub name_range: Option<Range<u32>>,
}

/// The tracked symbols query: [`Symbol`]s in document order.
#[salsa::tracked(returns(clone))]
pub fn symbols(db: &dyn DownstreamDb, file: SourceFile) -> Arc<Vec<Symbol>> {
    let index = index(db, file);
    let text = file.text(db);
    Arc::new(
        index
            .nodes
            .iter()
            .filter(|node| db.symbol_kinds().contains(&node.kind))
            .map(|node| {
                let (name, name_range) = match node.first_significant.clone() {
                    Some(span) => (
                        Some(text[span.start as usize..span.end as usize].to_owned()),
                        Some(span),
                    ),
                    None => (None, None),
                };
                Symbol {
                    kind: node.kind,
                    range: node.span.clone(),
                    name,
                    name_range,
                }
            })
            .collect(),
    )
}

/// One derived folding region: a node whose kind the DB promotes
/// ([`DownstreamDb::fold_kinds`]), with the line bounds the editor folds
/// through (the last-byte convention of the pack LSP's folding view: the
/// node's final byte, floored to a char boundary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fold {
    /// Node kind.
    pub kind: u16,
    /// Byte span of the whole node.
    pub range: Range<u32>,
    /// Line the region opens on (0-based).
    pub start_line: u32,
    /// Line the region's last byte sits on (0-based).
    pub end_line: u32,
}

/// The tracked folds query: [`Fold`]s in document order.
#[salsa::tracked(returns(clone))]
pub fn folds(db: &dyn DownstreamDb, file: SourceFile) -> Arc<Vec<Fold>> {
    let index = index(db, file);
    let text = file.text(db);
    let source = Source::from_str_unchecked(text);
    Arc::new(
        index
            .nodes
            .iter()
            .filter(|node| db.fold_kinds().contains(&node.kind))
            .map(|node| {
                let mut last = node.span.end.saturating_sub(1).max(node.span.start);
                while last > node.span.start && !text.is_char_boundary(last as usize) {
                    last -= 1;
                }
                Fold {
                    kind: node.kind,
                    range: node.span.clone(),
                    start_line: source.line_col(node.span.start).0,
                    end_line: source.line_col(last).0,
                }
            })
            .collect(),
    )
}

/// One scopes-lite scope: a node whose kind the DB promotes
/// ([`DownstreamDb::scope_kinds`]), parented by its nearest scope-kind
/// ancestor (the scopes-lite nesting rule — non-scope nodes in between
/// are transparent).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// Node kind.
    pub kind: u16,
    /// Byte span of the whole scope.
    pub range: Range<u32>,
    /// Parent scope index into [`ScopeTree::scopes`] (`None` for a
    /// root scope).
    pub parent: Option<u32>,
}

/// The scopes-lite product: every scope in document order with parent
/// links into the flat [`Scope`] list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeTree {
    /// Scopes in document order (preorder over the positioned index).
    pub scopes: Vec<Scope>,
}

/// The tracked scopes-lite query: the [`ScopeTree`] for `file`.
#[salsa::tracked(returns(clone))]
pub fn scopes(db: &dyn DownstreamDb, file: SourceFile) -> Arc<ScopeTree> {
    let index = index(db, file);
    let scope_kinds = db.scope_kinds();
    // Scope parents are positions in the flat output list, not node
    // indexes — map each scope-kind node to its output position as the
    // preorder walk pushes it (parents always precede their children).
    let mut positions = vec![None; index.nodes.len()];
    let mut scopes = Vec::new();
    for (node_index, node) in index.nodes.iter().enumerate() {
        if !scope_kinds.contains(&node.kind) {
            continue;
        }
        let parent = index
            .nearest_scope(node_index, scope_kinds)
            .and_then(|parent| positions[parent as usize]);
        let position = u32::try_from(scopes.len()).unwrap_or(u32::MAX);
        positions[node_index] = Some(position);
        scopes.push(Scope {
            kind: node.kind,
            range: node.span.clone(),
            parent,
        });
    }
    Arc::new(ScopeTree { scopes })
}
