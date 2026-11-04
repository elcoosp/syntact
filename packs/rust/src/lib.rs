//! # packs-rust — the Rust-subset grammar pack (issue 033)
//!
//! Reference parser for a documented Rust subset on the runtime-core
//! event protocol: items (fn/struct/enum/impl/trait/mod/use/const/
//! static/type), generics with lifetimes and where clauses, patterns,
//! Pratt expressions (closures, ranges, `?`/`as`), attributes, and
//! macros as opaque balanced token trees.
//!
//! # Documented subset (v0)
//!
//! - **Items**: `fn` (generics, where clauses, optional/default-less
//!   params, `-> Ret`), `struct` (unit/tuple/named), `enum` (variants,
//!   discriminants), `impl` (inherent + trait, generics + where),
//!   `trait`, `mod`, `use` trees, `const`/`static`, `type` aliases.
//!   `unsafe`/`async`/`const` ride modifier NAME tokens. `extern`
//!   blocks are out (recover as errors).
//! - **Generics**: `<T: A + B<'a>, const N: usize>` with lifetimes;
//!   `where T: Into<U>, 'a: 'b` predicates (split on top-level commas).
//! - **Expressions**: Pratt with `..`/`..=` ranges (lowest), `||`/`&&`
//!   … down to `**`; closures `|a, b| expr`; postfix `?`, calls, method
//!   calls, field/index access, `as` casts; `if`/`if let`/`while`/
//!   `for`/`loop`/`match` expressions with arms; blocks; `return`/
//!   `break`/`continue` (labels ride NAME tokens).
//! - **Generic `<` heuristic**: `::<` (turbofish) always opens generic
//!   arguments; a `<` glued to a path segment (`Vec<T>`, no whitespace
//!   before `<`) opens generics; any other `<` is a comparison.
//! - **Macros**: `name!(…)`, `name![…]`, `name!{…}` — one MACRO node
//!   whose contents are one `TOKEN_TREE` of balanced delimiters and raw
//!   tokens (the documented boundary: no macro-internal grammar).
//! - **Patterns**: literals, `_`, `mut x`, `x @ sub`, tuples, struct
//!   paths `{…}`, refs `&`/`&mut`, slices `[…]`, ranges, wildcards.
//! - **Attributes**: `#[…]/#![…]` — one ATTR node with a balanced
//!   `TOKEN_TREE` inside; `///` and `//!` doc comments are trivia (as
//!   are `//` and `/* … */`).
//! - **Statements**: `let pat: T = init;`, expression statements,
//!   `;`-terminated; newlines are pure trivia (Rust is brace- and
//!   semicolon-structured).
//!
//! Kind numbering (documented mapping, mirroring the other packs): node
//! kinds in [`kinds`], leaf tokens + punctuation in [`token_kinds`]
//! (dense continuation), offending regions via
//! [`runtime_core::ERROR_KIND`], implicit root [`ROOT_KIND`].
//!
//! # Trivia discipline (ADR-008)
//!
//! Whitespace and comments are never token events — they ride
//! [`runtime_core::sink::TriviaRecord`]s, so `render(parse(x)) == x` byte-for-byte.

pub mod reference;

/// Node kinds for the Rust-subset grammar (dense).
pub mod kinds {
    /// `fn` item.
    pub const FN: u16 = 0;
    /// `struct` item.
    pub const STRUCT: u16 = 1;
    /// `enum` item.
    pub const ENUM: u16 = 2;
    /// `impl` item.
    pub const IMPL: u16 = 3;
    /// `trait` item.
    pub const TRAIT: u16 = 4;
    /// `mod` item.
    pub const MOD: u16 = 5;
    /// `use` item.
    pub const USE: u16 = 6;
    /// `const`/`static` item.
    pub const CONST: u16 = 7;
    /// `type` alias.
    pub const TYPE_ALIAS: u16 = 8;
    /// A macro invocation `name!…`.
    pub const MACRO: u16 = 9;
    /// The opaque balanced token tree inside a macro or attribute.
    pub const TOKEN_TREE: u16 = 10;
    /// An attribute `#[…]`.
    pub const ATTR: u16 = 11;
    /// `{ … }` block.
    pub const BLOCK: u16 = 12;
    /// Generic parameter list `<…>`.
    pub const GENERIC_PARAMS: u16 = 13;
    /// `where` clause.
    pub const WHERE: u16 = 14;
    /// Parameter list of a fn/closure.
    pub const PARAMS: u16 = 15;
    /// `let` statement.
    pub const LET: u16 = 16;
    /// An expression statement.
    pub const EXPR_STMT: u16 = 17;
    /// `if` / `if let` expression.
    pub const IF: u16 = 18;
    /// `else` clause.
    pub const ELSE: u16 = 19;
    /// `while` / `while let`.
    pub const WHILE: u16 = 20;
    /// `for pat in iter`.
    pub const FOR: u16 = 21;
    /// `loop { … }`.
    pub const LOOP: u16 = 22;
    /// `match scrutinee { arms }`.
    pub const MATCH: u16 = 23;
    /// One match arm `pat => expr`.
    pub const ARM: u16 = 24;
    /// `return` expression.
    pub const RETURN: u16 = 25;
    /// `break` / `continue`.
    pub const BREAK: u16 = 26;
    /// `continue`.
    pub const CONTINUE: u16 = 27;
    /// A closure `|…| expr`.
    pub const CLOSURE: u16 = 28;
    /// A range `a..b` / `a..=b` / `..x`.
    pub const RANGE: u16 = 29;
    /// A tuple expression or paren group.
    pub const TUPLE: u16 = 30;
    /// An array literal `[a, b]`.
    pub const ARRAY: u16 = 31;
    /// Index access `a[i]`.
    pub const INDEX: u16 = 32;
    /// Call `f(args)`.
    pub const CALL: u16 = 33;
    /// Method call `a.b(args)`.
    pub const METHOD: u16 = 34;
    /// Field access `a.b`.
    pub const FIELD: u16 = 35;
    /// Assignment `=`/compound (right-associative).
    pub const ASSIGN: u16 = 36;
    /// Binary operation.
    pub const BINOP: u16 = 37;
    /// Unary `-`/`!`/`*`/`&`/`&mut`.
    pub const UNARY: u16 = 38;
    /// Ternary-free Rust: the `?` postfix operator node.
    pub const TRY: u16 = 39;
    /// A struct-literal `Path { field: value, … }`.
    pub const STRUCT_LIT: u16 = 40;
    /// One `field: value` pair.
    pub const PAIR: u16 = 41;
    /// A struct/enum/tuple body `{ members }` — members nest flat.
    pub const BODY: u16 = 42;
    /// One named field or enum variant.
    pub const MEMBER: u16 = 43;
    /// A `use` spec tree (nested braces allowed).
    pub const USE_TREE: u16 = 44;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// An identifier or keyword (text-matched by the parser).
    pub const NAME: u16 = 45;
    /// A lifetime or label: `'a`, `'static` (no trailing quote).
    pub const LIFETIME: u16 = 46;
    /// A char literal `'a'`, `'\n'` (quotes included).
    pub const CHAR: u16 = 47;
    /// A string literal `"…"` (any raw/byte flavour, quotes included).
    pub const STRING: u16 = 48;
    /// A numeric literal (suffixes included).
    pub const NUMBER: u16 = 49;
    /// `{`
    pub const LBRACE: u16 = 50;
    /// `}`
    pub const RBRACE: u16 = 51;
    /// `(`
    pub const LPAREN: u16 = 52;
    /// `)`
    pub const RPAREN: u16 = 53;
    /// `[`
    pub const LBRACKET: u16 = 54;
    /// `]`
    pub const RBRACKET: u16 = 55;
    /// `<`
    pub const LT: u16 = 56;
    /// `>`
    pub const GT: u16 = 57;
    /// `,`
    pub const COMMA: u16 = 58;
    /// `;`
    pub const SEMI: u16 = 59;
    /// `:`
    pub const COLON: u16 = 60;
    /// `::`
    pub const COLON2: u16 = 61;
    /// `.`
    pub const DOT: u16 = 62;
    /// `..`
    pub const DOT2: u16 = 63;
    /// `..=`
    pub const DOT2EQ: u16 = 64;
    /// `->`
    pub const RARROW: u16 = 65;
    /// `=>`
    pub const FATARROW: u16 = 66;
    /// `#`
    pub const HASH: u16 = 67;
    /// `!` (never-type / macro bang / not)
    pub const BANG: u16 = 68;
    /// `&`
    pub const AMP: u16 = 69;
    /// `&&`
    pub const AND: u16 = 70;
    /// `|`
    pub const PIPE: u16 = 71;
    /// `||`
    pub const OR: u16 = 72;
    /// `+`
    pub const PLUS: u16 = 73;
    /// `-`
    pub const MINUS: u16 = 74;
    /// `*`
    pub const STAR: u16 = 75;
    /// `/`
    pub const SLASH: u16 = 76;
    /// `%`
    pub const PERCENT: u16 = 77;
    /// `^`
    pub const CARET: u16 = 78;
    /// `~`? — reserved; v0 keeps `!` for not.
    pub const TILDE: u16 = 79;
    /// `@`
    pub const AT: u16 = 80;
    /// `_` wildcard (also part of names — a bare `_` lexes here).
    pub const UNDERSCORE: u16 = 81;
    /// `==`
    pub const EQ: u16 = 82;
    /// `!=`
    pub const NEQ: u16 = 83;
    /// `<=`
    pub const LE: u16 = 84;
    /// `>=`
    pub const GE: u16 = 85;
    /// `<`-as-comparison shares [`LT`]; `>` shares [`GT`].
    pub const ASSIGN: u16 = 86;
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
    /// `^=`
    pub const CARETEQ: u16 = 92;
    /// `&=`
    pub const AMP_EQ: u16 = 93;
    /// `|=`
    pub const PIPE_EQ: u16 = 94;
    /// `<<=`
    pub const LSHIFT_EQ: u16 = 95;
    /// `>>=`
    pub const SHIFT_EQ: u16 = 96;
    /// `<<`
    pub const LSHIFT: u16 = 97;
    /// `>>`
    pub const SHIFT: u16 = 98;
    /// `?`
    pub const QUESTION: u16 = 99;
    /// `$`
    pub const DOLLAR: u16 = 100;
    /// Whitespace trivia.
    pub const WHITESPACE: u16 = 101;
    /// `// …`, `/// …`, `//! …` line-comment trivia.
    pub const COMMENT: u16 = 102;
    /// `/* … */` block-comment trivia.
    pub const BLOCK_COMMENT: u16 = 103;
}

/// Implicit module-root kind for the green tree — pack-local sentinel.
pub const ROOT_KIND: u16 = u16::MAX - 5;

/// Parse Rust-subset source into a green tree (the full integration:
/// `parse_events` → `process` → `drive_sink` → `TreeBuilder`).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *Rust* is recovered, not an error.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;
