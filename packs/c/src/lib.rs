//! # packs-c — the C grammar pack (issue 034, documented subset)
//!
//! Reference parser for a C11 subset on the runtime-core event
//! protocol.
//!
//! **The subset** (everything else errors and recovers):
//! - declarations with pointer / qualifier spellings (`int *const x`,
//!   `static unsigned long n`), `typedef` declarations feeding the
//!   **typedef shadowing heuristic** (a known typedef name in
//!   `T * x;` / `T (f)(…);` starts a declaration, otherwise the
//!   statement is an expression);
//! - function definitions with full declarators — `int (*f)(void)`
//!   (parenthesized pointer-to-function), `int a[3]`, `char **argv`;
//! - structs / unions / enums with fields and enumerators;
//! - statements: compound, `if`/`else`, `while`, `do…while`, `for`
//!   (declaration or expression head), `return`, `break`,
//!   `continue`, expression statements;
//! - precedence expressions: assignment (right-assoc), `?:`,
//!   `|| && | ^ & == != < > <= >= << >> + - * / %`, unary
//!   (`! ~ - + * & ++ --`), casts via the `(T)` lookahead, calls,
//!   index, field access (`.` and `->`), `sizeof`, initializers
//!   `{1, 2, {3}}`;
//! - preprocessor lines (`#include`, `#define`, `#ifdef`, …, with
//!   `\`-continuations) as structured DIRECTIVE nodes.
//!
//! **Boundaries** (documented): no bit-field widths beyond tokens, no
//! `_Atomic`/`_Generic`/`_Static_assert`, no VLAs beyond `[expr]`
//! token soup, no `goto`/labels, no compound literals, declarations
//! keep declarator spelling as token soup inside the node.

pub mod lexer;
pub mod reference;

/// Node kinds for the C grammar (dense).
pub mod kinds {
    /// `ret f(params) { … }`.
    pub const FUNCTION_DEF: u16 = 0;
    /// A declaration (`int x = 5;`, `typedef …`, `struct S s;`).
    pub const DECLARATION: u16 = 1;
    /// `(a, b)` parameter list.
    pub const PARAM_LIST: u16 = 2;
    /// One parameter declaration.
    pub const PARAM_DECL: u16 = 3;
    /// `struct S { … }` specifier.
    pub const STRUCT_SPEC: u16 = 4;
    /// `union U { … }` specifier.
    pub const UNION_SPEC: u16 = 5;
    /// `enum E { … }` specifier.
    pub const ENUM_SPEC: u16 = 6;
    /// One enumerator `A (= expr)?`.
    pub const ENUMERATOR: u16 = 7;
    /// `Type name;` inside a struct/union body.
    pub const FIELD_DECL: u16 = 8;
    /// `{ stmts }`.
    pub const COMPOUND_STMT: u16 = 9;
    /// `if (c) s (else s)?`.
    pub const IF_STMT: u16 = 10;
    /// `while (c) s`.
    pub const WHILE_STMT: u16 = 11;
    /// `do s while (c);`.
    pub const DO_STMT: u16 = 12;
    /// `for (head) s`.
    pub const FOR_STMT: u16 = 13;
    /// `return (e)?;`.
    pub const RETURN_STMT: u16 = 14;
    /// `break;`.
    pub const BREAK_STMT: u16 = 15;
    /// `continue;`.
    pub const CONTINUE_STMT: u16 = 16;
    /// `e;`.
    pub const EXPR_STMT: u16 = 17;
    /// `f(args)` call.
    pub const CALL_EXPR: u16 = 18;
    /// `a.b` / `a->b`.
    pub const FIELD_ACCESS: u16 = 19;
    /// `a[i]`.
    pub const INDEX_EXPR: u16 = 20;
    /// `(T) e` cast.
    pub const CAST_EXPR: u16 = 21;
    /// Binary operation (op kind in the child token).
    pub const BIN_EXPR: u16 = 22;
    /// Unary `! ~ - + * & ++ -- sizeof`.
    pub const UNARY_EXPR: u16 = 23;
    /// `c ? t : e`.
    pub const COND_EXPR: u16 = 24;
    /// `( e )` grouping.
    pub const PAREN_EXPR: u16 = 25;
    /// `{1, 2, {3}}` initializer list.
    pub const INIT_LIST: u16 = 26;
    /// `(T)` / `sizeof(T)` type spellings.
    pub const TYPE_NAME: u16 = 27;
    /// One preprocessor line (`#include`, `#define`, …).
    pub const DIRECTIVE: u16 = 28;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// Identifier.
    pub const IDENT: u16 = 29;
    /// Integer literal (hex / octal / decimal, suffix included).
    pub const INT_LIT: u16 = 30;
    /// Float literal (`1.5`, `1e3`, suffix included).
    pub const FLOAT_LIT: u16 = 31;
    /// `'a'` char literal with escapes.
    pub const CHAR_LIT: u16 = 32;
    /// `"…"` string with escapes.
    pub const STRING_LIT: u16 = 33;
    /// One preprocessor line `# …` (with `\` continuations).
    pub const PP_LINE: u16 = 34;
    /// `+`
    pub const PLUS: u16 = 35;
    /// `-`
    pub const MINUS: u16 = 36;
    /// `*`
    pub const STAR: u16 = 37;
    /// `/`
    pub const SLASH: u16 = 38;
    /// `%`
    pub const PERCENT: u16 = 39;
    /// `&`
    pub const AMP: u16 = 40;
    /// `|`
    pub const PIPE: u16 = 41;
    /// `^`
    pub const CARET: u16 = 42;
    /// `~`
    pub const TILDE: u16 = 43;
    /// `!`
    pub const BANG: u16 = 44;
    /// `&&`
    pub const ANDAND: u16 = 45;
    /// `||`
    pub const OROR: u16 = 46;
    /// `==`
    pub const EQ: u16 = 47;
    /// `!=`
    pub const NE: u16 = 48;
    /// `<`
    pub const LT: u16 = 49;
    /// `>`
    pub const GT: u16 = 50;
    /// `<=`
    pub const LE: u16 = 51;
    /// `>=`
    pub const GE: u16 = 52;
    /// `<<`
    pub const SHL: u16 = 53;
    /// `>>`
    pub const SHR: u16 = 54;
    /// `=`
    pub const ASSIGN: u16 = 55;
    /// `+=`
    pub const PLUSEQ: u16 = 56;
    /// `-=`
    pub const MINUSEQ: u16 = 57;
    /// `*=`
    pub const STAREQ: u16 = 58;
    /// `/=`
    pub const SLASHEQ: u16 = 59;
    /// `%=`
    pub const PERCENTEQ: u16 = 60;
    /// `&=`
    pub const ANDEQ: u16 = 61;
    /// `|=`
    pub const OREQ: u16 = 62;
    /// `^=`
    pub const XOREQ: u16 = 63;
    /// `<<=`
    pub const SHLEQ: u16 = 64;
    /// `>>=`
    pub const SHREQ: u16 = 65;
    /// `.`
    pub const DOT: u16 = 66;
    /// `->`
    pub const ARROW: u16 = 67;
    /// `:`
    pub const COLON: u16 = 68;
    /// `;`
    pub const SEMI: u16 = 69;
    /// `,`
    pub const COMMA: u16 = 70;
    /// `?`
    pub const QUESTION: u16 = 71;
    /// `(`
    pub const LPAREN: u16 = 72;
    /// `)`
    pub const RPAREN: u16 = 73;
    /// `[`
    pub const LBRACK: u16 = 74;
    /// `]`
    pub const RBRACK: u16 = 75;
    /// `{`
    pub const LBRACE: u16 = 76;
    /// `}`
    pub const RBRACE: u16 = 77;
    /// `++`
    pub const INCR: u16 = 78;
    /// `--`
    pub const DECR: u16 = 79;
    /// `int`
    pub const KW_INT: u16 = 80;
    /// `char`
    pub const KW_CHAR: u16 = 81;
    /// `float`
    pub const KW_FLOAT: u16 = 82;
    /// `double`
    pub const KW_DOUBLE: u16 = 83;
    /// `void`
    pub const KW_VOID: u16 = 84;
    /// `long`
    pub const KW_LONG: u16 = 85;
    /// `short`
    pub const KW_SHORT: u16 = 86;
    /// `signed`
    pub const KW_SIGNED: u16 = 87;
    /// `unsigned`
    pub const KW_UNSIGNED: u16 = 88;
    /// `_Bool`
    pub const KW_BOOL: u16 = 89;
    /// `const`
    pub const KW_CONST: u16 = 90;
    /// `volatile`
    pub const KW_VOLATILE: u16 = 91;
    /// `struct`
    pub const KW_STRUCT: u16 = 92;
    /// `union`
    pub const KW_UNION: u16 = 93;
    /// `enum`
    pub const KW_ENUM: u16 = 94;
    /// `if`
    pub const KW_IF: u16 = 95;
    /// `else`
    pub const KW_ELSE: u16 = 96;
    /// `while`
    pub const KW_WHILE: u16 = 97;
    /// `do`
    pub const KW_DO: u16 = 98;
    /// `for`
    pub const KW_FOR: u16 = 99;
    /// `return`
    pub const KW_RETURN: u16 = 100;
    /// `break`
    pub const KW_BREAK: u16 = 101;
    /// `continue`
    pub const KW_CONTINUE: u16 = 102;
    /// `sizeof`
    pub const KW_SIZEOF: u16 = 103;
    /// `typedef`
    pub const KW_TYPEDEF: u16 = 104;
    /// `static`
    pub const KW_STATIC: u16 = 105;
    /// `extern`
    pub const KW_EXTERN: u16 = 106;
    /// `inline`
    pub const KW_INLINE: u16 = 107;
    /// Whitespace trivia (never a token event, ADR-008).
    pub const WHITESPACE: u16 = 108;
    /// `//` and `/* */` comment trivia.
    pub const COMMENT: u16 = 109;
}

/// Implicit document-root kind — pack-local sentinel below the
/// runtime-core sentinels.
pub const ROOT_KIND: u16 = u16::MAX - 9;

/// Parse C source into a green tree (the full integration).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;
