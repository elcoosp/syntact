//! # packs-python — the Python grammar pack (issue 028, documented subset)
//!
//! Reference parser for a documented Python subset on the runtime-core
//! event protocol.
//!
//! **The subset** (everything else errors and recovers):
//! - statements: expression/assignment, `return` (optional value),
//!   `pass`, `break`, `continue`, `if`/`elif`/`else`, `while`, `for … in`,
//!   `def` (positional params with optional defaults), `@` decorators;
//! - expressions: full Pratt surface — `or`/`and`/`not`, comparisons
//!   (`== != < > <= >= in is`), `| ^ & << >> + - * / // %`, unary
//!   `- + ~`, right-assoc `**`, calls, attributes, subscripts, names,
//!   numbers, strings, f-strings, `True`/`False`/`None`, parenthesized
//!   groups/tuples, list displays and list comprehensions
//!   (`[e for x in it (if c)*]`, single-name targets);
//! - **INDENT/DEDENT** suites (spaces; tabs are an error region),
//!   bracket continuation (no NEWLINE inside `()[]`),
//!   `#` comments and blanks as trivia, and **f-strings with nested
//!   interpolation** (`f"a {f"{b}"} c"`) — no format specs, no dict
//!   displays inside interpolations (documented).

pub mod lexer;
pub mod reference;

/// Node kinds for the Python grammar (dense).
pub mod kinds {
    /// A module (the implicit root wraps sibling statements).
    pub const FUNC_DEF: u16 = 0;
    /// `if` / `elif` header + suite.
    pub const IF_STMT: u16 = 1;
    /// `else:` clause.
    pub const ELSE_CLAUSE: u16 = 2;
    /// `while` loop.
    pub const WHILE_STMT: u16 = 3;
    /// `for` loop.
    pub const FOR_STMT: u16 = 4;
    /// `return` statement.
    pub const RETURN: u16 = 5;
    /// `pass`.
    pub const PASS: u16 = 6;
    /// `break`.
    pub const BREAK: u16 = 7;
    /// `continue`.
    pub const CONTINUE: u16 = 8;
    /// `target = value` assignment.
    pub const ASSIGN: u16 = 9;
    /// A bare expression statement.
    pub const EXPR_STMT: u16 = 10;
    /// One `@decorator` line.
    pub const DECORATOR: u16 = 11;
    /// `def name(params): suite`.
    pub const PARAMS: u16 = 12;
    /// Binary operation (op kind in the child token).
    pub const BIN_EXPR: u16 = 13;
    /// Unary operation.
    pub const UNARY_EXPR: u16 = 14;
    /// `f(args)` call.
    pub const CALL: u16 = 15;
    /// `obj.attr` attribute access.
    pub const ATTR: u16 = 16;
    /// `obj[expr]` subscript.
    pub const SUBSCRIPT: u16 = 17;
    /// `[a, b]` list display / comprehension.
    pub const LIST_EXPR: u16 = 18;
    /// `(a, b)` tuple / grouping.
    pub const TUPLE_EXPR: u16 = 19;
    /// `f"…"` interpolation container.
    pub const FSTRING: u16 = 20;
    /// `[e for x in it]` comprehension clause.
    pub const COMP_FOR: u16 = 21;
    /// comprehension `if` filter.
    pub const COMP_IF: u16 = 22;
    /// `name(args…)` function definition parameters.
    pub const FUNC_HEADER: u16 = 23;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// Identifier / keyword-shaped name.
    pub const NAME: u16 = 24;
    /// Int / float literal.
    pub const NUMBER: u16 = 25;
    /// Single-quoted string (no prefix).
    pub const STRING: u16 = 26;
    /// `f"` / `f'` opener.
    pub const FSTRING_START: u16 = 27;
    /// Literal text inside an f-string.
    pub const FSTRING_TEXT: u16 = 28;
    /// `{` opening an interpolation.
    pub const INTERP_LBRACE: u16 = 29;
    /// `}` closing an interpolation.
    pub const INTERP_RBRACE: u16 = 30;
    /// Closing quote of an f-string.
    pub const FSTRING_END: u16 = 31;
    /// Indentation increase.
    pub const INDENT: u16 = 32;
    /// Indentation decrease (one per level).
    pub const DEDENT: u16 = 33;
    /// Logical line end (outside brackets).
    pub const NEWLINE: u16 = 34;
    /// `if`
    pub const KW_IF: u16 = 35;
    /// `elif`
    pub const KW_ELIF: u16 = 36;
    /// `else`
    pub const KW_ELSE: u16 = 37;
    /// `while`
    pub const KW_WHILE: u16 = 38;
    /// `for`
    pub const KW_FOR: u16 = 39;
    /// `in`
    pub const KW_IN: u16 = 40;
    /// `def`
    pub const KW_DEF: u16 = 41;
    /// `return`
    pub const KW_RETURN: u16 = 42;
    /// `pass`
    pub const KW_PASS: u16 = 43;
    /// `break`
    pub const KW_BREAK: u16 = 44;
    /// `continue`
    pub const KW_CONTINUE: u16 = 45;
    /// `and`
    pub const KW_AND: u16 = 46;
    /// `or`
    pub const KW_OR: u16 = 47;
    /// `not`
    pub const KW_NOT: u16 = 48;
    /// `is`
    pub const KW_IS: u16 = 49;
    /// `True`
    pub const KW_TRUE: u16 = 50;
    /// `False`
    pub const KW_FALSE: u16 = 51;
    /// `None`
    pub const KW_NONE: u16 = 52;
    /// `+`
    pub const PLUS: u16 = 53;
    /// `-`
    pub const MINUS: u16 = 54;
    /// `*`
    pub const STAR: u16 = 55;
    /// `/`
    pub const SLASH: u16 = 56;
    /// `//`
    pub const DSLASH: u16 = 57;
    /// `%`
    pub const PERCENT: u16 = 58;
    /// `**`
    pub const PSTAR: u16 = 59;
    /// `==`
    pub const EQ: u16 = 60;
    /// `!=`
    pub const NE: u16 = 61;
    /// `<`
    pub const LT: u16 = 62;
    /// `>`
    pub const GT: u16 = 63;
    /// `<=`
    pub const LE: u16 = 64;
    /// `>=`
    pub const GE: u16 = 65;
    /// `=`
    pub const ASSIGN: u16 = 66;
    /// `(`
    pub const LPAREN: u16 = 67;
    /// `)`
    pub const RPAREN: u16 = 68;
    /// `[`
    pub const LBRACK: u16 = 69;
    /// `]`
    pub const RBRACK: u16 = 70;
    /// `:`
    pub const COLON: u16 = 71;
    /// `,`
    pub const COMMA: u16 = 72;
    /// `.`
    pub const DOT: u16 = 73;
    /// `->`
    pub const ARROW: u16 = 74;
    /// `@`
    pub const AT: u16 = 75;
    /// `|`
    pub const PIPE: u16 = 76;
    /// `^`
    pub const CARET: u16 = 77;
    /// `&`
    pub const AMP: u16 = 78;
    /// `~`
    pub const TILDE: u16 = 79;
    /// `<<`
    pub const LSHIFT: u16 = 80;
    /// `>>`
    pub const RSHIFT: u16 = 81;
    /// Whitespace trivia (never a token event, ADR-008).
    pub const WHITESPACE: u16 = 82;
    /// `# …` comment trivia.
    pub const COMMENT: u16 = 83;
}

/// Implicit document-root kind — pack-local sentinel below the
/// runtime-core sentinels.
pub const ROOT_KIND: u16 = u16::MAX - 6;

/// Parse Python source into a green tree (the full integration).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;
