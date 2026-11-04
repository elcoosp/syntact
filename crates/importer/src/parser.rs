//! JS-subset recursive-descent parser (issue 037).
//!
//! Recovers a small value tree out of the token stream — object
//! literals, arrays, calls, member chains, string/regex/number
//! literals, and `$ => body` arrows. The converter (see `convert`)
//! evaluates the tree; this layer only sees shapes.

use crate::ImportError;
use crate::lexer::{Tok, Token};

/// A parsed JS value with its source offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned {
    /// The value.
    pub val: Val,
    /// Byte offset where the value starts.
    pub offset: usize,
}

/// The JS values the subset represents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Val {
    /// An identifier (`choice`, `$`).
    Ident(String),
    /// A string literal (unescaped).
    Str(String),
    /// A regex literal (body, flags).
    Regex(String, String),
    /// A numeric literal (verbatim text).
    Num(String),
    /// An array literal.
    Array(Vec<Spanned>),
    /// An object literal (insertion-ordered properties).
    Object(Vec<(String, Spanned)>),
    /// A call: `callee(...)` with dotted callees flattened
    /// (`prec.left` → `prec.left`).
    Call {
        /// Flattened callee name.
        callee: String,
        /// Arguments in order.
        args: Vec<Spanned>,
    },
    /// A member access: `obj.prop` (`$.value`, `prec.left`).
    Member {
        /// The object expression.
        obj: Box<Spanned>,
        /// The property name.
        prop: String,
    },
    /// An arrow function `$ => body` (parameter name is ignored — the
    /// subset only ever binds the grammar handle).
    Arrow {
        /// The body expression.
        body: Box<Spanned>,
    },
}

/// A parsed module: the `grammar({...})` object plus how many unrelated
/// top-level statements were skipped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// Properties of the grammar object literal.
    pub props: Vec<(String, Spanned)>,
    /// Names of skipped top-level statements (`function commaSep1`, …).
    pub skipped: Vec<String>,
}

/// Parse a whole module, locating `module.exports = grammar({...})`.
///
/// # Errors
/// [`ImportError::Parse`] on malformed syntax; [`ImportError::NoGrammar`]
/// when no `module.exports = grammar({...})` statement exists.
pub fn parse_module(tokens: &[Token]) -> Result<Module, ImportError> {
    let mut parser = Parser { tokens, pos: 0 };
    let mut skipped = Vec::new();
    let mut props: Option<Vec<(String, Spanned)>> = None;
    loop {
        if matches!(parser.peek(), Tok::Ident(name) if name == "module") {
            let start = parser.pos;
            if let Some(found) = parser.try_module_exports()? {
                props = Some(found);
            } else {
                // Looked like module.exports but not the shape we
                // support: skip it and keep scanning.
                parser.pos = start;
                parser.skip_statement(&mut skipped)?;
            }
        } else if parser.peek_done() {
            break;
        } else {
            parser.skip_statement(&mut skipped)?;
        }
        if parser.peek_done() {
            break;
        }
    }
    match props {
        Some(props) => Ok(Module { props, skipped }),
        None => Err(ImportError::NoGrammar),
    }
}

struct Parser<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        &self.tokens[self.pos].tok
    }

    fn peek_done(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }

    fn offset(&self) -> usize {
        self.tokens[self.pos].start
    }

    fn bump(&mut self) -> Token {
        let tok = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        tok
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if matches!(self.peek(), Tok::Punct(got) if *got == p) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, p: &str) -> Result<(), ImportError> {
        if self.eat_punct(p) {
            Ok(())
        } else {
            Err(self.err(format!("expected `{p}`")))
        }
    }

    fn err(&self, message: impl Into<String>) -> ImportError {
        ImportError::Parse {
            offset: self.offset(),
            message: message.into(),
        }
    }

    /// Try to match `module . exports = grammar ( { ... } )` at the
    /// current position. On success returns the grammar object's
    /// properties and leaves the cursor after the call. Returns `None`
    /// (cursor untouched) when the statement has another shape.
    fn try_module_exports(&mut self) -> Result<Option<Vec<(String, Spanned)>>, ImportError> {
        let start = self.pos;
        self.bump(); // `module`
        if !(self.eat_punct(".") && matches!(self.peek(), Tok::Ident(name) if name == "exports")) {
            self.pos = start;
            return Ok(None);
        }
        self.bump(); // `exports`
        if !self.eat_punct("=") {
            self.pos = start;
            return Ok(None);
        }
        let callee_ok = matches!(self.peek(), Tok::Ident(name) if name == "grammar");
        if !callee_ok {
            self.pos = start;
            return Ok(None);
        }
        self.bump(); // `grammar`
        if !self.eat_punct("(") {
            self.pos = start;
            return Ok(None);
        }
        let obj = self.parse_value()?;
        if !self.eat_punct(")") {
            self.pos = start;
            return Ok(None);
        }
        self.eat_punct(";");
        match obj.val {
            Val::Object(props) => Ok(Some(props.into_iter().collect())),
            _ => Err(self.err("`grammar(...)` expects an object literal")),
        }
    }

    /// Skip one top-level statement: everything up to the next `;` at
    /// brace depth 0, or one balanced `{...}` block when the statement
    /// opens one (function bodies, object literals).
    fn skip_statement(&mut self, skipped: &mut Vec<String>) -> Result<(), ImportError> {
        let start_tok = self.tokens[self.pos].clone();
        let mut depth = 0i32;
        let mut name = String::new();
        if let Tok::Ident(first) = &start_tok.tok {
            name.clone_from(first);
            if first == "function" {
                if let Tok::Ident(fn_name) = &self.tokens[self.pos + 1].tok {
                    name = format!("function {fn_name}");
                }
            }
        }
        loop {
            match self.peek() {
                Tok::Eof => {
                    skipped.push(name);
                    return Ok(());
                }
                Tok::Punct("{") => {
                    depth += 1;
                    self.bump();
                }
                Tok::Punct("}") => {
                    depth -= 1;
                    self.bump();
                    if depth <= 0 {
                        skipped.push(name);
                        self.eat_punct(";");
                        return Ok(());
                    }
                }
                Tok::Punct(";") if depth <= 0 => {
                    self.bump();
                    skipped.push(name);
                    return Ok(());
                }
                _ => {
                    self.bump();
                }
            }
        }
    }

    /// Parse the inside of an object literal (insertion-ordered props).
    fn parse_object_props(&mut self) -> Result<Vec<(String, Spanned)>, ImportError> {
        self.bump();
        let mut props = Vec::new();
        if self.eat_punct("}") {
            return Ok(props);
        }
        loop {
            let key = match self.peek().clone() {
                Tok::Ident(name) => {
                    self.bump();
                    name
                }
                Tok::Str(text) | Tok::Num(text) => {
                    self.bump();
                    text
                }
                _ => return Err(self.err("expected an object key")),
            };
            self.expect_punct(":")?;
            let value = self.parse_value()?;
            props.push((key, value));
            if self.eat_punct(",") {
                if self.eat_punct("}") {
                    break;
                }
            } else {
                self.expect_punct("}")?;
                break;
            }
        }
        Ok(props)
    }

    fn parse_value(&mut self) -> Result<Spanned, ImportError> {
        let offset = self.offset();
        let val = match self.peek().clone() {
            Tok::Ident(name) => {
                self.bump();
                if self.eat_punct("=>") {
                    let body = self.parse_value()?;
                    Val::Arrow {
                        body: Box::new(body),
                    }
                } else if matches!(self.peek(), Tok::Punct("(")) {
                    self.bump();
                    let mut args = Vec::new();
                    if !self.eat_punct(")") {
                        loop {
                            args.push(self.parse_value()?);
                            if self.eat_punct(",") {
                                if self.eat_punct(")") {
                                    break;
                                }
                            } else {
                                self.expect_punct(")")?;
                                break;
                            }
                        }
                    }
                    Val::Call { callee: name, args }
                } else {
                    Val::Ident(name)
                }
            }
            Tok::Str(text) => {
                self.bump();
                Val::Str(text)
            }
            Tok::Regex(body, flags) => {
                self.bump();
                Val::Regex(body, flags)
            }
            Tok::Num(text) => {
                self.bump();
                Val::Num(text)
            }
            Tok::Punct("[") => {
                self.bump();
                let mut items = Vec::new();
                if !self.eat_punct("]") {
                    loop {
                        items.push(self.parse_value()?);
                        if self.eat_punct(",") {
                            if self.eat_punct("]") {
                                break;
                            }
                        } else {
                            self.expect_punct("]")?;
                            break;
                        }
                    }
                }
                Val::Array(items)
            }
            Tok::Punct("{") => Val::Object(self.parse_object_props()?),
            Tok::Punct("(") => {
                self.bump();
                let inner = self.parse_value()?;
                self.expect_punct(")")?;
                return Ok(inner);
            }
            other => return Err(self.err(format!("unexpected {other} in expression"))),
        };
        let node = Spanned { val, offset };
        self.parse_postfix(node)
    }

    /// Consume a postfix member chain (`$.value`, `prec.left`): each
    /// `.prop` either joins a call (`prec.left(...)`) or wraps the node
    /// in a member access.
    fn parse_postfix(&mut self, mut node: Spanned) -> Result<Spanned, ImportError> {
        loop {
            if self.eat_punct(".") {
                let prop = match self.peek().clone() {
                    Tok::Ident(name) => {
                        self.bump();
                        name
                    }
                    Tok::Num(text) => {
                        self.bump();
                        text
                    }
                    _ => return Err(self.err("expected a property name after `.`")),
                };
                // A member followed by a call flattens into one callee.
                if matches!(self.peek(), Tok::Punct("(")) {
                    self.bump();
                    let mut args = Vec::new();
                    if !self.eat_punct(")") {
                        loop {
                            args.push(self.parse_value()?);
                            if self.eat_punct(",") {
                                if self.eat_punct(")") {
                                    break;
                                }
                            } else {
                                self.expect_punct(")")?;
                                break;
                            }
                        }
                    }
                    let callee = match &node.val {
                        Val::Ident(base) => format!("{base}.{prop}"),
                        _ => return Err(self.err("only plain callees support `.` calls")),
                    };
                    node = Spanned {
                        val: Val::Call { callee, args },
                        offset: node.offset,
                    };
                } else {
                    let offset = node.offset;
                    node = Spanned {
                        val: Val::Member {
                            obj: Box::new(node),
                            prop,
                        },
                        offset,
                    };
                }
            } else {
                break;
            }
        }
        Ok(node)
    }
}
