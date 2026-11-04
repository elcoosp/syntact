//! Query text parsing + compilation (issue 030): `.scm`-shaped patterns
//! parsed with source spans, validated against a [`KindResolver`], and
//! compiled to flat [`QueryStep`] programs with kind-prefilter bitsets.

use std::ops::Range;

/// A child quantifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quant {
    /// Exactly one (the default).
    One,
    /// Zero or one (`?`).
    ZeroOrOne,
    /// Zero or more (`*`).
    ZeroOrMore,
    /// One or more (`+`).
    OneOrMore,
}

/// One child matcher inside a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    /// The child's node kind (resolved id).
    pub kind: u16,
    /// `true` for a `_` wildcard.
    pub wildcard: bool,
    /// Capture slot the child binds.
    pub capture: Option<u8>,
    /// The child's quantifier.
    pub quant: Quant,
    /// Source span of the child pattern (for diagnostics).
    pub span: Range<usize>,
}

/// A compiled pattern: a root kind + ordered child steps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryStep {
    /// The root node kind (`u16::MAX` for a wildcard root).
    pub kind: u16,
    /// `true` when the root is a `_` wildcard.
    pub wildcard: bool,
    /// The root's capture slot.
    pub capture: Option<u8>,
    /// The ordered child steps.
    pub children: Vec<ChildStep>,
    /// Source span of the whole pattern.
    pub span: Range<usize>,
}

/// A child step in the flat program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildStep {
    /// The child kind (`u16::MAX` for a wildcard).
    pub kind: u16,
    /// Capture slot the child binds.
    pub capture: Option<u8>,
    /// The child's quantifier.
    pub quant: Quant,
}

/// 256-kind bitset — the O(1) kind prefilter per pattern.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KindBitset(pub [u64; 4]);

impl KindBitset {
    /// Set one kind bit.
    pub fn set(&mut self, kind: u16) {
        let index = usize::from(kind >> 6);
        if index < 4 {
            self.0[index] |= 1_u64 << (kind & 63);
        }
    }

    /// Whether `kind` may start a match — O(1).
    #[must_use]
    pub fn contains(&self, kind: u16) -> bool {
        let index = usize::from(kind >> 6);
        index < 4 && self.0[index] & (1_u64 << (kind & 63)) != 0
    }
}

/// A compiled predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Predicate {
    /// `(#eq? @cap "text")`.
    Eq {
        /// The capture slot compared.
        capture: u8,
        /// The exact text the capture must equal.
        value: String,
    },
    /// `(#not-eq? @cap "text")`.
    NotEq {
        /// The capture slot compared.
        capture: u8,
        /// The exact text the capture must not equal.
        value: String,
    },
    /// Any other `(#name? args…)` — dispatched to the host closure.
    Custom {
        /// The predicate name (without the leading `#` or `?`).
        name: String,
        /// Capture slots and literal texts, in order.
        args: Vec<PredArgSpec>,
        /// Source span (for diagnostics).
        span: Range<usize>,
    },
}

/// A predicate argument in compiled form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredArgSpec {
    /// A capture slot.
    Capture(u8),
    /// A literal string.
    Text(String),
}

/// A fully compiled query: patterns + predicates + the prefilter.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompiledQuery {
    /// The flat pattern programs.
    pub patterns: Vec<QueryStep>,
    /// The compiled predicates (apply to every pattern's matches).
    pub predicates: Vec<Predicate>,
    /// Union of all patterns' root kinds — the cursor's first O(1) test.
    pub prefilter: KindBitset,
    /// Number of capture slots allocated.
    pub capture_count: u8,
}

/// Resolves kind names to runtime ids.
pub trait KindResolver {
    /// The id for `name`, or `None` when unknown (a validation error).
    fn resolve(&self, name: &str) -> Option<u16>;
}

/// A query-language error with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryError {
    /// Human-readable cause.
    pub message: String,
    /// Byte span in the query text.
    pub span: Range<usize>,
}

/// Parse query text into a [`CompiledQuery`], resolving kinds via
/// `resolver`.
///
/// # Errors
/// Syntax and validation errors carry the offending span.
pub fn parse(text: &str, resolver: &dyn KindResolver) -> Result<CompiledQuery, QueryError> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    let mut compiled = CompiledQuery::default();
    let mut captures: Vec<String> = Vec::new();
    loop {
        parser.skip_ws();
        if parser.pos >= parser.bytes.len() {
            break;
        }
        if parser.peek() == Some(b'(') && parser.at_pred() {
            let pred = parser.parse_predicate(&mut captures)?;
            compiled.predicates.push(pred);
        } else {
            let pattern = parser.parse_pattern(resolver, &mut captures)?;
            // Prefilter: the root kind (wildcards match everything).
            if pattern.wildcard {
                compiled.prefilter = all_kinds();
            } else {
                compiled.prefilter.set(pattern.kind);
            }
            compiled.patterns.push(pattern);
        }
    }
    if compiled.patterns.is_empty() {
        return Err(QueryError {
            message: "empty query".to_owned(),
            span: 0..text.len(),
        });
    }
    compiled.capture_count = u8::try_from(captures.len()).unwrap_or(u8::MAX);
    Ok(compiled)
}

/// The full-coverage prefilter for wildcard roots.
fn all_kinds() -> KindBitset {
    let mut bits = KindBitset::default();
    bits.0.fill(u64::MAX);
    bits
}

struct Parser<'t> {
    bytes: &'t [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn at_pred(&self) -> bool {
        // "(#" — the next non-space byte after `(` is `#`.
        let mut idx = self.pos + 1;
        while matches!(self.bytes.get(idx), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            idx += 1;
        }
        self.bytes.get(idx) == Some(&b'#')
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn parse_predicate(&mut self, captures: &mut Vec<String>) -> Result<Predicate, QueryError> {
        let start = self.pos;
        self.skip_ws();
        self.expect(b'(')?;
        self.skip_ws();
        self.expect(b'#')?;
        let name_start = self.pos;
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            self.pos += 1;
        }
        let mut name = String::from_utf8_lossy(&self.bytes[name_start..self.pos]).into_owned();
        if self.peek() == Some(b'?') {
            self.pos += 1;
            name.push('?');
        }
        let mut args: Vec<PredArgSpec> = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b'"') => {
                    let text = self.parse_string()?;
                    args.push(PredArgSpec::Text(text));
                }
                Some(b'@') => {
                    self.pos += 1;
                    let cap = self.capture_slot(captures)?;
                    args.push(PredArgSpec::Capture(cap));
                }
                _ => {
                    return Err(QueryError {
                        message: "expected a capture or string argument".to_owned(),
                        span: self.pos..self.pos + 1,
                    });
                }
            }
        }
        let span = start..self.pos;
        let stripped = name.trim_end_matches('?');
        match (stripped, args.as_slice()) {
            ("eq", [PredArgSpec::Capture(c), PredArgSpec::Text(v)]) => Ok(Predicate::Eq {
                capture: *c,
                value: v.clone(),
            }),
            ("not-eq", [PredArgSpec::Capture(c), PredArgSpec::Text(v)]) => Ok(Predicate::NotEq {
                capture: *c,
                value: v.clone(),
            }),
            _ => Ok(Predicate::Custom {
                name: stripped.to_owned(),
                args,
                span,
            }),
        }
    }

    fn parse_string(&mut self) -> Result<String, QueryError> {
        self.expect(b'"')?;
        let mut value = String::new();
        while let Some(b) = self.peek() {
            match b {
                b'"' => {
                    self.pos += 1;
                    return Ok(value);
                }
                b'\\' => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'n') => value.push('\n'),
                        Some(b't') => value.push('\t'),
                        Some(b) => value.push(char::from(b)),
                        None => {
                            return Err(QueryError {
                                message: "unterminated string".to_owned(),
                                span: self.pos..self.pos + 1,
                            });
                        }
                    }
                    self.pos += 1;
                }
                _ => {
                    value.push(char::from(b));
                    self.pos += 1;
                }
            }
        }
        Err(QueryError {
            message: "unterminated string".to_owned(),
            span: self.pos..self.pos,
        })
    }

    fn expect(&mut self, byte: u8) -> Result<(), QueryError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(QueryError {
                message: format!("expected `{}`", char::from(byte)),
                span: self.pos..self.pos + 1,
            })
        }
    }

    /// `(kind capture? child*)` or `_ capture?`.
    #[allow(clippy::too_many_lines)] // one pattern grammar
    fn parse_pattern(
        &mut self,
        resolver: &dyn KindResolver,
        captures: &mut Vec<String>,
    ) -> Result<QueryStep, QueryError> {
        let start = self.pos;
        self.skip_ws();
        self.expect(b'(')?;
        self.skip_ws();
        // Kind name.
        let name_start = self.pos;
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            self.pos += 1;
        }
        let name = String::from_utf8_lossy(&self.bytes[name_start..self.pos]).into_owned();
        if name.is_empty() {
            return Err(QueryError {
                message: "expected a node kind".to_owned(),
                span: self.pos..self.pos + 1,
            });
        }
        let (kind, wildcard) = if name == "_" {
            (u16::MAX, true)
        } else {
            let Some(kind) = resolver.resolve(&name) else {
                return Err(QueryError {
                    message: format!("unknown kind `{name}`"),
                    span: name_start..self.pos,
                });
            };
            (kind, false)
        };
        // Root capture.
        self.skip_ws();
        let mut capture = None;
        if self.peek() == Some(b'@') {
            self.pos += 1;
            capture = Some(self.capture_slot(captures)?);
        }
        // Children.
        let mut children: Vec<ChildStep> = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    return Err(QueryError {
                        message: "unterminated pattern".to_owned(),
                        span: start..self.pos,
                    });
                }
                Some(b'(') => {
                    // A nested child pattern — one level only: a
                    // grandchild is a validation error.
                    let child_start = self.pos;
                    self.pos += 1;
                    self.skip_ws();
                    let cname_start = self.pos;
                    while self
                        .peek()
                        .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    {
                        self.pos += 1;
                    }
                    let cname =
                        String::from_utf8_lossy(&self.bytes[cname_start..self.pos]).into_owned();
                    let (ckind, cwild) = if cname == "_" {
                        (u16::MAX, true)
                    } else {
                        let Some(k) = resolver.resolve(&cname) else {
                            return Err(QueryError {
                                message: format!("unknown kind `{cname}`"),
                                span: cname_start..self.pos,
                            });
                        };
                        (k, false)
                    };
                    self.skip_ws();
                    // Grandchildren are unsupported (documented subset).
                    if self.peek() == Some(b'(') {
                        return Err(QueryError {
                            message: "nested sub-patterns beyond one level are unsupported"
                                .to_owned(),
                            span: child_start..self.pos,
                        });
                    }
                    self.expect(b')')?;
                    // The capture binds the CHILD: `(string) @key ?`.
                    self.skip_ws();
                    let mut child_cap = None;
                    if self.peek() == Some(b'@') {
                        self.pos += 1;
                        child_cap = Some(self.capture_slot(captures)?);
                    }
                    let quant = self.parse_quant();
                    children.push(ChildStep {
                        kind: ckind,
                        capture: child_cap,
                        quant,
                    });
                    let _ = (child_start, cwild);
                }
                Some(b'@') => {
                    return Err(QueryError {
                        message: "a bare capture outside a pattern is not a child".to_owned(),
                        span: self.pos..self.pos + 1,
                    });
                }
                _ => {
                    return Err(QueryError {
                        message: "expected a child pattern or `)`".to_owned(),
                        span: self.pos..self.pos + 1,
                    });
                }
            }
        }
        Ok(QueryStep {
            kind,
            wildcard,
            capture,
            children,
            span: start..self.pos,
        })
    }

    /// `?` / `*` / `+` after a child pattern.
    fn parse_quant(&mut self) -> Quant {
        match self.peek() {
            Some(b'?') => {
                self.pos += 1;
                Quant::ZeroOrOne
            }
            Some(b'*') => {
                self.pos += 1;
                Quant::ZeroOrMore
            }
            Some(b'+') => {
                self.pos += 1;
                Quant::OneOrMore
            }
            _ => Quant::One,
        }
    }

    /// Map a capture name to a dense slot (order of first appearance).
    fn capture_slot(&mut self, captures: &mut Vec<String>) -> Result<u8, QueryError> {
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(QueryError {
                message: "empty capture name".to_owned(),
                span: start..start + 1,
            });
        }
        let name = String::from_utf8_lossy(&self.bytes[start..self.pos]).into_owned();
        if let Some(slot) = captures.iter().position(|existing| *existing == name) {
            return Ok(u8::try_from(slot).unwrap_or(u8::MAX));
        }
        captures.push(name);
        Ok(u8::try_from(captures.len() - 1).unwrap_or(u8::MAX))
    }
}

/// Compile is folded into [`parse`] (the parser resolves and lowers in
/// one pass); this alias documents the two-phase intent.
///
/// # Errors
/// [`QueryError`] for malformed pattern syntax or unknown kind names
/// (same diagnostics as [`parse`]).
pub fn compile(text: &str, resolver: &dyn KindResolver) -> Result<CompiledQuery, QueryError> {
    parse(text, resolver)
}
