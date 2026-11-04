//! # packs-rust — the Rust grammar pack (issue 033, documented subset)
//!
//! Reference parser for a documented Rust subset on the runtime-core
//! event protocol.
//!
//! **The subset** (everything else errors and recovers):
//! - items: `fn` (params, return type, generics, where clause),
//!   `struct` (unit / tuple / braced), `enum`, `impl` (inherent and
//!   trait), `trait`, `use` (nested brace trees, `as`, `*`), `mod`
//!   (decl and inline), `const`/`static`, `type` aliases, `pub` (with
//!   `(crate)`-style visibility as bare tokens);
//! - expressions: full Pratt surface — `||` `&&`, comparisons, `| ^ &
//!   << >> + - * / %`, unary `- ! * & &mut`, `as` casts, assignment
//!   (`= += -= *= /= %=` right-assoc), ranges `.. ..=` (infix and
//!   prefix), calls, field/method access, indexing, paths
//!   (`a::b::c`, turbofish `::<T>()`), closures (`move |x| e`),
//!   `if`/`else`/`match`/`loop`/`while`/`for`/`unsafe`/block
//!   expressions, arrays (`[a, b]` and `[0; n]`), tuple groups;
//! - patterns: `_`, bindings (`mut`/`ref`), literals, `&pat`,
//!   tuples, path patterns (tuple and struct forms), or-patterns
//!   (flat siblings), match guards;
//! - types: paths with generic args, `&'a mut T`, `[T; n]`,
//!   tuples, `fn(A) -> B` pointers;
//! - `#[attr(…)]` nodes with balanced token trees, macros as opaque
//!   balanced token trees (`m!(…)`, `m![…]`, `m!{…}`), `///`-doc
//!   comments as trivia, nested `/* */` block comments.
//!
//! **Boundaries** (documented): no struct literals in expression
//! position, no `?`/`await`/`async`/`dyn`, no raw strings or byte
//! strings, no associated-type bindings (`<T as Trait>::Assoc`), no
//! macro-definition bodies, comparisons parse as left-associated.

pub mod lexer;
pub mod reference;

/// Node kinds for the Rust grammar (dense).
pub mod kinds {
    /// `fn name(…) … { … }`.
    pub const FN_DEF: u16 = 0;
    /// `struct S { … }`.
    pub const STRUCT_DEF: u16 = 1;
    /// `enum E { … }`.
    pub const ENUM_DEF: u16 = 2;
    /// One enum variant (optional tuple/braced body).
    pub const ENUM_VARIANT: u16 = 3;
    /// `impl … { … }` (inherent or trait).
    pub const IMPL_DEF: u16 = 4;
    /// `trait T … { … }`.
    pub const TRAIT_DEF: u16 = 5;
    /// `use …;` declaration.
    pub const USE_DECL: u16 = 6;
    /// A (possibly nested) use-tree brace group.
    pub const USE_TREE: u16 = 7;
    /// `mod m;` / `mod m { … }`.
    pub const MOD_DECL: u16 = 8;
    /// `const`/`static` item.
    pub const CONST_DEF: u16 = 9;
    /// `type A = …;`.
    pub const TYPE_ALIAS: u16 = 10;
    /// One struct field `name: Type`.
    pub const FIELD: u16 = 11;
    /// `struct S(A, B);` body.
    pub const TUPLE_STRUCT_BODY: u16 = 12;
    /// `let PAT (: T)? (= e)?;`.
    pub const LET_STMT: u16 = 13;
    /// An expression statement.
    pub const EXPR_STMT: u16 = 14;
    /// `(a: T, b: U)` parameter list.
    pub const PARAM_LIST: u16 = 15;
    /// `<T: B, 'a>` generic parameter list.
    pub const TYPE_PARAMS: u16 = 16;
    /// `where T: B, …`.
    pub const WHERE_CLAUSE: u16 = 17;
    /// `::<T, 'a>` generic argument list.
    pub const GENERIC_ARGS: u16 = 18;
    /// `{ stmts / items }`.
    pub const BLOCK: u16 = 19;
    /// `if … { … } (else …)?`.
    pub const IF_EXPR: u16 = 20;
    /// `match e { … }`.
    pub const MATCH_EXPR: u16 = 21;
    /// One match arm `pat => expr`.
    pub const MATCH_ARM: u16 = 22;
    /// `if guard` inside an arm.
    pub const GUARD: u16 = 23;
    /// `loop`/`while`/`for` loops.
    pub const LOOP_EXPR: u16 = 24;
    /// Call suffix `f(…)`.
    pub const CALL: u16 = 25;
    /// Field suffix `.name`.
    pub const FIELD_EXPR: u16 = 26;
    /// Index suffix `[e]`.
    pub const INDEX_EXPR: u16 = 27;
    /// `e as T`.
    pub const CAST_EXPR: u16 = 28;
    /// Binary operation (op kind in the child token).
    pub const BIN_EXPR: u16 = 29;
    /// Unary `- ! * & &mut`.
    pub const UNARY_EXPR: u16 = 30;
    /// `a..b` / `..e` range.
    pub const RANGE_EXPR: u16 = 31;
    /// `move |x| e` / `|x| e` closure.
    pub const CLOSURE_EXPR: u16 = 32;
    /// `(a, b)` tuple / paren group.
    pub const TUPLE_EXPR: u16 = 33;
    /// `[a, b]` / `[0; n]` array.
    pub const ARRAY_EXPR: u16 = 34;
    /// `a::b::c` path expression.
    pub const PATH_EXPR: u16 = 35;
    /// `m!(…)` macro invocation.
    pub const MACRO_CALL: u16 = 36;
    /// One balanced `(...)`/`[…]`/`{…}` group.
    pub const TOKEN_TREE: u16 = 37;
    /// `#[derive(…)]` attribute.
    pub const ATTR: u16 = 38;
    /// `(p, q)` tuple pattern.
    pub const TUPLE_PAT: u16 = 39;
    /// `S { x, y: 1 }` struct pattern.
    pub const STRUCT_PAT: u16 = 40;
    /// `Some(x)` / `None` / unit-struct path pattern.
    pub const PATH_PAT: u16 = 41;
    /// `&pat` / `&mut pat`.
    pub const REF_PAT: u16 = 42;
    /// `(A, B)` tuple type.
    pub const TUPLE_TYPE: u16 = 43;
    /// `[T; n]` array type.
    pub const ARRAY_TYPE: u16 = 44;
    /// `&'a mut T` reference type.
    pub const REF_TYPE: u16 = 45;
    /// `fn(A) -> B` function-pointer type.
    pub const FN_PTR_TYPE: u16 = 46;
    /// `return (e)?`.
    pub const RETURN_EXPR: u16 = 47;
    /// `break ('a)? (e)?`.
    pub const BREAK_EXPR: u16 = 48;
    /// `continue ('a)?`.
    pub const CONTINUE_EXPR: u16 = 49;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// Identifier (keywords lex separately; `_` lexes here).
    pub const IDENT: u16 = 50;
    /// Integer literal (with optional suffix).
    pub const INT_LIT: u16 = 51;
    /// Float literal (`1.5`, `1e3`, suffix included).
    pub const FLOAT_LIT: u16 = 52;
    /// `"…"` string with escapes.
    pub const STRING_LIT: u16 = 53;
    /// `'x'` / `'\n'` char literal.
    pub const CHAR_LIT: u16 = 54;
    /// `'a` lifetime (no closing quote).
    pub const LIFETIME: u16 = 55;
    /// `+`
    pub const PLUS: u16 = 56;
    /// `-`
    pub const MINUS: u16 = 57;
    /// `*`
    pub const STAR: u16 = 58;
    /// `/`
    pub const SLASH: u16 = 59;
    /// `%`
    pub const PERCENT: u16 = 60;
    /// `^`
    pub const CARET: u16 = 61;
    /// `!`
    pub const BANG: u16 = 62;
    /// `&`
    pub const AMP: u16 = 63;
    /// `|`
    pub const PIPE: u16 = 64;
    /// `<<`
    pub const SHL: u16 = 65;
    /// `>>`
    pub const SHR: u16 = 66;
    /// `&&`
    pub const ANDAND: u16 = 67;
    /// `||`
    pub const OROR: u16 = 68;
    /// `==`
    pub const EQ: u16 = 69;
    /// `!=`
    pub const NE: u16 = 70;
    /// `<`
    pub const LT: u16 = 71;
    /// `>`
    pub const GT: u16 = 72;
    /// `<=`
    pub const LE: u16 = 73;
    /// `>=`
    pub const GE: u16 = 74;
    /// `=`
    pub const ASSIGN: u16 = 75;
    /// `+=`
    pub const PLUSEQ: u16 = 76;
    /// `-=`
    pub const MINUSEQ: u16 = 77;
    /// `*=`
    pub const STAREQ: u16 = 78;
    /// `/=`
    pub const SLASHEQ: u16 = 79;
    /// `%=`
    pub const PERCENTEQ: u16 = 80;
    /// `.`
    pub const DOT: u16 = 81;
    /// `..`
    pub const DOTDOT: u16 = 82;
    /// `..=`
    pub const DOTDOTEQ: u16 = 83;
    /// `,`
    pub const COMMA: u16 = 84;
    /// `;`
    pub const SEMI: u16 = 85;
    /// `:`
    pub const COLON: u16 = 86;
    /// `::`
    pub const COLONCOLON: u16 = 87;
    /// `->`
    pub const ARROW: u16 = 88;
    /// `=>`
    pub const FATARROW: u16 = 89;
    /// `#`
    pub const POUND: u16 = 90;
    /// `(`
    pub const LPAREN: u16 = 91;
    /// `)`
    pub const RPAREN: u16 = 92;
    /// `[`
    pub const LBRACK: u16 = 93;
    /// `]`
    pub const RBRACK: u16 = 94;
    /// `{`
    pub const LBRACE: u16 = 95;
    /// `}`
    pub const RBRACE: u16 = 96;
    /// `fn`
    pub const KW_FN: u16 = 97;
    /// `let`
    pub const KW_LET: u16 = 98;
    /// `struct`
    pub const KW_STRUCT: u16 = 99;
    /// `enum`
    pub const KW_ENUM: u16 = 100;
    /// `impl`
    pub const KW_IMPL: u16 = 101;
    /// `trait`
    pub const KW_TRAIT: u16 = 102;
    /// `use`
    pub const KW_USE: u16 = 103;
    /// `mod`
    pub const KW_MOD: u16 = 104;
    /// `pub`
    pub const KW_PUB: u16 = 105;
    /// `const`
    pub const KW_CONST: u16 = 106;
    /// `static`
    pub const KW_STATIC: u16 = 107;
    /// `type`
    pub const KW_TYPE: u16 = 108;
    /// `if`
    pub const KW_IF: u16 = 109;
    /// `else`
    pub const KW_ELSE: u16 = 110;
    /// `match`
    pub const KW_MATCH: u16 = 111;
    /// `while`
    pub const KW_WHILE: u16 = 112;
    /// `loop`
    pub const KW_LOOP: u16 = 113;
    /// `for`
    pub const KW_FOR: u16 = 114;
    /// `in`
    pub const KW_IN: u16 = 115;
    /// `return`
    pub const KW_RETURN: u16 = 116;
    /// `break`
    pub const KW_BREAK: u16 = 117;
    /// `continue`
    pub const KW_CONTINUE: u16 = 118;
    /// `true`
    pub const KW_TRUE: u16 = 119;
    /// `false`
    pub const KW_FALSE: u16 = 120;
    /// `as`
    pub const KW_AS: u16 = 121;
    /// `where`
    pub const KW_WHERE: u16 = 122;
    /// `mut`
    pub const KW_MUT: u16 = 123;
    /// `self`
    pub const KW_SELF: u16 = 124;
    /// `crate`
    pub const KW_CRATE: u16 = 125;
    /// `super`
    pub const KW_SUPER: u16 = 126;
    /// `move`
    pub const KW_MOVE: u16 = 127;
    /// `ref`
    pub const KW_REF: u16 = 128;
    /// `unsafe`
    pub const KW_UNSAFE: u16 = 129;
    /// Whitespace trivia (never a token event, ADR-008).
    pub const WHITESPACE: u16 = 130;
    /// `// …` line-comment trivia.
    pub const COMMENT: u16 = 131;
    /// `///` / `//!` / `/**` doc-comment trivia (issue 033).
    pub const DOC_COMMENT: u16 = 132;
}

/// Implicit document-root kind — pack-local sentinel below the
/// runtime-core sentinels.
pub const ROOT_KIND: u16 = u16::MAX - 8;

/// Parse Rust source into a green tree (the full integration).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;
