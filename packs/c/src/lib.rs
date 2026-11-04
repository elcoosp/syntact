//! # packs-c — the C11-subset grammar pack (issue 034)
//!
//! Reference parser for a documented C11 subset on the runtime-core
//! event protocol: declarations with pointer/type qualifiers and the
//! full declarator grammar (`int (*f)(void)`), function definitions,
//! structs/unions/enums, statements, precedence expressions, and
//! preprocessor lines as structured trivia.
//!
//! # Documented subset (v0)
//!
//! - **Declarations**: base types (`int`, `char`, `void`, `unsigned`,
//!   `signed`, `long`, `short`, `float`, `double`, `const`, `static`,
//!   `struct S`, `enum E`, `union U`, typedef names), declarators with
//!   `*` pointers, `[N]` arrays, `(params)` function prototypes, and
//!   parenthesised function-pointer declarators (`int (*f)(void)`).
//!   Initialisers parse as expressions.
//! - **Statements**: expression statements, `if`/`else`, `while`,
//!   `do…while`, `for` (init;cond;step), `return`, `break`,
//!   `continue`, blocks, empty `;`.
//! - **Expressions**: precedence climbing over the C11 operator table
//!   (comma lowest, assignment right-assoc, ternary, `||…` to
//!   postfix calls/index/member access, unary `* & - ! ~ ++ --`
//!   and `sizeof`).
//! - **Typedef shadowing heuristic**: a name the lexer has seen in a
//!   `typedef` is treated as a type in later declarations (a
//!   single-file, first-definition-wins heuristic — documented).
//! - **Preprocessor**: `#…` lines (up to the newline) become one
//!   PREPROC node holding the raw line tokens — structured trivia for
//!   the grammar, still lossless.
//! - Strings/chars with escapes, line/block comments as trivia,
//!   newlines are pure trivia (C is brace- and semicolon-structured).
//!
//! Kind numbering (documented mapping, mirroring the other packs):
//! node kinds in [`kinds`], leaf tokens + punctuation in
//! [`token_kinds`] (dense continuation), offending regions via
//! [`runtime_core::ERROR_KIND`], implicit root [`ROOT_KIND`].

pub mod reference;

/// Node kinds for the C11-subset grammar (dense).
pub mod kinds {
    /// A declaration `int x, *p;`.
    pub const DECLARATION: u16 = 0;
    /// A function definition `int f(…) { … }`.
    pub const FUNCTION: u16 = 1;
    /// A parameter list.
    pub const PARAMS: u16 = 2;
    /// `struct`/`union`/`enum` specifier.
    pub const RECORD: u16 = 3;
    /// One struct/union field or enum constant.
    pub const MEMBER: u16 = 4;
    /// `{ … }` block.
    pub const BLOCK: u16 = 5;
    /// `if` statement.
    pub const IF: u16 = 6;
    /// `else` clause.
    pub const ELSE: u16 = 7;
    /// `while` / `do…while`.
    pub const WHILE: u16 = 8;
    /// `for (…;…;…)`.
    pub const FOR: u16 = 9;
    /// `return`.
    pub const RETURN: u16 = 10;
    /// `break`/`continue`.
    pub const JUMP: u16 = 11;
    /// `typedef`.
    pub const TYPEDEF: u16 = 12;
    /// `#…` preprocessor line (structured trivia).
    pub const PREPROC: u16 = 13;
    /// Expression statement.
    pub const EXPR_STMT: u16 = 14;
    /// Assignment expression (right-assoc).
    pub const ASSIGN: u16 = 15;
    /// Binary operation (incl. comparisons).
    pub const BINOP: u16 = 16;
    /// Unary `* & - ! ~ ++ -- sizeof`.
    pub const UNARY: u16 = 17;
    /// Ternary `c ? a : b`.
    pub const CONDITIONAL: u16 = 18;
    /// Call `f(args)`.
    pub const CALL: u16 = 19;
    /// Index `a[i]`.
    pub const INDEX: u16 = 20;
    /// Member `a.b` / `a->b`.
    pub const MEMBER_ACCESS: u16 = 21;
    /// Cast `(T) expr`.
    pub const CAST: u16 = 22;
    /// sizeof / `enum`-tag / parenthesised type in expressions.
    pub const TYPE_EXPR: u16 = 23;
    /// A declarator group `(*f)(…)` / `x[3]`.
    pub const DECLARATOR: u16 = 24;
    /// `switch (…) { … }`.
    pub const SWITCH: u16 = 79;
    /// A label: `case e:`, `default:`, or `name:`.
    pub const CASE_LABEL: u16 = 80;
    /// `goto name;`.
    pub const GOTO: u16 = 81;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// An identifier or keyword (text-matched by the parser).
    pub const NAME: u16 = 25;
    /// A number literal (int/float, suffixes included).
    pub const NUMBER: u16 = 26;
    /// A string literal `"…"`.
    pub const STRING: u16 = 27;
    /// A char literal `'…'`.
    pub const CHAR: u16 = 28;
    /// `{`
    pub const LBRACE: u16 = 29;
    /// `}`
    pub const RBRACE: u16 = 30;
    /// `(`
    pub const LPAREN: u16 = 31;
    /// `)`
    pub const RPAREN: u16 = 32;
    /// `[`
    pub const LBRACKET: u16 = 33;
    /// `]`
    pub const RBRACKET: u16 = 34;
    /// `;`
    pub const SEMI: u16 = 35;
    /// `,`
    pub const COMMA: u16 = 36;
    /// `:`
    pub const COLON: u16 = 37;
    /// `.`
    pub const DOT: u16 = 38;
    /// `->`
    pub const ARROW: u16 = 39;
    /// `?`
    pub const QUESTION: u16 = 40;
    /// `+`
    pub const PLUS: u16 = 41;
    /// `-`
    pub const MINUS: u16 = 42;
    /// `*`
    pub const STAR: u16 = 43;
    /// `/`
    pub const SLASH: u16 = 44;
    /// `%`
    pub const PERCENT: u16 = 45;
    /// `!`
    pub const BANG: u16 = 46;
    /// `&`
    pub const AMP: u16 = 47;
    /// `|`
    pub const PIPE: u16 = 48;
    /// `^`
    pub const CARET: u16 = 49;
    /// `~`
    pub const TILDE: u16 = 50;
    /// `==`
    pub const EQ: u16 = 51;
    /// `!=`
    pub const NEQ: u16 = 52;
    /// `<=`
    pub const LE: u16 = 53;
    /// `>=`
    pub const GE: u16 = 54;
    /// `<`
    pub const LT: u16 = 55;
    /// `>`
    pub const GT: u16 = 56;
    /// `&&`
    pub const AND: u16 = 57;
    /// `||`
    pub const OR: u16 = 58;
    /// `<<`
    pub const LSHIFT: u16 = 59;
    /// `>>`
    pub const RSHIFT: u16 = 60;
    /// `++`
    pub const INCREMENT: u16 = 61;
    /// `--`
    pub const DECREMENT: u16 = 62;
    /// `=`
    pub const ASSIGN: u16 = 63;
    /// `+=`
    pub const PLUSEQ: u16 = 64;
    /// `-=`
    pub const MINUSEQ: u16 = 65;
    /// `*=`
    pub const STAREQ: u16 = 66;
    /// `/=`
    pub const SLASHEQ: u16 = 67;
    /// `%=`
    pub const PERCENTEQ: u16 = 68;
    /// `&=`
    pub const AMP_EQ: u16 = 69;
    /// `|=`
    pub const PIPE_EQ: u16 = 70;
    /// `^=`
    pub const CARET_EQ: u16 = 71;
    /// `<<=`
    pub const LSHIFT_EQ: u16 = 72;
    /// `>>=`
    pub const RSHIFT_EQ: u16 = 73;
    /// `…` (ellipsis)
    pub const ELLIPSIS: u16 = 74;
    /// `#`
    pub const HASH: u16 = 75;
    /// Whitespace trivia.
    pub const WHITESPACE: u16 = 76;
    /// `// …` line-comment trivia.
    pub const COMMENT: u16 = 77;
    /// `/* … */` block-comment trivia.
    pub const BLOCK_COMMENT: u16 = 78;
}

/// Implicit translation-unit root kind — pack-local sentinel.
pub const ROOT_KIND: u16 = u16::MAX - 5;

/// Parse C11-subset source into a green tree (the full integration:
/// `parse_events` → `process` → `drive_sink` → `TreeBuilder`).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *C* is recovered, not an error.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;
