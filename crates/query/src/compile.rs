//! Compilation (issue 031): the parsed [`QuerySpec`] flattens into
//! [`QueryStep`] arrays whose kind-prefilter bitsets reject candidates in
//! one AND + test. Kind names resolve through the pack's kind table
//! (injected); unknown kinds are compile errors carrying the node span.

use crate::parse::{PatternDef, Quant, QuerySpec};

/// The prefilter word count (192 kinds — three u64 words).
pub const PREFILTER_WORDS: usize = 3;

/// Bits per prefilter word (64 kinds).
const PREFILTER_WORDS_PER_WORD: usize = 64;

/// The wildcard sentinel kind (matches any node kind).
pub const WILDCARD: u16 = u16::MAX;

/// One flat match step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryStep {
    /// The expected node kind (the wildcard sentinel matches any).
    pub kind: u16,
    /// The quantifier's lower bound.
    pub min: u8,
    /// The quantifier's upper bound (`u8::MAX` = unbounded).
    pub max: u8,
    /// The capture id (`None` = uncaptured).
    pub capture: Option<u16>,
    /// The kind-prefilter bitset: this step's own kind plus all its
    /// descendants' kinds — a candidate subtree whose kind bit is absent
    /// cannot match, rejected in O(1).
    pub prefilter: [u64; PREFILTER_WORDS],
}

/// One compiled pattern: flat steps (pre-order: the root first, then
/// each child's steps recursively).
#[derive(Debug, Clone)]
pub struct CompiledPattern {
    /// The flat steps.
    pub steps: Vec<QueryStep>,
    /// The predicate literal per `#eq?` (capture id, expected text).
    pub predicates: Vec<(u16, String)>,
}

/// The compiled query.
#[derive(Debug, Clone, Default)]
pub struct CompiledQuery {
    /// Patterns in source order.
    pub patterns: Vec<CompiledPattern>,
    /// Capture names by id (dense, first-seen order).
    pub capture_names: Vec<String>,
}

/// The kind resolver: a pack's kind-name → id mapping.
pub trait KindResolver {
    /// The id for `name`, or `None` when the grammar has no such kind.
    fn kind_id(&self, name: &str) -> Option<u16>;
}

/// A resolver over a slice of `(name, id)` pairs.
pub struct SliceResolver<'a> {
    /// The mapping.
    pub kinds: &'a [(&'a str, u16)],
}

impl KindResolver for SliceResolver<'_> {
    fn kind_id(&self, name: &str) -> Option<u16> {
        self.kinds
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, id)| *id)
    }
}

/// Compile the query against a kind resolver.
///
/// # Errors
/// A [`crate::parse::QueryError`]-shaped span error (re-exported as
/// `CompileError`) when a kind name is unknown.
pub fn compile(
    spec: &QuerySpec,
    resolver: &dyn KindResolver,
) -> Result<CompiledQuery, CompileError> {
    let mut out = CompiledQuery::default();
    for pattern in &spec.patterns {
        let mut steps = Vec::new();
        let mut predicates = Vec::new();
        compile_node(
            pattern,
            &mut steps,
            resolver,
            &mut out.capture_names,
            &mut predicates,
        )?;
        out.patterns.push(CompiledPattern { steps, predicates });
    }
    Ok(out)
}

/// A compilation error: unknown kind with its span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    /// The node's text span.
    pub span: std::ops::Range<u32>,
    /// The unknown kind name.
    pub kind: String,
}

impl CompileError {
    /// The error message (mirrors the Display text).
    #[must_use]
    pub fn message(&self) -> String {
        format!(
            "unknown node kind `{}` at byte {}",
            self.kind, self.span.start
        )
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown node kind `{}` at byte {}",
            self.kind, self.span.start
        )
    }
}

impl std::error::Error for CompileError {}

fn compile_node(
    node: &PatternDef,
    steps: &mut Vec<QueryStep>,
    resolver: &dyn KindResolver,
    captures: &mut Vec<String>,
    predicates: &mut Vec<(u16, String)>,
) -> Result<(), CompileError> {
    // compile_qnode recurses: the root step plus every descendant's
    // steps, pre-order.
    compile_qnode(&node.root, steps, resolver, captures)?;
    for pred in &node.predicates {
        let id = u16::try_from(
            captures
                .iter()
                .position(|name| *name == pred.capture)
                .ok_or(CompileError {
                    span: pred.span.clone(),
                    kind: format!("@{}", pred.capture),
                })?,
        )
        .expect("capture count fits u16");
        predicates.push((id, pred.value.clone()));
    }
    Ok(())
}

fn compile_qnode(
    node: &crate::parse::QNode,
    steps: &mut Vec<QueryStep>,
    resolver: &dyn KindResolver,
    captures: &mut Vec<String>,
) -> Result<(), CompileError> {
    let kind = match &node.kind {
        Some(name) => {
            let Some(id) = resolver.kind_id(name) else {
                return Err(CompileError {
                    span: node.span.clone(),
                    kind: name.clone(),
                });
            };
            id
        }
        None => WILDCARD,
    };
    let capture = node.capture.as_ref().map(|name| {
        if let Some(pos) = captures.iter().position(|n| n == name) {
            u16::try_from(pos).expect("capture count fits u16")
        } else {
            captures.push(name.clone());
            u16::try_from(captures.len() - 1).expect("capture count fits u16")
        }
    });
    let (min, max) = quant_bounds(node.quant);
    // Prefilter: this kind's bit plus every descendant's; the wildcard
    // matches anything, so its prefilter is all ones.
    let mut prefilter = if kind == WILDCARD {
        [u64::MAX; PREFILTER_WORDS]
    } else {
        [0_u64; PREFILTER_WORDS]
    };
    if kind != WILDCARD {
        set_bit(&mut prefilter, kind);
    }
    for child in &node.children {
        let child_steps = descendant_bits(child, resolver)?;
        for word in 0..PREFILTER_WORDS {
            prefilter[word] |= child_steps[word];
        }
    }
    steps.push(QueryStep {
        kind,
        min,
        max,
        capture,
        prefilter,
    });
    for child in &node.children {
        compile_qnode(child, steps, resolver, captures)?;
    }
    Ok(())
}

fn descendant_bits(
    node: &crate::parse::QNode,
    resolver: &dyn KindResolver,
) -> Result<[u64; PREFILTER_WORDS], CompileError> {
    let mut bits = if node.kind.is_none() {
        // The wildcard matches anything: all bits set.
        [u64::MAX; PREFILTER_WORDS]
    } else {
        [0_u64; PREFILTER_WORDS]
    };
    if let Some(name) = &node.kind {
        let Some(id) = resolver.kind_id(name) else {
            return Err(CompileError {
                span: node.span.clone(),
                kind: name.clone(),
            });
        };
        set_bit(&mut bits, id);
    } else {
        return Ok(bits);
    }
    for child in &node.children {
        let child_bits = descendant_bits(child, resolver)?;
        for word in 0..PREFILTER_WORDS {
            bits[word] |= child_bits[word];
        }
    }
    Ok(bits)
}

fn set_bit(bits: &mut [u64; PREFILTER_WORDS], kind: u16) {
    let word = usize::from(kind) / PREFILTER_WORDS_PER_WORD;
    if word < PREFILTER_WORDS {
        bits[word] |= 1_u64 << (kind % 64);
    }
}

/// The bitset test (the O(1) candidate rejection).
#[must_use]
pub fn bitset_allows(bits: &[u64; PREFILTER_WORDS], kind: u16) -> bool {
    let word = usize::from(kind) / PREFILTER_WORDS_PER_WORD;
    (word < PREFILTER_WORDS && bits[word] & (1_u64 << (kind % 64)) != 0) || kind == WILDCARD
}

fn quant_bounds(quant: Quant) -> (u8, u8) {
    match quant {
        Quant::One => (1, 1),
        Quant::ZeroOne => (0, 1),
        Quant::ZeroPlus => (0, u8::MAX),
        Quant::OnePlus => (1, u8::MAX),
    }
}
