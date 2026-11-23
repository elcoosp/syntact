//! # packs-json typed views — zero-copy projections over the green tree
//!
//! The reference [`parse`](crate::parse) returns an `Arc<GreenNode>` rooted
//! at [`crate::reference::ROOT_KIND`]. This module gives consumers typed
//! accessors (`Value`, `Object`, `Array`) that project over that tree without
//! copying — the §16.C.1 "typed views" default, implemented on the
//! [`runtime_core::view::TreeView`] seam rather than via generated ASTs.
//!
//! # Tree shape (after [`crate::parse`])
//!
//! ```text
//! ROOT_KIND
//! ├─ value: OBJECT | ARRAY | scalar-token | ERROR-node
//! └─ EOF token (zero-width)
//! ```
//! - `OBJECT` children: `LBRACE`, `PAIR`*, `RBRACE` (punctuation tokens
//!   interleaved with `PAIR` nodes and `,` separators).
//! - `PAIR` children: `STRING` (key), `COLON`, value (a node OR a scalar
//!   token leaf), plus `WHITESPACE` trivia leaves.
//! - `ARRAY` children: `LBRACK`, value*, `RBRACK` (values are nodes OR
//!   scalar token leaves, interleaved with `,` separators).
//! - Scalars (`STRING`, `NUMBER`, `TRUE`, `FALSE`, `NULL`) are token leaves
//!   carrying the literal's byte range — they have no wrapping node.
//!
//! # Zero-copy contract
//!
//! Every accessor borrows the underlying `Arc<GreenNode>` and the `Source`
//! for `'t`; no `String` is allocated. `as_str` returns `&'t str` borrowing
//! the source text. A typed view is invalidated when the source `Arc` is
//! dropped — the `'t` borrow enforces that.

use std::ops::Range;
use std::sync::Arc;

use runtime_core::input::Source;
use runtime_core::tree::{GreenChild, GreenNode};
use runtime_core::view::TreeView;

use crate::kinds;
use crate::reference::token_kinds::{COLON, COMMA, LBRACE, LBRACK, RBRACE, RBRACK, WHITESPACE};

/// A typed projection over a JSON value.
///
/// A value is either a *node* (`OBJECT`, `ARRAY`, `ERROR`) or a *scalar
/// token leaf* (`STRING`, `NUMBER`, `TRUE`, `FALSE`, `NULL`). The
/// discriminator ([`Value::value_kind`]) reads the kind from whichever the
/// backing green child carries.
#[derive(Debug, Clone, Copy)]
pub struct Value<'t> {
    /// The root tree (the `ROOT_KIND` wrapper) — needed for the
    /// [`TreeView`] cursor.
    root: &'t Arc<GreenNode>,
    /// The source text (for zero-copy string slices).
    source: &'t Source<'t>,
    /// The backing green child (a node or a scalar token leaf).
    inner: ValueInner<'t>,
    /// The byte offset of this value within the source (absolute span start).
    offset: u32,
}

/// The backing green child of a [`Value`]: either an interior node or a
/// scalar token leaf (JSON scalars have no wrapping node).
#[derive(Debug, Clone, Copy)]
enum ValueInner<'t> {
    /// An interior node (`OBJECT`, `ARRAY`, `ERROR`).
    Node(&'t GreenNode),
    /// A scalar token leaf (`STRING`, `NUMBER`, `TRUE`, `FALSE`, `NULL`).
    Token {
        /// Token kind.
        kind: runtime_core::TokenKind,
        /// Byte range start (inclusive) in the source.
        start: u32,
        /// Byte range end (exclusive) in the source.
        end: u32,
    },
}

impl<'t> Value<'t> {
    /// Project a typed value over a parsed JSON tree and its source. The
    /// root must be the `ROOT_KIND` wrapper produced by [`crate::parse`];
    /// the value is the root's first node child (objects, arrays) or its
    /// first scalar token leaf (scalar documents).
    #[must_use]
    pub fn new(root: &'t Arc<GreenNode>, source: &'t Source<'t>) -> Self {
        match root.children.first() {
            Some(GreenChild::Node(node)) => Self {
                root,
                source,
                inner: ValueInner::Node(node),
                offset: 0,
            },
            Some(GreenChild::Token { kind, range, .. }) => Self {
                root,
                source,
                inner: ValueInner::Token {
                    kind: *kind,
                    start: range.start,
                    end: range.end,
                },
                offset: range.start,
            },
            None => Self {
                root,
                source,
                inner: ValueInner::Node(root.as_ref()),
                offset: 0,
            },
        }
    }

    /// The value's discriminator.
    #[must_use]
    pub fn value_kind(&self) -> ValueKind {
        match self.inner {
            ValueInner::Node(node) => match node.kind {
                kinds::OBJECT => ValueKind::Object,
                kinds::ARRAY => ValueKind::Array,
                kinds::STRING => ValueKind::String,
                kinds::NUMBER => ValueKind::Number,
                kinds::TRUE => ValueKind::True,
                kinds::FALSE => ValueKind::False,
                kinds::NULL => ValueKind::Null,
                // ERROR_KIND, ROOT_KIND, and any unrecognized kind all map
                // to Error; collapse to the wildcard.
                _ => ValueKind::Error,
            },
            ValueInner::Token { kind, .. } => match kind {
                kinds::STRING => ValueKind::String,
                kinds::NUMBER => ValueKind::Number,
                kinds::TRUE => ValueKind::True,
                kinds::FALSE => ValueKind::False,
                kinds::NULL => ValueKind::Null,
                _ => ValueKind::Error,
            },
        }
    }

    /// Borrow this value as an [`Object`] when it is one, else `None`.
    #[must_use]
    pub fn as_object(&self) -> Option<Object<'t>> {
        match self.inner {
            ValueInner::Node(node) if node.kind == kinds::OBJECT => Some(Object { value: *self }),
            _ => None,
        }
    }

    /// Borrow this value as an [`Array`] when it is one, else `None`.
    #[must_use]
    pub fn as_array(&self) -> Option<Array<'t>> {
        match self.inner {
            ValueInner::Node(node) if node.kind == kinds::ARRAY => Some(Array { value: *self }),
            _ => None,
        }
    }

    /// The string literal's bytes (including quotes), for a
    /// [`ValueKind::String`]. Returns `None` for non-string values.
    #[must_use]
    pub fn as_str(&self) -> Option<&'t str> {
        match self.inner {
            ValueInner::Token { kind, start, end } if kind == kinds::STRING => {
                Some(self.source.slice(start..end))
            }
            ValueInner::Node(node) if node.kind == kinds::STRING => {
                Self::first_token_range(node).map(|r| self.source.slice(r))
            }
            _ => None,
        }
    }

    /// The number literal's bytes, for a [`ValueKind::Number`]. Returns
    /// `None` for non-number values.
    #[must_use]
    pub fn as_number(&self) -> Option<&'t str> {
        match self.inner {
            ValueInner::Token { kind, start, end } if kind == kinds::NUMBER => {
                Some(self.source.slice(start..end))
            }
            ValueInner::Node(node) if node.kind == kinds::NUMBER => {
                Self::first_token_range(node).map(|r| self.source.slice(r))
            }
            _ => None,
        }
    }

    /// The byte span of this value in the source.
    #[must_use]
    pub fn span(&self) -> Range<u32> {
        match self.inner {
            ValueInner::Node(node) => self.offset..self.offset + node.text_len,
            ValueInner::Token { start, end, .. } => start..end,
        }
    }

    /// Whether this value's subtree contains an `ERROR` node (recovery
    /// landed inside). A typed accessor on an error-bearing value still
    /// returns data (the tree is lossless); callers that need a hard
    /// guarantee check this first.
    #[must_use]
    pub fn has_error(&self) -> bool {
        match self.inner {
            ValueInner::Node(node) => node.has_error,
            ValueInner::Token { kind, .. } => kind == runtime_core::ERROR_KIND,
        }
    }

    /// The first non-trivia token leaf's range under `node` (for scalar
    /// values backed by a node, when the parser wraps them).
    fn first_token_range(node: &'t GreenNode) -> Option<Range<u32>> {
        node.children.iter().find_map(|child| match child {
            GreenChild::Token { kind, range } if *kind != WHITESPACE => Some(range.clone()),
            GreenChild::Token { .. } | GreenChild::Node(_) => None,
        })
    }

    /// Build a child value over a green child at a known absolute offset.
    fn from_child(
        root: &'t Arc<GreenNode>,
        source: &'t Source<'t>,
        child: &'t GreenChild,
        offset: u32,
    ) -> Self {
        match child {
            GreenChild::Node(node) => Self {
                root,
                source,
                inner: ValueInner::Node(node),
                offset,
            },
            GreenChild::Token { kind, range } => Self {
                root,
                source,
                inner: ValueInner::Token {
                    kind: *kind,
                    start: range.start,
                    end: range.end,
                },
                offset: range.start,
            },
        }
    }
}

impl<'t> TreeView<'t> for Value<'t> {
    fn kind(&self) -> runtime_core::NodeKind {
        match self.inner {
            ValueInner::Node(node) => node.kind,
            ValueInner::Token { kind, .. } => kind,
        }
    }
    fn span(&self, _source: &Source<'_>) -> Range<u32> {
        self.span()
    }
    fn has_error(&self) -> bool {
        self.has_error()
    }
    fn cursor(&self) -> runtime_core::view::GreenCursor<'t> {
        runtime_core::view::GreenCursor::new(self.root)
    }
}

/// A typed view over a JSON object value.
#[derive(Debug, Clone, Copy)]
pub struct Object<'t> {
    value: Value<'t>,
}

impl<'t> Object<'t> {
    /// The object's `PAIR` children, as `(key, value)` pairs in document
    /// order. The key is a `&'t str` borrowing the source (the raw key
    /// token, including quotes); the value is a typed [`Value`]. A pair
    /// whose value subtree contains an `ERROR` node still projects — the
    /// caller decides whether to consume it.
    #[must_use]
    pub fn pairs(&self) -> Vec<(&'t str, Value<'t>)> {
        let mut out = Vec::new();
        let ValueInner::Node(object) = self.value.inner else {
            return out;
        };
        let mut pair_offset = self.value.offset;
        for child in &object.children {
            let len = child.text_len();
            if let GreenChild::Node(pair) = child {
                if pair.kind == kinds::PAIR {
                    if let Some(entry) = self.pair_entry(pair, pair_offset) {
                        out.push(entry);
                    }
                }
            }
            pair_offset = pair_offset.saturating_add(len);
        }
        out
    }

    /// The object's key strings (raw tokens, including quotes) in document
    /// order.
    #[must_use]
    pub fn keys(&self) -> Vec<&'t str> {
        self.pairs().into_iter().map(|(k, _)| k).collect()
    }

    /// Resolve one `PAIR` node to its `(key, value)` projection. The key
    /// is the first `STRING` token leaf; the value is the first child that
    /// is not the key, `COLON`, `,`, `{`, `}`, `[`, `]`, or trivia.
    fn pair_entry(&self, pair: &'t GreenNode, pair_offset: u32) -> Option<(&'t str, Value<'t>)> {
        let mut key_range: Option<Range<u32>> = None;
        let mut value_child: Option<&'t GreenChild> = None;
        let mut value_offset = pair_offset;
        let mut after_key = false;
        for child in &pair.children {
            let len = child.text_len();
            match child {
                GreenChild::Token { kind, range } => {
                    if key_range.is_none() && *kind == kinds::STRING {
                        key_range = Some(range.clone());
                        after_key = true;
                    } else if after_key && !is_structural(*kind) && *kind != WHITESPACE {
                        value_child = Some(child);
                        break;
                    }
                }
                GreenChild::Node(_) if after_key => {
                    value_child = Some(child);
                    break;
                }
                GreenChild::Node(_) => {}
            }
            value_offset = value_offset.saturating_add(len);
        }
        let key = self.value.source.slice(key_range?.clone());
        let value = Value::from_child(
            self.value.root,
            self.value.source,
            value_child?,
            value_offset,
        );
        Some((key, value))
    }
}

/// A typed view over a JSON array value.
#[derive(Debug, Clone, Copy)]
pub struct Array<'t> {
    value: Value<'t>,
}

impl<'t> Array<'t> {
    /// The array's element values in document order. Structural punctuation
    /// tokens (`[`, `]`, `,`) and trivia are skipped; each remaining child
    /// (a node or a scalar token leaf) projects as a [`Value`].
    #[must_use]
    pub fn items(&self) -> Vec<Value<'t>> {
        let mut out = Vec::new();
        let ValueInner::Node(array) = self.value.inner else {
            return out;
        };
        let mut offset = self.value.offset;
        for child in &array.children {
            let len = child.text_len();
            if is_value_child(child) {
                out.push(Value::from_child(
                    self.value.root,
                    self.value.source,
                    child,
                    offset,
                ));
            }
            offset = offset.saturating_add(len);
        }
        out
    }
}

/// Whether a token kind is JSON structural punctuation (not a value).
fn is_structural(kind: runtime_core::TokenKind) -> bool {
    matches!(kind, LBRACE | RBRACE | LBRACK | RBRACK | COLON | COMMA)
}

/// Whether a green child is a JSON value (a node, or a scalar token leaf
/// that is not structural punctuation or trivia).
fn is_value_child(child: &GreenChild) -> bool {
    match child {
        GreenChild::Node(_) => true,
        GreenChild::Token { kind, .. } => !is_structural(*kind) && *kind != WHITESPACE,
    }
}

/// The JSON value discriminator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    /// An object (`{ … }`).
    Object,
    /// An array (`[ … ]`).
    Array,
    /// A string literal.
    String,
    /// A number literal.
    Number,
    /// The `true` literal.
    True,
    /// The `false` literal.
    False,
    /// The `null` literal.
    Null,
    /// An error region or an unrecognized node (recovery landed here).
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;
    use runtime_core::input::Source;

    fn parse_tree(source: &str) -> Arc<GreenNode> {
        parse(source).expect("lossless parse")
    }

    #[test]
    fn object_value_projects_as_object() {
        let src = r#"{"a": 1, "b": 2}"#;
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        assert_eq!(value.value_kind(), ValueKind::Object);
        let object = value.as_object().expect("is object");
        assert_eq!(object.pairs().len(), 2);
    }

    #[test]
    fn object_keys_are_the_raw_key_tokens() {
        let src = r#"{"a": 1, "b": 2}"#;
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        let object = value.as_object().expect("is object");
        let keys = object.keys();
        assert_eq!(keys, vec![r#""a""#, r#""b""#]);
    }

    #[test]
    fn object_pair_values_carry_their_kinds() {
        let src = r#"{"a": 1, "b": [2, 3]}"#;
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        let object = value.as_object().expect("is object");
        let pairs = object.pairs();
        assert_eq!(pairs.len(), 2);
        let (_, a) = &pairs[0];
        assert_eq!(a.value_kind(), ValueKind::Number);
        assert_eq!(a.as_number(), Some("1"));
        let (_, b) = &pairs[1];
        assert_eq!(b.value_kind(), ValueKind::Array);
    }

    #[test]
    fn array_value_projects_as_array() {
        let src = "[1, 2, 3]";
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        assert_eq!(value.value_kind(), ValueKind::Array);
        let array = value.as_array().expect("is array");
        assert_eq!(array.items().len(), 3);
    }

    #[test]
    fn number_scalar_projects_as_number() {
        let src = "42";
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        assert_eq!(value.value_kind(), ValueKind::Number);
        assert_eq!(value.as_number(), Some("42"));
    }

    #[test]
    fn string_scalar_projects_as_string() {
        let src = r#""hello""#;
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        assert_eq!(value.value_kind(), ValueKind::String);
        assert_eq!(value.as_str(), Some(r#""hello""#));
    }

    #[test]
    fn true_false_null_project_as_their_literals() {
        let src = "true";
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        assert_eq!(Value::new(&tree, &input).value_kind(), ValueKind::True);
        let src = "false";
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        assert_eq!(Value::new(&tree, &input).value_kind(), ValueKind::False);
        let src = "null";
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        assert_eq!(Value::new(&tree, &input).value_kind(), ValueKind::Null);
    }

    #[test]
    fn nested_object_pairs_project_in_document_order() {
        let src = r#"{"a": {"b": 2}, "c": 3}"#;
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        let object = value.as_object().expect("is object");
        let pairs = object.pairs();
        assert_eq!(pairs.len(), 2);
        let (_, first_value) = &pairs[0];
        assert_eq!(first_value.value_kind(), ValueKind::Object);
        let inner = first_value.as_object().expect("inner is object");
        assert_eq!(inner.pairs().len(), 1);
    }

    #[test]
    fn array_items_carry_their_kinds() {
        let src = r#"[1, "two", true, null]"#;
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        let array = value.as_array().expect("is array");
        let items = array.items();
        assert_eq!(items.len(), 4);
        assert_eq!(items[0].value_kind(), ValueKind::Number);
        assert_eq!(items[1].value_kind(), ValueKind::String);
        assert_eq!(items[2].value_kind(), ValueKind::True);
        assert_eq!(items[3].value_kind(), ValueKind::Null);
    }

    #[test]
    fn tree_view_trait_is_implemented_for_value() {
        let src = "42";
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        // The TreeView trait's kind() returns the raw NodeKind (the
        // scalar's token kind); the inherent value_kind() returns ValueKind.
        assert_eq!(
            <Value as runtime_core::view::TreeView>::kind(&value),
            kinds::NUMBER
        );
        assert_eq!(value.span(), 0..2);
        assert!(!value.has_error());
    }

    #[test]
    fn value_span_recovers_absolute_range_for_descendant() {
        let src = r"[1, 2, 3]";
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        let array = value.as_array().expect("is array");
        let items = array.items();
        // The second element "2" starts at byte offset 4 ("[1, " = 4 bytes).
        assert_eq!(items[1].span(), 4..5);
    }

    #[test]
    fn array_of_objects_items_project_as_objects() {
        let src = r#"[{"x": 1}, {"y": 2}]"#;
        let tree = parse_tree(src);
        let input = Source::from_str_unchecked(src);
        let value = Value::new(&tree, &input);
        let array = value.as_array().expect("is array");
        let items = array.items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].value_kind(), ValueKind::Object);
        assert_eq!(items[1].value_kind(), ValueKind::Object);
    }
}
