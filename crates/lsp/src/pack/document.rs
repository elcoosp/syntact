//! The pack document model (issue 042): one pack `parse` per version, a
//! single outline DFS, and every tree view prebuilt once per version —
//! edit-time compute, query-time reads. Queries answer from the cached
//! views without touching the tree again.
//!
// Document text is bounded by the input layer's u32 length guard
// (runtime-core `input::Source`), so usize to u32 casts of outline
// node indices cannot truncate for any document that parsed at all.

use std::sync::Arc;

use runtime_core::{MISSING_KIND, tree::GreenNode};
use serde::Serialize;

use crate::index::{LineIndex, Position, Range};

use super::outline::Outline;

/// LSP `SymbolKind` for structural nodes (the generic outline has no
/// per-pack kind tables — every structural node is an Object).
pub const SYMBOL_OBJECT: u32 = 19;

/// LSP `DocumentHighlightKind::Text`.
pub const HIGHLIGHT_TEXT: u32 = 1;

/// The semantic-token legend (fixed, pack-agnostic shape classes).
pub const SEMANTIC_TOKEN_TYPES: [&str; 4] = ["string", "number", "operator", "identifier"];

/// Legend index of each class (kept next to the legend for tests).
mod token_class {
    /// String literals (open on a quote).
    pub const STRING: u32 = 0;
    /// Numeric literals (optional sign, then a digit).
    pub const NUMBER: u32 = 1;
    /// Runs of ASCII punctuation.
    pub const OPERATOR: u32 = 2;
    /// Everything else significant (identifiers, keywords).
    pub const IDENTIFIER: u32 = 3;
}

/// A hierarchical document symbol (LSP `DocumentSymbol` shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackSymbol {
    /// The first significant token text of the node (capped).
    pub name: String,
    /// LSP `SymbolKind`.
    pub kind: u32,
    /// The node's full span.
    pub range: Range,
    /// The span the symbol should be selected on.
    pub selection_range: Range,
    /// Nested symbols.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<PackSymbol>,
}

/// A folding range (LSP `FoldingRange` shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoldingRange {
    /// First foldable line.
    pub start_line: u32,
    /// Last foldable line.
    pub end_line: u32,
}

/// A token occurrence (LSP `DocumentHighlight` shape).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentHighlight {
    /// The occurrence's span.
    pub range: Range,
    /// LSP `DocumentHighlightKind::Text`.
    pub kind: u32,
}

/// One pack document version: text, tree, position index, and the
/// prebuilt views.
#[derive(Debug, Clone)]
pub struct Document {
    /// The document uri this version belongs to (echoed in queries).
    pub uri: String,
    /// The pack name the document was resolved to.
    pub pack: &'static str,
    /// The document text (the tree renders back to this exactly).
    pub text: String,
    /// The green tree of the current version.
    pub tree: Arc<GreenNode>,
    /// The byte ↔ UTF-16 index (rebuilt per version).
    pub line_index: LineIndex,
    /// The positioned node index (rebuilt per version).
    pub outline: Outline,
    /// Error regions as diagnostics (rebuilt per version).
    pub diagnostics: Vec<crate::analysis::Diag>,
    /// The hierarchical outline (rebuilt per version).
    pub symbols: Vec<PackSymbol>,
    /// Multi-line node spans (rebuilt per version).
    pub folding: Vec<FoldingRange>,
    /// Delta-encoded semantic-token data (rebuilt per version).
    pub semantic_data: Vec<u32>,
}

use super::outline::{
    SLOT_COUNT, SLOT_END, SLOT_FOLD_LAST, SLOT_FS_END, SLOT_FS_START, SLOT_START, TOKEN_SLOT_COUNT,
};

impl Document {
    /// Parse `text` with `pack` and build every view. A structural
    /// parse failure (never expected from the shipped packs) keeps an
    /// empty tree and turns the message into a document diagnostic.
    #[must_use]
    pub fn open(
        uri: &str,
        pack: &'static str,
        parse: crate::pack::registry::ParseFn,
        text: &str,
    ) -> Self {
        let (tree, parse_error) = match parse(text) {
            Ok(tree) => (tree, None),
            Err(message) => (
                Arc::new(GreenNode {
                    kind: 0,
                    text_len: 0,
                    has_error: true,
                    children: Vec::new(),
                }),
                Some(message),
            ),
        };
        let mut document = Self {
            uri: uri.to_owned(),
            pack,
            text: text.to_owned(),
            tree: Arc::clone(&tree),
            line_index: LineIndex::new(text),
            outline: Outline::build(&tree, text),
            diagnostics: Vec::new(),
            symbols: Vec::new(),
            folding: Vec::new(),
            semantic_data: Vec::new(),
        };
        document.build_views();
        if let Some(message) = parse_error {
            document.diagnostics.push(crate::analysis::Diag {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 0,
                    },
                },
                severity: crate::analysis::DiagSeverity::Error,
                message,
            });
        }
        document
    }

    /// Apply a batch of edits (`span = None` is a full replacement) and
    /// rebuild every view. Each span was resolved against the text
    /// state it applies to (the server converts before calling).
    pub fn edit(
        &mut self,
        parse: crate::pack::registry::ParseFn,
        edits: &[(Option<std::ops::Range<usize>>, &str)],
    ) {
        for (span, replacement) in edits {
            if let Some(span) = span {
                let start = span.start.min(self.text.len());
                let end = span.end.min(self.text.len()).max(start);
                if !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
                    continue;
                }
                self.text.replace_range(start..end, replacement);
            } else {
                self.text.clear();
                self.text.push_str(replacement);
            }
            // Positions of the next edit in the batch resolve against
            // this state, so the index tracks every application.
            self.line_index = LineIndex::new(&self.text);
        }
        self.reparse(parse);
    }

    /// Reparse the current text and rebuild every view.
    fn reparse(&mut self, parse: crate::pack::registry::ParseFn) {
        let (tree, parse_error) = match parse(&self.text) {
            Ok(tree) => (tree, None),
            Err(message) => (
                Arc::new(GreenNode {
                    kind: 0,
                    text_len: 0,
                    has_error: true,
                    children: Vec::new(),
                }),
                Some(message),
            ),
        };
        self.tree = tree;
        self.outline = Outline::build(&self.tree, &self.text);
        self.build_views();
        if let Some(message) = parse_error {
            self.diagnostics.push(crate::analysis::Diag {
                range: Range {
                    start: Position {
                        line: 0,
                        character: 0,
                    },
                    end: Position {
                        line: 0,
                        character: 0,
                    },
                },
                severity: crate::analysis::DiagSeverity::Error,
                message,
            });
        }
    }

    /// Build every view from the current outline (pub for the bench
    /// phase-attribution dev tool).
    #[doc(hidden)]
    pub fn build_views(&mut self) {
        // One monotonic sweep converts every node span to UTF-16
        // positions; the view builders read the table instead of
        // re-walking the text per node.
        let spans = self.span_positions();
        self.build_diagnostics();
        self.build_symbols(&spans);
        self.build_folding(&spans);
        self.build_semantic_tokens();
    }

    /// Positions for every outline node in one pass:
    /// `[start, end, first-significant start/end, fold-last-byte]` per
    /// node index. The DFS event stream (entries + exits) and the
    /// token stream are both monotonic in bytes by construction, so
    /// two monotonic sweeps — no sort — produce the whole table; the
    /// first-significant positions come from the token sweep by exact
    /// span lookup (every first-significant span is a token).
    pub(crate) fn span_positions(&self) -> Vec<[Position; SLOT_COUNT]> {
        let nodes = &self.outline.nodes;
        let mut table = vec![
            Position {
                line: 0,
                character: 0
            };
            nodes.len() * SLOT_COUNT
        ];
        let mut sweep = MonotonicSweep::new(&self.text, &self.line_index);
        for &(byte, slot, node) in &self.outline.events {
            let position = sweep.position_at(byte as usize);
            table[node as usize * SLOT_COUNT + usize::from(slot)] = position;
        }
        // Token positions in their own sequential sweep (two passes
        // over two arrays beat one interleaved pass — measured).
        let mut token_positions = vec![
            Position {
                line: 0,
                character: 0
            };
            self.outline.tokens.len() * TOKEN_SLOT_COUNT
        ];
        let mut token_sweep = MonotonicSweep::new(&self.text, &self.line_index);
        for (token_index, (_, span)) in self.outline.tokens.iter().enumerate() {
            let start = token_sweep.position_at(span.start as usize);
            let end = token_sweep.position_at(span.end as usize);
            let base = token_index * TOKEN_SLOT_COUNT;
            token_positions[base] = start;
            token_positions[base + 1] = end;
        }
        // First-significant positions: resolve each distinct span once
        // against the token sweep. Ancestors inherit their descendant's
        // token, so a reverse preorder walk with a one-entry memo turns
        // every inheritance into a cache hit.
        let mut memo: Option<(std::ops::Range<u32>, Position, Position)> = None;
        for index in (0..nodes.len()).rev() {
            let Some(span) = &nodes[index].first_significant else {
                continue;
            };
            let (start, end) = if memo.as_ref().is_some_and(|(seen, _, _)| seen == span) {
                let (_, start, end) = memo.as_ref().expect("checked");
                (*start, *end)
            } else {
                let lookup = self.outline.tokens.binary_search_by(|(_, candidate)| {
                    candidate
                        .start
                        .cmp(&span.start)
                        .then(candidate.end.cmp(&span.end))
                });
                let Some(token_index) = lookup.ok() else {
                    continue;
                };
                // Token slots store (start, end) pairwise.
                let base = token_index * TOKEN_SLOT_COUNT;
                let (start, end) = (token_positions[base], token_positions[base + 1]);
                memo = Some((span.clone(), start, end));
                (start, end)
            };
            table[index * SLOT_COUNT + usize::from(SLOT_FS_START)] = start;
            table[index * SLOT_COUNT + usize::from(SLOT_FS_END)] = end;
        }
        let mut out = Vec::with_capacity(nodes.len());
        for chunk in table.chunks_exact(SLOT_COUNT) {
            out.push([chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]]);
        }
        out
    }

    fn build_diagnostics(&mut self) {
        self.diagnostics = self
            .outline
            .errors
            .iter()
            .map(|(kind, span)| crate::analysis::Diag {
                range: self.line_index.range(
                    &self.text,
                    gir_syntax::ByteSpan {
                        start: span.start as usize,
                        end: span.end as usize,
                    },
                ),
                severity: crate::analysis::DiagSeverity::Error,
                message: if *kind == MISSING_KIND {
                    "missing token".to_owned()
                } else {
                    "syntax error".to_owned()
                },
            })
            .collect();
    }

    fn build_symbols(&mut self, spans: &[[Position; SLOT_COUNT]]) {
        // The root is the document itself; its children are the
        // top-level symbols. Insignificant nodes are spliced out (their
        // children are promoted) so no blank rows reach the editor.
        self.symbols = self.symbol_children(spans, 0);
    }

    fn symbol_children(&self, spans: &[[Position; SLOT_COUNT]], index: u32) -> Vec<PackSymbol> {
        let node = &self.outline.nodes[index as usize];
        // Direct children chain through subtree_end (grandchild blocks
        // sit between siblings in the preorder layout).
        let block_end = node.subtree_end as usize;
        let mut symbols = Vec::new();
        let mut cursor = index as usize + 1;
        while cursor < block_end {
            let child = cursor;
            let span = self.outline.nodes[child].first_significant.clone();
            // No nameable token: the children splice upwards.
            let significant_span = span.unwrap_or(0..0);
            cursor = self.outline.nodes[child].subtree_end as usize;
            let Some(significant) =
                (significant_span.start < significant_span.end).then_some(significant_span)
            else {
                symbols.extend(self.symbol_children(spans, child as u32));
                continue;
            };
            let name = self
                .text
                .get(significant.start as usize..significant.end as usize)
                .unwrap_or("");
            let slots = &spans[child];
            symbols.push(PackSymbol {
                name: symbol_name(name),
                kind: SYMBOL_OBJECT,
                range: Range {
                    start: slots[usize::from(SLOT_START)],
                    end: slots[usize::from(SLOT_END)],
                },
                selection_range: Range {
                    start: slots[usize::from(SLOT_FS_START)],
                    end: slots[usize::from(SLOT_FS_END)],
                },
                children: self.symbol_children(spans, child as u32),
            });
        }
        symbols
    }

    fn build_folding(&mut self, spans: &[[Position; SLOT_COUNT]]) {
        self.folding = self
            .outline
            .nodes
            .iter()
            .enumerate()
            .filter_map(|(index, _node)| {
                // The fold spans from the node's first line to the line
                // of its last byte (a node ending at a line boundary
                // folds to the line before; one ending mid-line folds
                // onto its line).
                let start = spans[index][usize::from(SLOT_START)];
                let end = spans[index][usize::from(SLOT_FOLD_LAST)];
                (start.line < end.line).then_some(FoldingRange {
                    start_line: start.line,
                    end_line: end.line,
                })
            })
            .collect();
    }

    fn build_semantic_tokens(&mut self) {
        self.semantic_data =
            semantic_token_data(&self.text, &self.line_index, &self.outline.tokens);
    }

    /// Every token matching the token at (or ending at) `offset` by
    /// kind and text. Whitespace positions match nothing.
    #[must_use]
    pub fn highlight(&self, offset: usize) -> Vec<DocumentHighlight> {
        let tokens = &self.outline.tokens;
        let position = tokens.partition_point(|(_, span)| span.end as usize <= offset);
        let Some((kind, span)) = tokens.get(position) else {
            return Vec::new();
        };
        let active = (span.start as usize..span.end as usize).contains(&offset)
            || (span.end as usize == offset && span.start < span.end);
        // Whitespace/trivia tokens (and positions inside them) match
        // nothing — editors highlight occurrences of real tokens.
        let significant = self
            .text
            .get(span.start as usize..span.end as usize)
            .is_some_and(|text| !text.bytes().all(|byte| byte.is_ascii_whitespace()));
        if !active || !significant {
            return Vec::new();
        }
        let Some(needle) = self
            .text
            .get(span.start as usize..span.end as usize)
            .map(str::to_owned)
        else {
            return Vec::new();
        };
        let mut highlights = Vec::new();
        for (candidate_kind, candidate_span) in tokens {
            if *candidate_kind != *kind {
                continue;
            }
            let matches = self
                .text
                .get(candidate_span.start as usize..candidate_span.end as usize)
                .is_some_and(|text| text == needle);
            if matches {
                highlights.push(DocumentHighlight {
                    range: self.line_index.range(
                        &self.text,
                        gir_syntax::ByteSpan {
                            start: candidate_span.start as usize,
                            end: candidate_span.end as usize,
                        },
                    ),
                    kind: HIGHLIGHT_TEXT,
                });
            }
        }
        highlights
    }

    /// The ancestor chain of node spans at `offset`, innermost first,
    /// as LSP ranges (a position past the last node clamps to the root
    /// chain — editors query at the document end).
    #[must_use]
    pub fn selection_chain(&self, offset: usize) -> Vec<Range> {
        let index = self
            .outline
            .innermost(offset)
            .unwrap_or_else(|| self.outline.root());
        self.outline
            .chain(index)
            .into_iter()
            .map(|node| {
                let span = &self.outline.nodes[node as usize];
                self.line_index.range(
                    &self.text,
                    gir_syntax::ByteSpan {
                        start: span.start as usize,
                        end: span.end as usize,
                    },
                )
            })
            .collect()
    }
}

/// A monotonic byte-to-UTF-16 position cursor: queries must arrive in
/// non-decreasing byte order (the outline event and token streams both
/// do), so the whole document is walked once — never a per-query char
/// walk from the line start. A rare backstep (a parent's fold-last
/// byte after a child's end shares the boundary) resets the cursor to
/// the target line and re-walks within it.
struct MonotonicSweep<'a> {
    text: &'a str,
    line_index: &'a LineIndex,
    line: usize,
    byte: usize,
    units: u32,
}

impl MonotonicSweep<'_> {
    fn new<'a>(text: &'a str, line_index: &'a LineIndex) -> MonotonicSweep<'a> {
        MonotonicSweep {
            text,
            line_index,
            line: 0,
            byte: 0,
            units: 0,
        }
    }

    /// The position of `byte` (a char boundary, clamped to the end).
    fn position_at(&mut self, byte: usize) -> Position {
        let start = byte.min(self.text.len());
        if start < self.byte {
            // Backstep: reset to the target line and re-walk within it.
            self.line = self.line_index.line_of(start);
            self.byte = self.line_index.line_start(self.line).unwrap_or(0);
            self.units = 0;
        }
        while self.line + 1 < self.line_index.len() {
            let next = self
                .line_index
                .line_start(self.line + 1)
                .unwrap_or(usize::MAX);
            if start < next {
                break;
            }
            self.line += 1;
            self.byte = next;
            self.units = 0;
        }
        if start >= self.byte {
            for character in self.text[self.byte..start].chars() {
                self.units += u32::try_from(character.len_utf16()).unwrap_or(0);
            }
            self.byte = start;
        }
        Position {
            line: u32::try_from(self.line).unwrap_or(u32::MAX),
            character: self.units,
        }
    }
}

/// A symbol name: the first significant token, trimmed to its first
/// line and capped (real outlines name nodes after their leading
/// token — a key, a heading marker, a section bracket).
#[must_use]
pub fn symbol_name(raw: &str) -> String {
    let line = raw.split('\n').next().unwrap_or("").trim_end();
    // Fast path: short single-line names are copied as-is.
    if line.len() <= 23 {
        return line.to_owned();
    }
    let mut name: String = line.chars().take(23).collect();
    if line.chars().count() > 23 {
        name.push('…');
    }
    name
}

/// Shape classification into the semantic-token legend.
/// Whitespace-only and prose runs (tokens containing whitespace) stay
/// unhighlighted; everything significant falls in a class.
#[must_use]
pub fn classify_token(text: &str) -> Option<u32> {
    use token_class::{IDENTIFIER, NUMBER, OPERATOR, STRING};
    let bytes = text.as_bytes();
    let first = *bytes.first()?;
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return None;
    }
    if matches!(first, b'"' | b'\'' | b'`') {
        return Some(STRING);
    }
    if bytes.iter().any(u8::is_ascii_whitespace) {
        return None;
    }
    let unsigned = if first == b'-' || first == b'+' {
        &bytes[1..]
    } else {
        bytes
    };
    if unsigned.first().is_some_and(u8::is_ascii_digit) {
        return Some(NUMBER);
    }
    if bytes
        .iter()
        .all(|byte| byte.is_ascii_punctuation() && *byte != b'_')
    {
        return Some(OPERATOR);
    }
    Some(IDENTIFIER)
}

/// Delta-encoded LSP semantic-token data over the document-ordered
/// token list. One monotonic line/character cursor — amortized
/// O(document), never a binary search per token.
#[must_use]
pub fn semantic_token_data(
    text: &str,
    line_index: &LineIndex,
    tokens: &[(u16, std::ops::Range<u32>)],
) -> Vec<u32> {
    let mut data = Vec::with_capacity(tokens.len() * 5);
    let mut previous_line = 0_u32;
    let mut previous_character = 0_u32;
    let mut cursor_line = 0_usize;
    let mut cursor_byte = 0_usize;
    let mut cursor_units = 0_u32;
    for (_, span) in tokens {
        let start = (span.start as usize).min(text.len());
        let end = (span.end as usize).min(text.len()).max(start);
        let Some(slice) = text.get(start..end) else {
            continue;
        };
        // Advance the line cursor (line starts are sorted).
        let mut line = cursor_line;
        let mut line_start = cursor_byte;
        while line + 1 < line_index.len() {
            let next = line_index.line_start(line + 1).unwrap_or(usize::MAX);
            if start < next {
                break;
            }
            line += 1;
            line_start = next;
        }
        if line != cursor_line {
            cursor_line = line;
            cursor_byte = line_start;
            cursor_units = 0;
        }
        // Advance the character cursor within the line.
        if start >= cursor_byte {
            for character in text[cursor_byte..start].chars() {
                cursor_units += u32::try_from(character.len_utf16()).unwrap_or(0);
            }
            cursor_byte = start;
        }
        let line_number = u32::try_from(line).unwrap_or(u32::MAX);
        let character = cursor_units;
        let Some(class) = classify_token(slice) else {
            continue;
        };
        let delta_line = line_number - previous_line;
        let delta_character = if delta_line == 0 {
            character - previous_character
        } else {
            character
        };
        data.extend([
            delta_line,
            delta_character,
            u32::try_from(end - start).unwrap_or(0),
            class,
            0,
        ]);
        previous_line = line_number;
        previous_character = character;
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::registry::PackEntry;
    use crate::pack::registry::PackRegistry;

    fn json_document(source: &str) -> Document {
        let registry = PackRegistry::reference();
        let entry = registry.get("json").expect("json pack");
        Document::open("file:///t.json", "json", entry.parse, source)
    }

    fn offset_of(source: &str, needle: &str) -> usize {
        source.find(needle).expect("needle")
    }

    // ---------------------------------------------------------------
    // Views over a valid document
    // ---------------------------------------------------------------

    #[test]
    fn valid_document_has_no_diagnostics() {
        let document = json_document(r#"{"a": [1, true]}"#);
        assert_eq!(document.diagnostics, []);
        assert!(!document.tree.has_error);
    }

    #[test]
    fn error_document_produces_region_diagnostics() {
        let source = "{\"a\": }";
        let document = json_document(source);
        assert_eq!(document.diagnostics.len(), 2);
        for diagnostic in &document.diagnostics {
            assert_eq!(diagnostic.severity, crate::analysis::DiagSeverity::Error);
            assert_eq!(diagnostic.message, "syntax error");
        }
        // The first diagnostic covers the stray closer (its region
        // starts after the colon, spanning the leading gap).
        let first = &document.diagnostics[0];
        let offset = document
            .line_index
            .offset(&document.text, first.range.start)
            .expect("in range");
        assert_eq!(offset, source.find(": ").expect("colon") + 1);
    }

    #[test]
    fn symbols_outline_names_nodes_by_first_token() {
        let source = "{\n  \"alpha\": 1,\n  \"beta\": 2\n}";
        let document = json_document(source);
        // Root excluded; the object is the top-level symbol, its pairs
        // named after the keys.
        assert_eq!(document.symbols.len(), 1);
        let object = &document.symbols[0];
        assert_eq!(object.name, "{");
        assert_eq!(object.kind, SYMBOL_OBJECT);
        assert_eq!(object.children.len(), 2);
        assert_eq!(object.children[0].name, "\"alpha\"");
        assert_eq!(object.children[1].name, "\"beta\"");
        // selectionRange sits on the key, range covers the whole pair.
        let pair = &object.children[0];
        let selection_start = document
            .line_index
            .offset(&document.text, pair.selection_range.start)
            .expect("in range");
        assert_eq!(selection_start, offset_of(source, "\"alpha\""));
        assert!(pair.range.start < pair.selection_range.start);
    }

    #[test]
    fn insignificant_nodes_are_spliced_not_shown() {
        // The json pack's ROOT is skipped by the server (document
        // itself); a node without any significant token is spliced.
        let document = json_document("   \n  ");
        assert!(
            document
                .symbols
                .iter()
                .all(|symbol| !symbol.name.is_empty())
        );
    }

    #[test]
    fn folding_covers_multiline_nodes_only() {
        let document = json_document("{\n  \"alpha\": [1, 2, 3]\n}");
        assert_ne!(document.folding, []);
        // The outermost fold spans the whole object minus the last line.
        assert_eq!(document.folding[0].start_line, 0);
        assert_eq!(document.folding[0].end_line, 2);
        for range in &document.folding {
            assert!(range.start_line < range.end_line);
        }
        // Single-line documents fold nothing.
        assert_eq!(json_document(r#"{"a": 1}"#).folding, []);
    }

    #[test]
    fn semantic_tokens_classify_shapes_and_encode_deltas() {
        let source = r#"{"alpha": 12, "beta": true}"#;
        let document = json_document(source);
        let data = &document.semantic_data;
        assert_eq!(data.len() % 5, 0);
        // Walk the absolute positions from the deltas.
        let mut line = 0_u32;
        let mut character = 0_u32;
        let mut classes = Vec::new();
        for chunk in data.chunks_exact(5) {
            let (delta_line, delta_character, length, class, modifiers) =
                (chunk[0], chunk[1], chunk[2], chunk[3], chunk[4]);
            if delta_line == 0 {
                character += delta_character;
            } else {
                line += delta_line;
                character = delta_character;
            }
            assert_eq!(modifiers, 0);
            classes.push((line, character, length, class));
        }
        // `}` operator, `"alpha"` string, `:` operator, `12` number,
        // `,` operator, … `true` identifier before the final `}`.
        assert_eq!(classes[0].3, 2); // {
        assert_eq!(classes[1].3, 0); // "alpha"
        assert_eq!(classes[2].3, 2); // :
        assert_eq!(classes[3].3, 1); // 12
        assert_eq!(classes[4].3, 2); // ,
        let last = classes.last().expect("}");
        assert_eq!(last.3, 2); // } → operator class
        let before = &classes[classes.len() - 2];
        assert_eq!(before.3, 3); // true → identifier class
        // Absolute positions match the line index.
        let start = document
            .line_index
            .offset(
                &document.text,
                Position {
                    line: before.0,
                    character: before.1,
                },
            )
            .expect("in range");
        assert_eq!(&source[start..start + before.2 as usize], "true");
    }

    #[test]
    fn classify_token_rules() {
        assert_eq!(classify_token("\"str\""), Some(0));
        assert_eq!(classify_token("'c'"), Some(0));
        assert_eq!(classify_token("12.5e+3"), Some(1));
        assert_eq!(classify_token("-1"), Some(1));
        assert_eq!(classify_token("0x1f"), Some(1));
        assert_eq!(classify_token("{"), Some(2));
        assert_eq!(classify_token("->"), Some(2));
        assert_eq!(classify_token("=="), Some(2));
        assert_eq!(classify_token("true"), Some(3));
        assert_eq!(classify_token("snake_case"), Some(3));
        assert_eq!(classify_token("   "), None);
        assert_eq!(classify_token(""), None);
        assert_eq!(classify_token("a b"), None);
    }

    #[test]
    fn symbol_names_cap_and_stay_single_line() {
        assert_eq!(
            symbol_name("\"a very long string literal\""),
            "\"a very long string lit…".to_owned()
        );
        assert_eq!(symbol_name("short"), "short");
        assert_eq!(symbol_name("multi\nline"), "multi");
    }

    // ---------------------------------------------------------------
    // Queries
    // ---------------------------------------------------------------

    #[test]
    fn highlight_matches_kind_and_text() {
        let source = r#"{"a": 1, "a": 2}"#;
        let document = json_document(source);
        let offset = offset_of(source, "\"a\": 1");
        let highlights = document.highlight(offset);
        // Both key strings match; `1`/`2` do not.
        assert_eq!(highlights.len(), 2);
        assert_eq!(highlights[0].kind, HIGHLIGHT_TEXT);
        let first_start = document
            .line_index
            .offset(&document.text, highlights[0].range.start)
            .expect("in range");
        assert_eq!(first_start, offset_of(source, "\"a\": 1"));
        // A number highlights only itself.
        let offset = offset_of(source, "1,");
        let highlights = document.highlight(offset);
        assert_eq!(highlights.len(), 1);
        // Whitespace matches nothing (the byte after the first colon).
        let space = source.find(": ").expect("colon") + 1;
        assert_eq!(document.highlight(space), []);
        // Past the end matches nothing.
        assert_eq!(document.highlight(usize::MAX), []);
    }

    #[test]
    fn highlight_resolves_at_token_end() {
        let source = "[1, 2]";
        let document = json_document(source);
        let offset = source.find('1').expect("one") + 1;
        let highlights = document.highlight(offset);
        assert_eq!(highlights.len(), 1);
    }

    #[test]
    fn selection_chain_walks_ancestors() {
        let source = "{\n  \"alpha\": [1]\n}";
        let document = json_document(source);
        let offset = offset_of(source, "[1]");
        let chain = document.selection_chain(offset);
        assert!(chain.len() >= 3, "array, pair, object, root");
        // Each range strictly contains the next innermost's start.
        let starts: Vec<usize> = chain
            .iter()
            .map(|range| {
                document
                    .line_index
                    .offset(&document.text, range.start)
                    .expect("in range")
            })
            .collect();
        // Innermost first: starts move outwards (non-increasing).
        assert!(starts.windows(2).all(|pair| pair[0] >= pair[1]));
        // Document end clamps to the root chain.
        let chain = document.selection_chain(source.len());
        assert_eq!(chain.len(), 1);
    }

    // ---------------------------------------------------------------
    // Editing
    // ---------------------------------------------------------------

    #[test]
    fn edit_reparses_and_rebuilds_views() {
        let mut document = json_document("{\"a\": 1}");
        assert_eq!(document.diagnostics, []);
        let span = offset_of(&document.text, "1}");
        document.edit(
            PackRegistry::reference().get("json").expect("json").parse,
            &[(Some(span..span + 1), "")],
        );
        assert_eq!(document.text, "{\"a\": }");
        assert_eq!(document.diagnostics.len(), 2);
        assert_eq!(document.pack, "json");
    }

    #[test]
    fn full_replacement_replaces_the_text() {
        let mut document = json_document("{\"a\": 1}");
        document.edit(
            PackRegistry::reference().get("json").expect("json").parse,
            &[(None, "[1, 2")],
        );
        assert_eq!(document.text, "[1, 2");
        assert_eq!(document.diagnostics.len(), 1);
    }

    #[test]
    fn sequential_edits_resolve_against_their_own_state() {
        let mut document = json_document("[1, 2, 3]");
        let registry = PackRegistry::reference();
        let parse = registry.get("json").expect("json").parse;
        // Insert at 0 against the original (`0, [1, 2, 3]`), then a
        // span resolved against the post-insert text (byte 7 = `2`).
        let first = 0..0;
        let second = 7..8;
        document.edit(parse, &[(Some(first), "0, "), (Some(second), "")]);
        assert_eq!(document.text, "0, [1, , 3]");
    }

    #[test]
    fn char_boundary_violations_are_skipped() {
        let source = "αβ";
        let mut document = json_document(source);
        let registry = PackRegistry::reference();
        let parse = registry.get("json").expect("json").parse;
        // Offset 1 is mid-char for "α" — the edit is skipped.
        document.edit(parse, &[(Some(1..2), "x")]);
        assert_eq!(document.text, "αβ");
    }

    /// The fold-last byte rule: the node's end, floored to a char
    /// boundary (mirrors the outline's event emission).
    fn fold_last_byte(text: &str, node: &crate::pack::outline::OutlineNode) -> usize {
        let start = node.start as usize;
        let mut last = (node.end.saturating_sub(1) as usize).max(start);
        while last > start && !text.is_char_boundary(last) {
            last -= 1;
        }
        last
    }

    #[test]
    fn span_table_matches_the_line_index() {
        // Every slot of the span table must agree with
        // `LineIndex::position` at its byte offset.
        let source = "{\n  \"cl\u{e9}\": \"v \u{1f600}\"\n}\n[1, 2, 3]";
        let document = json_document(source);
        let table = document.span_positions();
        assert_eq!(table.len(), document.outline.nodes.len());
        for (index, node) in document.outline.nodes.iter().enumerate() {
            let slots = table[index];
            let expectations = [
                (usize::from(SLOT_START), node.start as usize),
                (usize::from(SLOT_END), node.end as usize),
                (
                    usize::from(SLOT_FOLD_LAST),
                    fold_last_byte(&document.text, node),
                ),
            ];
            for (slot, byte) in expectations {
                assert!(
                    source.is_char_boundary(byte),
                    "slot {slot} byte {byte} not a boundary"
                );
                assert_eq!(
                    slots[slot],
                    document.line_index.position(&document.text, byte),
                    "slot {slot} drift at byte {byte}"
                );
            }
        }
        // First-significant slots: every node with a nameable token
        // resolves to that token's span.
        for (index, node) in document.outline.nodes.iter().enumerate() {
            let Some(span) = &node.first_significant else {
                continue;
            };
            assert_eq!(
                table[index][usize::from(SLOT_FS_START)],
                document
                    .line_index
                    .position(&document.text, span.start as usize)
            );
            assert_eq!(
                table[index][usize::from(SLOT_FS_END)],
                document
                    .line_index
                    .position(&document.text, span.end as usize)
            );
        }
    }

    #[test]
    fn pack_entry_accessors_work_for_every_pack() {
        let registry = PackRegistry::reference();
        let entries: Vec<PackEntry> = registry
            .names()
            .iter()
            .map(|name| *registry.get(name).expect("registered"))
            .collect();
        assert_eq!(entries.len(), 10);
        for entry in &entries {
            let document = Document::open("mem", entry.name, entry.parse, "x");
            assert_eq!(document.pack, entry.name);
        }
    }
}
