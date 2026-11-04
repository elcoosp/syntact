//! # packs-typescript — the TypeScript-subset grammar pack (issue 027)
//!
//! Reference parser for a documented TypeScript subset on the
//! runtime-core event protocol: modules (`import`/`export`),
//! interfaces, type aliases, enums, classes (modifiers, fields,
//! methods), functions with optional/default/rest parameters, Pratt
//! expressions, template literals, ASI (newline-terminated statements,
//! optional `;`), and `//`/`/* … */` doc comments as trivia.
//!
//! # Documented subset (v0)
//!
//! - **Statements**: `import` (default / named / namespace / side-
//!   effect), `export` (declarations, default, named lists, `* from`),
//!   `interface`, `type`, `enum`/`const enum`, `class`, `function`,
//!   arrow functions (parenthesized params detected by a bounded
//!   pre-scan, single-parameter arrows without parens), `const`/`let`/
//!   `var` (with destructuring patterns), `if`/`else`, `while`,
//!   `for`/`for…in`/`for…of`, `return`, `break`, `continue`, `throw`,
//!   expression statements.
//! - **Not in the subset** (each parses as an error and recovers):
//!   `try`/`catch`/`finally`, `switch`, `do…while`, labels, generators
//!   (`function*`), `await`/`yield`, decorators, `?.`/`??`, `declare`,
//!   namespaces, abstract members beyond the modifier.
//! - **ASI**: statements end at a newline, `}`, or EOF; `;` is optional
//!   and consumed when present. Restricted productions apply to
//!   `return`/`throw` (no value across a newline). Infix operators
//!   continue across newlines exactly as JavaScript does (`a\n+ b` is
//    `a + b`), and so does the postfix `(`-call (`a\n(b)` is a call).
//! - **`>>` in nested generics**: `Array<Array<T>>` closes two generic
//!   arguments — the type parser splits the two-byte `>>` (and `>=`,
//!   `>>>`, `>>=`) into adjacent single-`>` tokens at the close sites;
//!   the test suite pins the two-token shape.
//! - **Types**: unions (`A | B`), generics (`Name<Args>`), arrays
//!   (`T[]`), tuples (`[A, B]`), parenthesized groups, and literal /
//!   keyword types. Function types, conditional types, mapped types,
//!   and template-literal types are out (each recovers as an error).
//! - **Template literals**: TEMPLATE node with start/middle chunks and
//!   `${expr}` interpolations parsed by the full Pratt grammar.
//!
//! Kind numbering (documented mapping, mirroring the JSON/TOML/Python
//! packs): node kinds in [`kinds`], leaf tokens + punctuation in
//! [`token_kinds`] (dense continuation), offending regions via
//! [`runtime_core::ERROR_KIND`], implicit root [`ROOT_KIND`].
//!
//! # Trivia discipline (ADR-008)
//!
//! Whitespace, newlines, `//` line comments and `/* … */` (incl. `/**`
//! doc comments) are never token events — they ride
//! [`runtime_core::sink::TriviaRecord`]s
//! attached at tree-build time, so `render(parse(x)) == x`
//! byte-for-byte. Newlines are trivia everywhere: statement structure
//! is brace + ASI based, not line based.

pub mod reference;

/// Node kinds for the TypeScript-subset grammar (dense).
pub mod kinds {
    /// `import … from "…"` declaration.
    pub const IMPORT: u16 = 0;
    /// `export …` declaration.
    pub const EXPORT: u16 = 1;
    /// One imported/exported name (with optional `as` alias).
    pub const ALIAS: u16 = 2;
    /// `interface Name<T> { … }`.
    pub const INTERFACE: u16 = 3;
    /// `type Name<T> = …`.
    pub const TYPE_ALIAS: u16 = 4;
    /// `class Name<T> extends … implements … { … }`.
    pub const CLASS: u16 = 5;
    /// `enum` / `const enum` declaration.
    pub const ENUM: u16 = 6;
    /// `function name<T>(params): Ret { … }`.
    pub const FUNCTION: u16 = 7;
    /// Parameter list (flat: params, optional/default/rest markers).
    pub const PARAMS: u16 = 8;
    /// `{ … }` statement or arrow body block.
    pub const BLOCK: u16 = 9;
    /// Class field (`x: T = init`).
    pub const FIELD: u16 = 10;
    /// Class method or constructor.
    pub const METHOD: u16 = 11;
    /// `if` statement.
    pub const IF: u16 = 12;
    /// `else` clause.
    pub const ELSE: u16 = 13;
    /// `while` statement.
    pub const WHILE: u16 = 14;
    /// `for` / `for…in` / `for…of` statement.
    pub const FOR: u16 = 15;
    /// `return` statement.
    pub const RETURN: u16 = 16;
    /// `break` statement.
    pub const BREAK: u16 = 17;
    /// `continue` statement.
    pub const CONTINUE: u16 = 18;
    /// `throw` statement.
    pub const THROW: u16 = 19;
    /// `const`/`let`/`var` declaration.
    pub const VAR_DECL: u16 = 20;
    /// Bare expression statement.
    pub const EXPR_STMT: u16 = 21;
    /// Assignment expression (`=`, compound forms — right-associative).
    pub const ASSIGN: u16 = 22;
    /// Binary operation (incl. comparisons — JS does not chain).
    pub const BINOP: u16 = 23;
    /// Unary operation (`-`/`+`/`!`/`~`/`++`/`--`/`typeof`/`new`).
    pub const UNARY: u16 = 24;
    /// Ternary conditional `c ? a : b`.
    pub const CONDITIONAL: u16 = 25;
    /// Call `f(args)`.
    pub const CALL: u16 = 26;
    /// Property access `a.b`.
    pub const ATTRIBUTE: u16 = 27;
    /// Index access `a[i]`.
    pub const INDEX: u16 = 28;
    /// Arrow function.
    pub const ARROW: u16 = 29;
    /// Template literal (start/middle/interpolations/end).
    pub const TEMPLATE: u16 = 30;
    /// Array literal.
    pub const ARRAY: u16 = 31;
    /// Object literal.
    pub const OBJECT: u16 = 32;
    /// `key: value` / `key = value` property.
    pub const PAIR: u16 = 33;
    /// A generic type reference `Name<Args>` (also plain names).
    pub const TYPE_REF: u16 = 34;
    /// Union type `A | B`.
    pub const TYPE_UNION: u16 = 35;
    /// Array type `T[]`.
    pub const TYPE_ARRAY: u16 = 36;
    /// Tuple type `[A, B]`.
    pub const TYPE_TUPLE: u16 = 37;
    /// Parenthesized type group.
    pub const TYPE_GROUP: u16 = 38;
    /// One interface/class body member.
    pub const MEMBER: u16 = 39;
    /// One generic type parameter (`<T extends U = V>`).
    pub const TYPE_PARAM: u16 = 40;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// An identifier or keyword (keywords are text-matched by the
    /// parser — the scanner emits `NAME` for both).
    pub const NAME: u16 = 41;
    /// A numeric literal.
    pub const NUMBER: u16 = 42;
    /// A string literal (`"…"` / `'…'`).
    pub const STRING: u16 = 43;
    /// Template literal opener: the backtick.
    pub const TEMPLATE_START: u16 = 44;
    /// Template literal text chunk (`\${`/`\\` escapes inside).
    pub const TEMPLATE_MIDDLE: u16 = 45;
    /// Template literal closer: the backtick.
    pub const TEMPLATE_END: u16 = 46;
    /// `{`
    pub const LBRACE: u16 = 47;
    /// `}`
    pub const RBRACE: u16 = 48;
    /// `(`
    pub const LPAREN: u16 = 49;
    /// `)`
    pub const RPAREN: u16 = 50;
    /// `[`
    pub const LBRACKET: u16 = 51;
    /// `]`
    pub const RBRACKET: u16 = 52;
    /// `<`
    pub const LT: u16 = 53;
    /// `>` — also the split first byte of `>>`/`>=`/`>>>`/`>>=`.
    pub const GT: u16 = 54;
    /// `,`
    pub const COMMA: u16 = 55;
    /// `;`
    pub const SEMI: u16 = 56;
    /// `:`
    pub const COLON: u16 = 57;
    /// `.`
    pub const DOT: u16 = 58;
    /// `?` (ternary / optional marker)
    pub const QUESTION: u16 = 59;
    /// `!` (non-null assertion)
    pub const BANG: u16 = 60;
    /// `...` (rest / spread)
    pub const ELLIPSIS: u16 = 61;
    /// `=>`
    pub const ARROW_FN: u16 = 62;
    /// `+`
    pub const PLUS: u16 = 63;
    /// `-`
    pub const MINUS: u16 = 64;
    /// `*`
    pub const STAR: u16 = 65;
    /// `/`
    pub const SLASH: u16 = 66;
    /// `%`
    pub const PERCENT: u16 = 67;
    /// `**`
    pub const POWER: u16 = 68;
    /// `==`
    pub const EQ: u16 = 69;
    /// `!=`
    pub const NEQ: u16 = 70;
    /// `===`
    pub const SEQ: u16 = 71;
    /// `!==`
    pub const SNEQ: u16 = 72;
    /// `<=`
    pub const LE: u16 = 73;
    /// `>=` — also the split first byte of `>=` inside generics.
    pub const GE: u16 = 74;
    /// `&&`
    pub const AND: u16 = 75;
    /// `||`
    pub const OR: u16 = 76;
    /// `&`
    pub const AMP: u16 = 77;
    /// `|`
    pub const PIPE: u16 = 78;
    /// `^`
    pub const CARET: u16 = 79;
    /// `~`
    pub const TILDE: u16 = 80;
    /// `<<`
    pub const LSHIFT: u16 = 81;
    /// `>>`
    pub const SHIFT: u16 = 82;
    /// `>>>`
    pub const USHIFT: u16 = 83;
    /// `++`
    pub const INCREMENT: u16 = 84;
    /// `--`
    pub const DECREMENT: u16 = 85;
    /// `=`
    pub const EQUALS: u16 = 86;
    /// `+=`
    pub const PLUSEQ: u16 = 87;
    /// `-=`
    pub const MINUSEQ: u16 = 88;
    /// `*=`
    pub const STAREQ: u16 = 89;
    /// `/=`
    pub const SLASHEQ: u16 = 90;
    /// `%=`
    pub const PERCENTEQ: u16 = 91;
    /// `**=`
    pub const POWEREQ: u16 = 92;
    /// `&&=`
    pub const AND_EQ: u16 = 93;
    /// `||=`
    pub const OR_EQ: u16 = 94;
    /// `&=`
    pub const AMP_EQ: u16 = 95;
    /// `|=`
    pub const PIPE_EQ: u16 = 96;
    /// `^=`
    pub const CARET_EQ: u16 = 97;
    /// `<<=`
    pub const LSHIFT_EQ: u16 = 98;
    /// `>>=`
    pub const SHIFT_EQ: u16 = 99;
    /// `>>>=`
    pub const USHIFT_EQ: u16 = 100;
    /// Whitespace (incl. newlines) trivia: never a token event.
    pub const WHITESPACE: u16 = 101;
    /// `// …` / `/* … */` (incl. `/** doc */`) comment trivia.
    pub const COMMENT: u16 = 102;
}

/// Implicit module-root kind for the green tree — pack-local sentinel,
/// numbered below the runtime-core sentinels so the pack's dense
/// `0..=102` ids stay collision-free.
pub const ROOT_KIND: u16 = u16::MAX - 5;

/// Parse TypeScript-subset source into a green tree (the full
/// integration: `parse_events` → `process` → `drive_sink` →
/// `TreeBuilder`).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *TypeScript* is recovered, not an error.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;
