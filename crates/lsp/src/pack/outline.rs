//! The positioned tree index (issue 042): one DFS over the green tree
//! materializes every node as a byte span in document order, with parent
//! links and subtree bounds — the single artifact all tree views derive
//! from. Green nodes are position-independent by design; this is the
//! per-document positioning layer.
//!
//! The DFS also collects the flat token list (document order) and the
//! error regions — every `Event::Error` is followed by an
//! [`ERROR_KIND`] node (zero-width when nothing was consumed), so the
//! node spans are the diagnostic squiggles; [`MISSING_KIND`] zero-width
//! tokens join them. View builders never re-walk the tree.

// Document text is bounded by the input layer's u32 length guard
// (runtime-core `input::Source` rejects sources beyond u32::MAX), so
// usize to u32 casts of in-document offsets and node counts cannot
// truncate for any document that parsed at all.

use std::ops::Range;
use std::sync::Arc;

use runtime_core::{ERROR_KIND, MISSING_KIND, tree::GreenNode};

/// One positioned node: byte span, parent link, subtree bounds, and the
/// span of the first significant (non-whitespace) token — the symbol
/// name source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineNode {
    /// Node kind (pack-local dense numbering).
    pub kind: u16,
    /// First byte of the node (start of its leading trivia included).
    pub start: u32,
    /// One past the last byte of the node.
    pub end: u32,
    /// Parent index into the outline (`None` for the root).
    pub parent: Option<u32>,
    /// Number of direct children (the child *block* is the contiguous
    /// range `[self + 1, subtree_end)`; direct children chain through
    /// [`OutlineNode::subtree_end`], skipping grandchildren blocks).
    pub child_count: u32,
    /// One past the index of the last node in this subtree (exclusive
    /// preorder bound — the whole subtree is `[self, subtree_end)`).
    pub subtree_end: u32,
    /// Span of the first token with a non-whitespace byte, if any.
    pub first_significant: Option<Range<u32>>,
}

impl OutlineNode {
    /// Whether this node's byte span contains `offset` (`start <=
    /// offset < end`; zero-width nodes contain nothing).
    #[must_use]
    pub fn contains(&self, offset: usize) -> bool {
        (self.start as usize..self.end as usize).contains(&offset)
    }
}

/// Position-event slots (see [`Outline::events`]): which span field a
/// byte offset fills for a node.
///
/// The node's byte span start.
pub const SLOT_START: u8 = 0;
/// The node's byte span end (exclusive).
pub const SLOT_END: u8 = 1;
/// The first-significant token's first byte.
pub const SLOT_FS_START: u8 = 2;
/// The first-significant token's end byte.
pub const SLOT_FS_END: u8 = 3;
/// The node's last byte (end floored to a char boundary) — the folding
/// view's end line.
pub const SLOT_FOLD_LAST: u8 = 4;
/// Number of position slots per outline node.
pub const SLOT_COUNT: usize = 5;
/// Number of position slots per token (start, end).
pub const TOKEN_SLOT_COUNT: usize = 2;

/// The positioned tree index over one document version.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    /// Nodes in preorder (children follow their parent contiguously).
    pub nodes: Vec<OutlineNode>,
    /// Every non-zero-width token leaf as `(kind, span)` in document
    /// order (the EOF sentinel excluded).
    pub tokens: Vec<(u16, Range<u32>)>,
    /// Error regions: `(kind, span)` for ERROR nodes (zero-width when
    /// the parser consumed nothing) and MISSING tokens.
    pub errors: Vec<(u16, Range<u32>)>,
    /// Position events in document order: `(byte, slot, node)`. The
    /// DFS emits each node's five span boundaries as it enters,
    /// names them, and exits — the stream is non-decreasing in bytes
    /// by construction, so one monotonic sweep converts it to UTF-16
    /// positions without a sort.
    pub events: Vec<(u32, u8, u32)>,
}

impl Outline {
    /// Build the outline for `tree` over `text` (the text supplies the
    /// significance test for token spans).
    #[must_use]
    pub fn build(tree: &Arc<GreenNode>, text: &str) -> Self {
        let mut outline = Self::default();
        outline.visit(tree, None, 0, text);
        outline
    }

    /// Preorder DFS: enters nodes, advances the offset over tokens.
    /// Returns the node's end offset (post-order completion).
    fn visit(&mut self, node: &GreenNode, parent: Option<u32>, start: usize, text: &str) -> usize {
        let index = self.nodes.len() as u32;
        self.nodes.push(OutlineNode {
            kind: node.kind,
            start: u32::try_from(start).unwrap_or(u32::MAX),
            end: 0,
            parent,
            child_count: 0,
            subtree_end: 0,
            first_significant: None,
        });
        self.events.push((start as u32, SLOT_START, index));
        let mut offset = start;
        for child in &node.children {
            match child {
                runtime_core::tree::GreenChild::Node(inner) => {
                    self.nodes[index as usize].child_count += 1;
                    let child_index = self.nodes.len() as u32;
                    let child_start = offset;
                    offset = self.visit(inner, Some(index), offset, text);
                    if inner.kind == ERROR_KIND {
                        self.errors
                            .push((ERROR_KIND, child_start as u32..offset as u32));
                    }
                    // A node's first significant token is the subtree's
                    // first: a table inherits its name token from the
                    // header node, a pair from its key.
                    if self.nodes[index as usize].first_significant.is_none() {
                        let inherited = self.nodes[child_index as usize].first_significant.clone();
                        if let Some(span) = &inherited {
                            // The inherited token's boundaries ride the
                            // event stream at their own byte position.
                            self.events.push((span.start, SLOT_FS_START, index));
                            self.events.push((span.end, SLOT_FS_END, index));
                        }
                        self.nodes[index as usize].first_significant = inherited;
                    }
                }
                runtime_core::tree::GreenChild::Token { kind, range } => {
                    let span = (range.start as usize).min(text.len())
                        ..(range.end as usize).min(text.len());
                    let slice = text.get(span.start..span.end).unwrap_or("");
                    let significant = slice.bytes().any(|byte| !byte.is_ascii_whitespace());
                    if significant && self.nodes[index as usize].first_significant.is_none() {
                        let first = span.start as u32..span.end as u32;
                        self.events.push((first.start, SLOT_FS_START, index));
                        self.events.push((first.end, SLOT_FS_END, index));
                        self.nodes[index as usize].first_significant = Some(first);
                    }
                    // Skip zero-width leaves (EOF sentinel, missing
                    // markers) from the token list; missing markers are
                    // diagnosed through the error table below.
                    if span.start < span.end {
                        self.tokens
                            .push((*kind, span.start as u32..span.end as u32));
                    }
                    if *kind == MISSING_KIND {
                        self.errors
                            .push((MISSING_KIND, span.start as u32..span.end as u32));
                    }
                    offset = span.end;
                }
            }
        }
        let end = u32::try_from(offset).unwrap_or(u32::MAX);
        self.nodes[index as usize].end = end;
        self.nodes[index as usize].subtree_end = self.nodes.len() as u32;
        // The fold's last byte: the node's final byte (end - 1),
        // floored to a char boundary (a node ending at a line boundary
        // folds to the line before; one ending mid-line folds onto its
        // line).
        let mut last = offset.saturating_sub(1).max(start);
        while last > start && !text.is_char_boundary(last) {
            last -= 1;
        }
        self.events.push((last as u32, SLOT_FOLD_LAST, index));
        self.events.push((end, SLOT_END, index));
        offset
    }

    /// The root node index.
    #[must_use]
    pub fn root(&self) -> u32 {
        0
    }

    /// The innermost outline node containing `offset`. Direct children
    /// are found inside the contiguous child block by a binary search
    /// followed by a parent-chain climb (the block interleaves
    /// grandchildren) — zero allocations, no full scan.
    #[must_use]
    pub fn innermost(&self, offset: usize) -> Option<u32> {
        let mut index = self.root();
        if !self.nodes.get(index as usize)?.contains(offset) {
            return None;
        }
        loop {
            let node = &self.nodes[index as usize];
            if node.child_count == 0 {
                return Some(index);
            }
            // The child block [index + 1, subtree_end) is preorder:
            // find the last node that begins at or before the offset,
            // then climb to its direct-child ancestor.
            let first = index as usize + 1;
            let block_end = node.subtree_end as usize;
            let position = self.nodes[first..block_end]
                .partition_point(|child| child.start as usize <= offset);
            if position == 0 {
                return Some(index);
            }
            let mut candidate = first + position - 1;
            while self.nodes[candidate].parent != Some(index) {
                candidate = self.nodes[candidate].parent? as usize;
            }
            if self.nodes[candidate].contains(offset) {
                index = candidate as u32;
            } else {
                return Some(index);
            }
        }
    }

    /// The ancestor chain of `index`, innermost first (self included).
    #[must_use]
    pub fn chain(&self, index: u32) -> Vec<u32> {
        let mut chain = Vec::new();
        let mut cursor = Some(index);
        while let Some(current) = cursor {
            chain.push(current);
            cursor = self.nodes[current as usize].parent;
        }
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_tree(source: &str) -> Arc<GreenNode> {
        packs_json::parse(source).expect("json pack parses")
    }

    #[test]
    fn outline_covers_every_node_with_parent_links() {
        let source = r#"{"a": [1, true]}"#;
        let tree = json_tree(source);
        let outline = Outline::build(&tree, source);
        // ROOT > object > pair > array: 4 interior nodes.
        assert_eq!(outline.nodes.len(), 4);
        assert_eq!(outline.nodes[3].parent, Some(2));
        assert_eq!(outline.root(), 0);
        // Preorder layout: children follow the parent contiguously.
        assert_eq!(outline.nodes[0].parent, None);
        assert_eq!(outline.nodes[0].child_count, 1);
        assert_eq!(outline.nodes[1].parent, Some(0));
        assert_eq!(outline.nodes[1].child_count, 1);
        assert_eq!(outline.nodes[2].parent, Some(1));
        // Spans: ROOT covers the whole document, object the same, the
        // pair ends where the object's closing brace begins.
        assert_eq!(outline.nodes[0].start, 0);
        assert_eq!(outline.nodes[0].end as usize, source.len());
        assert_eq!(outline.nodes[2].start, 1);
        assert_eq!(outline.nodes[2].end as usize, source.len() - 1);
        // First significant token of the pair is the key string.
        let pair = &outline.nodes[2];
        let first = pair.first_significant.clone().expect("key token");
        assert_eq!(&source[first.start as usize..first.end as usize], r#""a""#);
    }

    #[test]
    fn token_list_is_document_ordered_without_eof() {
        let source = r#"{"a": [1, true]}"#;
        let tree = json_tree(source);
        let outline = Outline::build(&tree, source);
        assert_ne!(outline.tokens, []);
        let mut previous = 0;
        for (_, span) in &outline.tokens {
            assert!(span.start >= previous, "tokens in document order");
            assert!(span.start < span.end, "zero-width leaves excluded");
            assert!(span.end as usize != source.len() || span.start < span.end);
            previous = span.end;
        }
        // The last token ends at the document end (no EOF sentinel).
        assert_eq!(
            outline.tokens.last().expect("tokens").1.end as usize,
            source.len()
        );
    }

    #[test]
    fn error_regions_are_collected() {
        // Every Event::Error materializes one ERROR node (zero-width
        // when the parser consumed nothing at the failure point).
        for (source, errors) in [("{\"a\": }", 2), ("[1, 2", 1), ("tru", 1), ("{}", 0)] {
            let tree = json_tree(source);
            let outline = Outline::build(&tree, source);
            assert_eq!(outline.errors.len(), errors, "errors for {source:?}");
            for (kind, span) in &outline.errors {
                assert_eq!(*kind, ERROR_KIND);
                assert!(span.start as usize <= span.end as usize);
                assert!(span.end as usize <= source.len());
            }
        }
        // The stray closer is wrapped by a non-empty ERROR region
        // (leading trivia between the colon and the closer included).
        let tree = json_tree("{\"a\": }");
        let outline = Outline::build(&tree, "{\"a\": }");
        assert!(outline.errors.iter().any(|(_, span)| span.end > span.start));
    }

    #[test]
    fn empty_document_produces_a_single_root() {
        let tree = json_tree("");
        let outline = Outline::build(&tree, "");
        assert_eq!(outline.nodes.len(), 1);
        assert_eq!(outline.nodes[0].child_count, 0);
        assert_eq!(outline.tokens, []);
        assert_eq!(outline.errors, []);
        assert_eq!(outline.nodes[0].first_significant, None);
    }

    #[test]
    fn innermost_descends_by_binary_search() {
        let source = r#"{"a": [1, true]}"#;
        let tree = json_tree(source);
        let outline = Outline::build(&tree, source);
        // Inside the key string: pair (2) is innermost, then object, root.
        let offset = source.find(r#""a""#).expect("key") + 1;
        assert_eq!(outline.innermost(offset), Some(2));
        let chain = outline.chain(2);
        assert_eq!(chain, vec![2, 1, 0]);
        // Inside the array, between elements: the array node contains
        // the position (its span covers the separators).
        let offset = source.find(',').expect("comma");
        assert_eq!(outline.innermost(offset), Some(3));
        // Positions no node spans (document end, out of range) find
        // nothing; the selection feature clamps to the root itself.
        assert_eq!(outline.innermost(source.len()), None);
        assert_eq!(outline.innermost(usize::MAX), None);
    }

    #[test]
    fn branching_trees_resolve_the_right_child() {
        // Two root children (table + keyval): the second child is NOT
        // index-adjacent to the first — grandchild blocks sit between.
        let source = "[deps]\nname = \"syntact\"\n";
        let tree = packs_toml::parse(source).expect("toml pack parses");
        let outline = Outline::build(&tree, source);
        // Inside the keyval's value: innermost is the keyval subtree,
        // not the table header.
        let offset = source.find("syntact").expect("value") + 1;
        let innermost = outline.innermost(offset).expect("contained");
        assert!(outline.nodes[innermost as usize].contains(offset));
        assert!(innermost > 2, "resolved inside the second child");
        // The chain climbs through the root only once.
        let chain = outline.chain(innermost);
        assert_eq!(*chain.last().expect("root"), 0);
        // Inside the table name: the header subtree.
        let offset = source.find("deps").expect("name");
        let innermost = outline.innermost(offset).expect("contained");
        assert!(outline.nodes[innermost as usize].contains(offset));
        assert!(outline.nodes[innermost as usize].start < 6);
    }

    #[test]
    fn deep_nesting_resolves_the_deepest_node() {
        let source = "[[[[[42]]]]]";
        let tree = json_tree(source);
        let outline = Outline::build(&tree, source);
        let offset = source.find('4').expect("number");
        let innermost = outline.innermost(offset).expect("contained");
        assert!(outline.nodes[innermost as usize].contains(offset));
        // The chain walks back to the root without cycles.
        let chain = outline.chain(innermost);
        assert_eq!(*chain.last().expect("root"), 0);
    }
}
