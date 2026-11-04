//! # packs-python — the Python-subset grammar pack (issue 028)
//!
//! Reference parser for a documented Python subset on the runtime-core
//! event protocol: simple + compound statements, `INDENT`/`DEDENT`
//! scanner suites (CPython-style column stack, tab = 8), Pratt
//! expressions with chaining comparisons, f-strings with nested
//! interpolation expressions, decorators, comprehensions, and `#`
//! comments as trivia.
//!
//! # Documented subset (v0)
//!
//! - **Statements**: expression statements, assignments (chained, aug,
//!   starred targets, annotations `x: T = v`), `return`/`pass`/`break`/
//!   `continue`, `import`/`from … import`, `del`, `global`, `assert`,
//!   `raise … from …`, `if`/`elif`/`else`, `while`/`for` (+ `else`),
//!   `def` (annotations + defaults + `*`/`**`), `async def`,
//!   `class`, `try`/`except [*] [as e]`/`else`/`finally`,
//!   `with item [as t], …` (`async with`), decorators,
//!   `match expr: case pattern [if guard]:` (patterns ride the
//!   expression grammar: literals, names, tuples, `_`, `|`, guards).
//!   Single-line suites after `:`.
//! - **Expressions**: lambda with full parameter lists
//!   (`lambda x, y=2, *a, **kw: e`), walrus `(n := expr)`,
//!   `await expr`, slices `[a:b:c]` and multi-item subscripts
//!   `[i, j::k]`, generator expressions in call position
//!   (`sum(x for x in xs if x)`), plus the existing comprehensions,
//!   f-strings, and Pratt operators.
//! - **Not in the subset** (each parses as an error and recovers):
//!   `yield` (bare), decorators with arguments beyond dotted names.
//! - **Scanner**: `INDENT` token = the indent run of a deeper line;
//!   `DEDENT` = one zero-width token per popped level (also flushed at
//!   EOF); `NEWLINE` = the `\n`/`\r\n` ending a logical line (inside
//!   brackets and on blank/comment-only lines it is trivia); `\`-newline
//!   continuation is trivia; comment-only lines carry the newline inside
//!   the comment trivia record, trailing comments do not.
//! - **f-strings** (PEP 701-flavoured): `FSTRING_START` (prefix+quote),
//!   `FSTRING_MIDDLE` literal chunks (`{{`/`}}` stay literal),
//!   interpolation = `{` expr [`!` conv] [`:` raw spec] `}` with the
//!   expression parsed by the full Pratt grammar (nested f-strings
//!   recurse), `FSTRING_END`. The format spec is raw (no nested
//!   interpolations, documented subset).
//! - **Comparisons** chain inside one `COMPARE` node (`a < b < c`);
//!   `**` is right-associative; everything else is left-associative.
//! - Adjacent string literal tokens are sibling leaves (implicit
//!   concatenation, no wrapper node in v0).
//!
//! Kind numbering (documented mapping, mirroring the JSON/TOML packs):
//! node kinds in [`kinds`], leaf tokens + punctuation in
//! [`token_kinds`] (dense continuation), offending regions via
//! [`runtime_core::ERROR_KIND`], implicit root [`ROOT_KIND`].
//!
//! # Trivia discipline (ADR-008)
//!
//! Whitespace and `#` comments are never token events — they ride
//! [`runtime_core::sink::TriviaRecord`]s (kinds [`token_kinds::WHITESPACE`] and
//! [`token_kinds::COMMENT`]) attached at tree-build time, so
//! `render(parse(x)) == x` byte-for-byte.

pub mod reference;

/// Node kinds for the Python-subset grammar (dense).
pub mod kinds {
    /// `if` / `elif` statement (elif chains nest as sibling nodes).
    pub const IF: u16 = 0;
    /// `else:` clause of `if`/`while`/`for`.
    pub const ELSE: u16 = 1;
    /// `while` statement.
    pub const WHILE: u16 = 2;
    /// `for … in …` statement.
    pub const FOR: u16 = 3;
    /// `def` statement.
    pub const FUNC_DEF: u16 = 4;
    /// `class` statement.
    pub const CLASS_DEF: u16 = 5;
    /// One or more decorators + the decorated `def`/`class`.
    pub const DECORATED: u16 = 6;
    /// One `@` decorator line.
    pub const DECORATOR: u16 = 7;
    /// Parameter list of a `def`.
    pub const PARAMS: u16 = 8;
    /// An indented block (`INDENT` statements `DEDENT`) or single-line
    /// suite.
    pub const SUITE: u16 = 9;
    /// `return` statement.
    pub const RETURN: u16 = 10;
    /// `pass` statement.
    pub const PASS: u16 = 11;
    /// `break` statement.
    pub const BREAK: u16 = 12;
    /// `continue` statement.
    pub const CONTINUE: u16 = 13;
    /// `import` statement.
    pub const IMPORT: u16 = 14;
    /// `from … import` statement.
    pub const FROM_IMPORT: u16 = 15;
    /// One imported name (dotted module + optional `as` alias).
    pub const ALIAS: u16 = 16;
    /// `del` statement.
    pub const DEL: u16 = 17;
    /// `global` statement.
    pub const GLOBAL: u16 = 18;
    /// `assert` statement.
    pub const ASSERT: u16 = 19;
    /// `raise` statement.
    pub const RAISE: u16 = 20;
    /// Assignment (`a = b = c` chains flat inside one node).
    pub const ASSIGN: u16 = 21;
    /// Augmented assignment (`+=` family).
    pub const AUG_ASSIGN: u16 = 22;
    /// Bare expression statement.
    pub const EXPR_STMT: u16 = 23;
    /// Binary operation (op token inside the node).
    pub const BINOP: u16 = 24;
    /// Unary operation (`-`/`+`/`~`/`not`).
    pub const UNARY: u16 = 25;
    /// Chained comparison (`a < b < c` — one node, op tokens inside).
    pub const COMPARE: u16 = 26;
    /// Conditional expression (`x if c else y`).
    pub const CONDITIONAL: u16 = 27;
    /// Call `f(args)`.
    pub const CALL: u16 = 28;
    /// Attribute access `a.b`.
    pub const ATTRIBUTE: u16 = 29;
    /// Subscript `a[i]`.
    pub const SUBSCRIPT: u16 = 30;
    /// Tuple display / target list `(a, b)` or `a, b`.
    pub const TUPLE: u16 = 31;
    /// List display / list comprehension `[…]`.
    pub const LIST: u16 = 32;
    /// Set display / set comprehension `{…}` (no `:` pairs).
    pub const SET: u16 = 33;
    /// Dict display / dict comprehension `{k: v …}`.
    pub const DICT: u16 = 34;
    /// One `key: value` pair inside a dict.
    pub const PAIR: u16 = 35;
    /// One `for` clause (+ `if` guards) of a comprehension.
    pub const COMPREHENSION: u16 = 36;
    /// Generator expression `(e for …)`.
    pub const GENERATOR: u16 = 37;
    /// f-string literal (start/middle/interpolations/end inside).
    pub const FSTRING: u16 = 38;
}

/// Token kinds — dense continuation of [`kinds`].
pub mod token_kinds {
    /// An identifier or keyword (keywords are text-matched by the
    /// parser — the scanner emits `NAME` for both).
    pub const NAME: u16 = 39;
    /// An int/float/imaginary literal (text distinguishes).
    pub const NUMBER: u16 = 40;
    /// A string literal with any prefix except `f` (prefix included).
    pub const STRING: u16 = 41;
    /// f-string opener: the `f` prefix + opening quote(s).
    pub const FSTRING_START: u16 = 42;
    /// f-string literal chunk (escaped braces stay inside).
    pub const FSTRING_MIDDLE: u16 = 43;
    /// f-string closer: the closing quote(s).
    pub const FSTRING_END: u16 = 44;
    /// Line-indent run of a deeper line (`CPython` scanner suite).
    pub const INDENT: u16 = 45;
    /// Zero-width dedent marker, one per popped level.
    pub const DEDENT: u16 = 46;
    /// The `\n` / `\r\n` ending a logical line.
    pub const NEWLINE: u16 = 47;
    /// `(`
    pub const LPAREN: u16 = 48;
    /// `)`
    pub const RPAREN: u16 = 49;
    /// `[`
    pub const LBRACKET: u16 = 50;
    /// `]`
    pub const RBRACKET: u16 = 51;
    /// `{`
    pub const LBRACE: u16 = 52;
    /// `}`
    pub const RBRACE: u16 = 53;
    /// `:`
    pub const COLON: u16 = 54;
    /// `;`
    pub const SEMI: u16 = 55;
    /// `,`
    pub const COMMA: u16 = 56;
    /// `.`
    pub const DOT: u16 = 57;
    /// `->`
    pub const RARROW: u16 = 58;
    /// `!` (f-string conversion; `!=` lexes as [`NEQ`]).
    pub const BANG: u16 = 59;
    /// `+`
    pub const PLUS: u16 = 60;
    /// `-`
    pub const MINUS: u16 = 61;
    /// `*`
    pub const STAR: u16 = 62;
    /// `**`
    pub const POWER: u16 = 63;
    /// `/`
    pub const SLASH: u16 = 64;
    /// `//`
    pub const DOUBLE_SLASH: u16 = 65;
    /// `%`
    pub const PERCENT: u16 = 66;
    /// `@` (matrix multiply; decorators use it too)
    pub const AT_SIGN: u16 = 67;
    /// `==`
    pub const EQ: u16 = 68;
    /// `!=`
    pub const NEQ: u16 = 69;
    /// `<=`
    pub const LE: u16 = 70;
    /// `>=`
    pub const GE: u16 = 71;
    /// `<`
    pub const LT: u16 = 72;
    /// `>`
    pub const GT: u16 = 73;
    /// `&`
    pub const AMP: u16 = 74;
    /// `|`
    pub const PIPE: u16 = 75;
    /// `^`
    pub const CARET: u16 = 76;
    /// `~`
    pub const TILDE: u16 = 77;
    /// `<<`
    pub const LSHIFT: u16 = 78;
    /// `>>`
    pub const RSHIFT: u16 = 79;
    /// `=`
    pub const EQUALS: u16 = 80;
    /// `+=`
    pub const PLUSEQ: u16 = 81;
    /// `-=`
    pub const MINUSEQ: u16 = 82;
    /// `*=`
    pub const STAREQ: u16 = 83;
    /// `/=`
    pub const SLASHEQ: u16 = 84;
    /// `//=`
    pub const DOUBLE_SLASHEQ: u16 = 85;
    /// `%=`
    pub const PERCENTEQ: u16 = 86;
    /// `**=`
    pub const POWEREQ: u16 = 87;
    /// `@=`
    pub const AT_SIGN_EQ: u16 = 88;
    /// `<<=`
    pub const LSHIFT_EQ: u16 = 89;
    /// `>>=`
    pub const RSHIFT_EQ: u16 = 90;
    /// `&=`
    pub const AMP_EQ: u16 = 91;
    /// `|=`
    pub const PIPE_EQ: u16 = 92;
    /// `^=`
    pub const CARET_EQ: u16 = 93;
    /// Whitespace trivia: never a token event (ADR-008).
    pub const WHITESPACE: u16 = 94;
    /// `# …` comment trivia: never a token event (ADR-008).
    pub const COMMENT: u16 = 95;
    /// `:=` walrus (expression-level assignment).
    pub const WALRUS: u16 = 96;
}

/// Node kinds — dense continuation of the token ids ([`crate::kinds`]
/// ends at 38, tokens at 96; these continue the dense sequence).
pub mod node_kinds_ext {
    /// `try: … except … [else …] [finally: …]`.
    pub const TRY: u16 = 97;
    /// One `except [type [as name]]:` handler.
    pub const EXCEPT: u16 = 98;
    /// `with item [as target], …: suite`.
    pub const WITH: u16 = 99;
    /// One with-item: `expr [as target]`.
    pub const WITH_ITEM: u16 = 100;
    /// `match expr: case …` (3.10 structural pattern match).
    pub const MATCH: u16 = 101;
    /// One `case pattern [if guard]:` arm.
    pub const CASE: u16 = 102;
    /// A subscript slice `[lower:upper:step]`.
    pub const SLICE: u16 = 103;
    /// `lambda params: expr`.
    pub const LAMBDA: u16 = 104;
}

/// Implicit module-root kind for the green tree — pack-local sentinel,
/// numbered below the runtime-core sentinels so the pack's dense
/// `0..=95` ids stay collision-free.
pub const ROOT_KIND: u16 = u16::MAX - 5;

/// Parse Python-subset source into a green tree (the full integration:
/// `parse_events` → `process` → `drive_sink` → `TreeBuilder`).
///
/// # Errors
/// Structural errors (a malformed event stream) propagate as `Err`;
/// malformed *Python* is recovered, not an error.
pub fn parse(source: &str) -> Result<std::sync::Arc<runtime_core::tree::GreenNode>, String> {
    reference::parse(source)
}

#[cfg(test)]
mod tests;
