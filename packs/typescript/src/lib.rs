//! # packs-typescript — the TypeScript grammar pack (issue 027, documented subset)
//!
//! Reference parser for a documented TypeScript subset on the
//! runtime-core event protocol.
//!
//! **The subset** (everything else errors and recovers):
//! - modules: `import … from "…"` (default + named braces), `export <decl>`;
//! - declarations: `interface` (members: props/methods, `?` optional),
//!   `type` aliases, `class` (fields with modifiers
//!   `readonly`/`private`/`public`/`static`, methods), `enum` (members
//!   with optional initializers), `function` with optional (`?`),
//!   default (`=`) and rest (`...`) parameters;
//! - statements: `const`/`let`/`var` with type annotations,
//!   expression/assignment, `if`/`else`, `while`, `for…of`, `return`
//!   (restricted ASI), blocks;
//! - types: primaries (names + builtins), generics `Type<A, B>`, arrays
//!   `T[]`, unions `A | B`, object types `{ … }`, function types
//!   `(a: X) => Y`, tuple types `[A, B]`, parenthesized;
//! - expressions: full Pratt surface (`|| &&`, `=== !== < > <= >=`,
//!   `+ - * / % **`, bitwise, unary `! - + ~`/`typeof`? no — unary
//!   `! - + ~` only, `**` right-assoc), calls/attributes/subscripts,
//!   array/object literals, arrow functions, template literals with
//!   nested `${}` interpolation;
//! - **ASI**: statements end at `;`, at a newline, or at `}`/EOF;
//!   `return` obeys the restricted production (a newline ends it);
//! - **`>>` splits**: the scanner never merges `>` — nested generics
//!   `Array<Array<number>>` close naturally and the Pratt climb combines
//!   two/three adjacent `>` into shift nodes (`<<` likewise);
//! - `/** … */` doc comments, `/* … */` and `//` comments are trivia.

pub mod lexer;
pub mod reference;

/// Node kinds for the TypeScript grammar (dense).
pub mod kinds {
    /// `import … from "…"`.
    pub const IMPORT_DECL: u16 = 0;
    /// `export <declaration>`.
    pub const EXPORT_DECL: u16 = 1;
    /// `interface Name { … }`.
    pub const INTERFACE_DECL: u16 = 2;
    /// `type Name = …`.
    pub const TYPE_ALIAS: u16 = 3;
    /// `class Name { … }`.
    pub const CLASS_DECL: u16 = 4;
    /// `enum Name { … }`.
    pub const ENUM_DECL: u16 = 5;
    /// `function name(params): T { … }`.
    pub const FUNC_DECL: u16 = 6;
    /// `const/let/var x: T = v`.
    pub const VAR_DECL: u16 = 7;
    /// Function/constructor parameter list.
    pub const PARAM_LIST: u16 = 8;
    /// One parameter (modifiers, name, `?`, default).
    pub const PARAM: u16 = 9;
    /// `: Type` annotation.
    pub const TYPE_ANNOTATION: u16 = 10;
    /// A type node (nested unions/generics/…).
    pub const TYPE_NODE: u16 = 11;
    /// `{ … }` block.
    pub const BLOCK: u16 = 12;
    /// `if (c) … else …`.
    pub const IF_STMT: u16 = 13;
    /// `else` clause.
    pub const ELSE_CLAUSE: u16 = 14;
    /// `while (c) …`.
    pub const WHILE_STMT: u16 = 15;
    /// `for (x of it) …`.
    pub const FOR_STMT: u16 = 16;
    /// `return [expr]`.
    pub const RETURN_STMT: u16 = 17;
    /// An expression statement.
    pub const EXPR_STMT: u16 = 18;
    /// `target = value`.
    pub const ASSIGN: u16 = 19;
    /// A class field (modifiers, name, annotation, initializer).
    pub const CLASS_FIELD: u16 = 20;
    /// A class method.
    pub const METHOD_DECL: u16 = 21;
    /// An enum member.
    pub const ENUM_MEMBER: u16 = 22;
    /// An interface member (prop or method signature).
    pub const INTERFACE_MEMBER: u16 = 23;
    /// Binary operation.
    pub const BIN_EXPR: u16 = 30;
    /// Unary operation.
    pub const UNARY_EXPR: u16 = 31;
    /// Call.
    pub const CALL: u16 = 32;
    /// Attribute access.
    pub const ATTR: u16 = 33;
    /// Subscript.
    pub const SUBSCRIPT: u16 = 34;
    /// Array literal.
    pub const ARRAY_LIT: u16 = 35;
    /// Object literal.
    pub const OBJECT_LIT: u16 = 36;
    /// Arrow function.
    pub const ARROW_FUNC: u16 = 37;
    /// Template literal.
    pub const TEMPLATE_EXPR: u16 = 38;
    /// Parenthesized/grouped expression.
    pub const GROUP_EXPR: u16 = 39;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// Identifier.
    pub const NAME: u16 = 50;
    /// Numeric literal.
    pub const NUMBER: u16 = 51;
    /// String literal.
    pub const STRING: u16 = 52;
    /// Backtick opener.
    pub const TEMPLATE_START: u16 = 53;
    /// Literal text inside a template.
    pub const TEMPLATE_TEXT: u16 = 54;
    /// `${` opening an interpolation.
    pub const TEMPLATE_DOLLAR: u16 = 55;
    /// Closing backtick.
    pub const TEMPLATE_END: u16 = 56;

    /// `import`
    pub const KW_IMPORT: u16 = 60;
    /// `from`
    pub const KW_FROM: u16 = 61;
    /// `export`
    pub const KW_EXPORT: u16 = 62;
    /// `default`
    pub const KW_DEFAULT: u16 = 63;
    /// `interface`
    pub const KW_INTERFACE: u16 = 64;
    /// `type`
    pub const KW_TYPE: u16 = 65;
    /// `class`
    pub const KW_CLASS: u16 = 66;
    /// `enum`
    pub const KW_ENUM: u16 = 67;
    /// `function`
    pub const KW_FUNCTION: u16 = 68;
    /// `const`
    pub const KW_CONST: u16 = 69;
    /// `let`
    pub const KW_LET: u16 = 70;
    /// `var`
    pub const KW_VAR: u16 = 71;
    /// `if`
    pub const KW_IF: u16 = 72;
    /// `else`
    pub const KW_ELSE: u16 = 73;
    /// `while`
    pub const KW_WHILE: u16 = 74;
    /// `for`
    pub const KW_FOR: u16 = 75;
    /// `of`
    pub const KW_OF: u16 = 76;
    /// `return`
    pub const KW_RETURN: u16 = 77;
    /// `readonly`
    pub const KW_READONLY: u16 = 78;
    /// `private`
    pub const KW_PRIVATE: u16 = 79;
    /// `public`
    pub const KW_PUBLIC: u16 = 80;
    /// `static`
    pub const KW_STATIC: u16 = 81;
    /// `true`
    pub const KW_TRUE: u16 = 82;
    /// `false`
    pub const KW_FALSE: u16 = 83;
    /// `null`
    pub const KW_NULL: u16 = 84;
    /// `undefined`
    pub const KW_UNDEFINED: u16 = 85;

    /// `{`
    pub const LBRACE: u16 = 90;
    /// `}`
    pub const RBRACE: u16 = 91;
    /// `(`
    pub const LPAREN: u16 = 92;
    /// `)`
    pub const RPAREN: u16 = 93;
    /// `[`
    pub const LBRACK: u16 = 94;
    /// `]`
    pub const RBRACK: u16 = 95;
    /// `;`
    pub const SEMI: u16 = 96;
    /// `,`
    pub const COMMA: u16 = 97;
    /// `.`
    pub const DOT: u16 = 98;
    /// `:`
    pub const COLON: u16 = 99;
    /// `?`
    pub const QUESTION: u16 = 100;
    /// `...`
    pub const ELLIPSIS: u16 = 101;
    /// `=>`
    pub const FAT_ARROW: u16 = 102;
    /// `=`
    pub const EQUALS: u16 = 103;
    /// `+`
    pub const PLUS: u16 = 104;
    /// `-`
    pub const MINUS: u16 = 105;
    /// `*`
    pub const STAR: u16 = 106;
    /// `/`
    pub const SLASH: u16 = 107;
    /// `%`
    pub const PERCENT: u16 = 108;
    /// `**`
    pub const PSTAR: u16 = 109;
    /// `===`
    pub const EQ3: u16 = 110;
    /// `!==`
    pub const NEQ3: u16 = 111;
    /// `<`
    pub const LT: u16 = 112;
    /// `>`
    pub const GT: u16 = 113;
    /// `<=`
    pub const LE: u16 = 114;
    /// `>=`
    pub const GE: u16 = 115;
    /// `&&`
    pub const ANDAND: u16 = 116;
    /// `||`
    pub const OROR: u16 = 117;
    /// `!`
    pub const BANG: u16 = 118;
    /// `&`
    pub const AMP: u16 = 119;
    /// `|`
    pub const PIPE: u16 = 120;
    /// `^`
    pub const CARET: u16 = 121;
    /// `~`
    pub const TILDE: u16 = 122;
    /// `!=`
    pub const NE: u16 = 123;

    /// Whitespace trivia (ADR-008).
    pub const WHITESPACE: u16 = 130;
    /// `// …` / `/* … */` comment trivia.
    pub const COMMENT: u16 = 131;
    /// `/** … */` doc-comment trivia.
    pub const DOC_COMMENT: u16 = 132;
}

/// Implicit document-root kind — pack-local sentinel below the
/// runtime-core sentinels.
pub const ROOT_KIND: u16 = u16::MAX - 7;

/// Parse TypeScript source into a green tree (the full integration).
///
/// # Errors
/// Structural errors propagate as `Err`; syntax errors are recovered.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;
