//! # Descent backend (issue 045) — `.gir` → compiling Rust parser crate
//!
//! The keystone of the compiler loop (roadmap §0, §6.1 pass 12, §12.3 P0
//! "compiling crate" row): take a [`descent_lower::Lowered`] program plus
//! a compiled [`lexgen::Scanner`] and emit a **standalone, compiling,
//! safe-Rust parser crate** that reproduces [`descent::engine`] output
//! byte-for-byte — same `Run` (events, token ranges, trivia, error count,
//! mode crossings) on every input — while reusing the runtime substrates
//! (`runtime-core` events/trees, `lexgen` scanner) instead of forking them.
//!
//! ## What it emits
//!
//! One file per concern, matching the packs-json API shape (issue 019):
//!
//! - `Cargo.toml` — `forbid(unsafe_code)`, `missing_docs`, `clippy::pedantic`
//!   clean; depends on `runtime-core` + `lexgen` (never a second lexer).
//! - `src/lib.rs` — `parse`/`parse_events`/`green_tree` (the packs-json shape).
//! - `src/kinds.rs` — one `pub const` per generated kind (declaration order).
//! - `src/scanner.rs` — the lexgen DFA serialized as Rust literals, rebuilt
//!   once via `OnceLock`; `scan` reuses `lexgen::Scanner::scan` exactly.
//! - `src/parser.rs` — one `fn pat_<n>` per pattern node + `fn rule_<id>`
//!   per node rule, each mirroring `descent::Engine::pat`/`rule` exactly,
//!   plus the `Lowered::feed` kind-remap/trivia bridge reproduced in code.
//!
//! ## Determinism (ADR-013)
//!
//! Emission is byte-identical under regeneration: no timestamps, fixed
//! wrapping, sorted maps, `#[rustfmt::skip]` on dense arrays. Two runs
//! over the same `Lowered` produce the same bytes — the determinism gate
//! asserts it.
//!
//! ## Refusals
//!
//! A lowering refusal surfaces as a named [`EmitError`]; the CLI reports
//! them as FAIL diagnostics rather than silently skipping emission.

use std::fmt::Write as _;

use descent::{Pat, PrattDesc};
use descent_lower::Lowered;
use lexgen::Scanner;

/// Why a `Lowered` program cannot be emitted as a compiling crate. Every
/// variant names the offending rule so the CLI can surface a real FAIL
/// diagnostic instead of silently skipping emission.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EmitError {
    /// A pattern shape v0 cannot lower to Rust yet (named, never dropped).
    UnsupportedPattern {
        /// The rule whose body contains the offending pattern.
        rule: String,
        /// A short label for the pattern shape.
        shape: String,
    },
}

impl std::fmt::Display for EmitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedPattern { rule, shape } => {
                write!(
                    f,
                    "rule `{rule}`: pattern `{shape}` has no Rust lowering yet"
                )
            }
        }
    }
}

impl std::error::Error for EmitError {}

// ---------------------------------------------------------------------------
// Crate
// ---------------------------------------------------------------------------

/// One emitted file: path relative to the crate root, contents.
#[derive(Debug, Clone)]
pub struct EmittedFile {
    /// The relative path (`src/parser.rs`, `Cargo.toml`, …).
    pub path: String,
    /// The file contents.
    pub contents: String,
}

/// The emitted crate: every file plus the crate name.
#[derive(Debug, Clone)]
pub struct EmittedCrate {
    /// The crate name (`syntact_json_generated`, sanitized).
    pub name: String,
    /// The grammar name (for the banner).
    pub grammar_name: String,
    /// The grammar version (for the banner).
    pub grammar_version: String,
    /// The files, in dependency order: Cargo.toml, src/lib.rs,
    /// src/kinds.rs, src/scanner.rs, src/parser.rs.
    pub files: Vec<EmittedFile>,
}

impl EmittedCrate {
    /// The file at `path` (used by tests).
    #[must_use]
    pub fn file(&self, path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.contents.as_str())
    }
}

/// Emit a compiling Rust parser crate from a lowered program plus its
/// compiled scanner.
///
/// # Errors
/// [`EmitError`] for any pattern shape v0 cannot lower to Rust.
pub fn emit_crate(
    lowered: &Lowered,
    scanner: &Scanner,
    grammar_name: &str,
    grammar_version: &str,
) -> Result<EmittedCrate, EmitError> {
    let name = sanitize_crate_name(grammar_name);
    let parser_contents = parser_rs(lowered, grammar_name, grammar_version)?;
    let files = vec![
        EmittedFile {
            path: "Cargo.toml".to_owned(),
            contents: cargo_toml(grammar_name, grammar_version),
        },
        EmittedFile {
            path: "src/lib.rs".to_owned(),
            contents: lib_rs(grammar_name, grammar_version, lowered),
        },
        EmittedFile {
            path: "src/kinds.rs".to_owned(),
            contents: kinds_rs(lowered, grammar_name, grammar_version),
        },
        EmittedFile {
            path: "src/scanner.rs".to_owned(),
            contents: scanner_rs(scanner, lowered, grammar_name, grammar_version),
        },
        EmittedFile {
            path: "src/parser.rs".to_owned(),
            contents: parser_contents,
        },
    ];
    Ok(EmittedCrate {
        name,
        grammar_name: grammar_name.to_owned(),
        grammar_version: grammar_version.to_owned(),
        files,
    })
}

/// Sanitize a grammar name into a valid crate name suffix.
fn sanitize_crate_name(grammar_name: &str) -> String {
    let stem: String = grammar_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("syntact_{stem}_generated")
}

/// The banner shared by every emitted source file (deterministic).
fn banner(grammar_name: &str, grammar_version: &str) -> String {
    let mut s = String::new();
    s.push_str("// Generated by syntact — DO NOT EDIT.\n");
    let _ = writeln!(s, "// Source grammar: {grammar_name} v{grammar_version}");
    s.push_str("// Regeneration is diff-stable (ADR-013): identical grammars\n");
    s.push_str("// produce byte-identical files.\n");
    s
}

/// The `Cargo.toml`: depends on runtime-core + lexgen (reused substrates),
/// with the same lint posture as hand-written crates.
fn cargo_toml(grammar_name: &str, grammar_version: &str) -> String {
    let mut s = String::new();
    s.push_str("# Generated by syntact — DO NOT EDIT.\n");
    let _ = writeln!(s, "# Source grammar: {grammar_name} v{grammar_version}");
    s.push_str("# Regeneration is diff-stable (ADR-013).\n\n");
    s.push_str("[package]\n");
    s.push_str("name = \"syntact_generated\"\n");
    s.push_str("description = \"Generated parser crate (syntact, issue 045)\"\n");
    s.push_str("version = \"0.1.0\"\n");
    s.push_str("edition = \"2024\"\n");
    s.push_str("rust-version = \"1.85\"\n");
    s.push_str("license = \"MIT OR Apache-2.0\"\n");
    s.push_str("publish = false\n\n");
    // Standalone: an empty [workspace] table keeps cargo from pulling this
    // generated crate into the parent workspace when emitted under
    // `target/codegen/<name>/` (issue 045).
    s.push_str("[workspace]\n\n");
    s.push_str("[dependencies]\n");
    s.push_str("lexgen = { path = \"../../../crates/lexgen\" }\n");
    s.push_str("runtime-core = { path = \"../../../crates/runtime-core\" }\n\n");
    s.push_str("[lints.rust]\n");
    s.push_str("missing_docs = \"warn\"\n");
    s.push_str("unsafe_code = \"forbid\"\n\n");
    s.push_str("[lints.clippy]\n");
    s.push_str("pedantic = { level = \"warn\", priority = -1 }\n");
    s.push_str("module_name_repetitions = \"allow\"\n");
    s.push_str("missing_errors_doc = \"allow\"\n");
    s.push_str("missing_panics_doc = \"allow\"\n");
    s.push_str("cast_precision_loss = \"allow\"\n");
    s.push_str("cast_possible_truncation = \"allow\"\n");
    s.push_str("struct_excessive_bools = \"allow\"\n");
    s.push_str("too_many_lines = \"allow\"\n");
    s.push_str("type_complexity = \"allow\"\n");
    s.push_str("only_used_in_recursion = \"allow\"\n");
    // Machine-generated table literals (DFA transitions/accepts) are dense
    // u32 sentinels (0xFFFF_FFFF = None); separators would obscure the
    // row structure. Allowlist with rationale (issue 045, ADR-013).
    s.push_str("unreadable_literal = \"allow\"\n");
    s
}

/// The `src/lib.rs`: the packs-json API shape (`parse`, `parse_events`,
/// `green_tree`), the root-kind constant, the diagnostics side table.
fn lib_rs(grammar_name: &str, grammar_version: &str, lowered: &Lowered) -> String {
    let banner = banner(grammar_name, grammar_version);
    let root_kind = u16::MAX - 3;
    let bad_kind = lowered.bad_kind;
    let mut s = String::new();
    s.push_str(&banner);
    s.push_str("//! # syntact_generated — generated parser crate (issue 045)\n");
    s.push_str("//!\n");
    s.push_str("//! Standalone, compiling, safe-Rust parser reproduced from a `.gir`\n");
    s.push_str("//! grammar by the `syntact build` codegen-rust descent backend. The\n");
    s.push_str("//! emitted parser reproduces `descent::engine` output byte-for-byte\n");
    s.push_str("//! (same events, token ranges, trivia, error count) while reusing the\n");
    s.push_str("//! `runtime-core` event/tree substrates and the `lexgen` scanner — no\n");
    s.push_str("//! second lexer engine, no forked runtime.\n");
    s.push_str("//!\n");
    s.push_str("//! # API (packs-json shape, issue 019)\n");
    s.push_str("//!\n");
    s.push_str("//! - [`parse`] — the green tree (lossless on any input).\n");
    s.push_str("//! - [`parse_events`] — the raw event stream + token ranges + error count.\n");
    s.push_str("//! - [`green_tree`] — wrap a run in the implicit root + drive the tree.\n");
    s.push_str("\n#![forbid(unsafe_code)]\n");
    s.push_str("#![warn(missing_docs)]\n\n");
    s.push_str("pub mod kinds;\n");
    s.push_str("pub mod parser;\n");
    s.push_str("pub mod scanner;\n");
    s.push_str("\nuse std::sync::Arc;\n");
    s.push_str("\nuse runtime_core::events::{self, Event};\n");
    s.push_str("use runtime_core::input::Source;\n");
    s.push_str("use runtime_core::sink::{TriviaRecord, drive_sink};\n");
    s.push_str("use runtime_core::tree::{GreenNode, NodeCache, TreeBuilder};\n");
    s.push_str("\n/// Implicit document-root kind (mirrors `packs_json::reference::ROOT_KIND`).\n");
    let _ = writeln!(s, "pub const ROOT_KIND: u16 = {root_kind};");
    let _ = writeln!(
        s,
        "/// The unmatched-character sentinel kind (mirrors `Lowered::bad_kind`)."
    );
    let _ = writeln!(s, "pub const BAD_KIND: u16 = {bad_kind};");
    s.push_str("/// The diagnostic side-table index meaning \"syntax error\" (the only\n");
    s.push_str("/// message the v0 generated parser emits).\n");
    s.push_str("pub const DIAG_SYNTAX: u32 = 0;\n\n");
    s.push_str("/// Resolve a diagnostic side-table index to its message text.\n");
    s.push_str("#[must_use]\n");
    s.push_str("pub fn error_message(diag: u32) -> &'static str {\n");
    s.push_str("    match diag {\n");
    s.push_str("        DIAG_SYNTAX => \"syntax error\",\n");
    s.push_str("        _ => \"unknown diagnostic\",\n");
    s.push_str("    }\n");
    s.push_str("}\n\n");
    s.push_str("/// Parse source into the raw event stream: `(events, token_ranges,\n");
    s.push_str("/// error_count)` — the engine's `Run` fields consumers read.\n");
    s.push_str("///\n");
    s.push_str("/// Lossless on any input (recovery wraps every unplaceable token in an\n");
    s.push_str("/// ERROR node).\n");
    s.push_str("#[must_use]\n");
    s.push_str("pub fn parse_events(source: &str) -> (Vec<Event>, Vec<(u16, std::ops::Range<u32>)>, usize) {\n");
    s.push_str("    let mut engine = parser::Engine::new(source);\n");
    s.push_str("    engine.parse_document();\n");
    s.push_str("    (engine.events, engine.tokens, engine.errors)\n");
    s.push_str("}\n\n");
    s.push_str("/// Parse source into a green tree (the packs-json shape).\n");
    s.push_str("///\n");
    s.push_str("/// # Errors\n");
    s.push_str("/// Structural errors (a malformed event stream) propagate as `Err`;\n");
    s.push_str("/// malformed input is recovered, not an error.\n");
    s.push_str("pub fn parse(source: &str) -> Result<Arc<GreenNode>, String> {\n");
    s.push_str("    let run = parse_events(source);\n");
    s.push_str("    green_tree(&run.0, &run.1, source)\n");
    s.push_str("}\n\n");
    s.push_str("/// Wrap an event stream in one implicit root and drive the tree\n");
    s.push_str("/// integration — mirrors `descent::green_tree`.\n");
    s.push_str("///\n");
    s.push_str("/// # Errors\n");
    s.push_str("/// Structural errors (a malformed event stream) propagate as `Err`.\n");
    s.push_str("pub fn green_tree(\n");
    s.push_str("    events: &[Event],\n");
    s.push_str("    tokens: &[(u16, std::ops::Range<u32>)],\n");
    s.push_str("    source: &str,\n");
    s.push_str(") -> Result<Arc<GreenNode>, String> {\n");
    s.push_str("    let trivia_kind = scanner::TRIVIA_KIND.expect(\"grammar declares trivia\");\n");
    s.push_str("    let mut wrapped = Vec::with_capacity(events.len() + 2);\n");
    s.push_str("    wrapped.push(Event::Start { kind: ROOT_KIND, forward_parent: None });\n");
    s.push_str("    wrapped.extend(events.iter().copied());\n");
    s.push_str("    wrapped.push(Event::Finish);\n");
    s.push_str("    let output = events::process(&wrapped)?;\n");
    s.push_str("    let input = Source::from_str_unchecked(source);\n");
    s.push_str("    let cache = Arc::new(NodeCache::new());\n");
    s.push_str("    let mut builder = TreeBuilder::with_source(cache, &input);\n");
    s.push_str("    // Re-derive trivia records from the token gaps (ADR-008).\n");
    s.push_str("    let mut last_end = 0_u32;\n");
    s.push_str("    let mut records = Vec::with_capacity(tokens.len());\n");
    s.push_str("    for (_, range) in tokens {\n");
    s.push_str("        if range.start > last_end {\n");
    s.push_str("            let text = &source[usize::try_from(last_end).unwrap_or(0)..usize::try_from(range.start).unwrap_or(source.len())];\n");
    s.push_str("            records.push(TriviaRecord {\n");
    s.push_str("                kind: trivia_kind,\n");
    s.push_str("                range: last_end..range.start,\n");
    s.push_str("                trailing: !text.contains('\\n'),\n");
    s.push_str("            });\n");
    s.push_str("        }\n");
    s.push_str("        last_end = range.end;\n");
    s.push_str("    }\n");
    s.push_str("    if usize::try_from(last_end).unwrap_or(0) < source.len() {\n");
    s.push_str("        records.push(TriviaRecord {\n");
    s.push_str("            kind: trivia_kind,\n");
    s.push_str("            range: last_end..u32::try_from(source.len()).unwrap_or(u32::MAX),\n");
    s.push_str("            trailing: !source[usize::try_from(last_end).unwrap_or(0)..].contains('\\n'),\n");
    s.push_str("        });\n");
    s.push_str("    }\n");
    s.push_str("    builder.attach_trivia_records(&records);\n");
    s.push_str("    drive_sink(\n");
    s.push_str("        &output,\n");
    s.push_str("        &tokens.iter().map(|(_, r)| r.clone()).collect::<Vec<_>>(),\n");
    s.push_str("        &mut builder,\n");
    s.push_str("    )?;\n");
    s.push_str("    builder.finish()\n");
    s.push_str("}\n");
    s
}

/// The `src/kinds.rs`: one `pub const` per generated kind in declaration
/// order (node rules, node tokens, plain tokens, trivia, bad sentinel).
fn kinds_rs(lowered: &Lowered, grammar_name: &str, grammar_version: &str) -> String {
    let banner = banner(grammar_name, grammar_version);
    let mut s = String::new();
    s.push_str(&banner);
    s.push_str("//! Generated kind constants (issue 045): the dense `u16` kind space\n");
    s.push_str("//! — `node=\"true\"` rules, then `node=\"true\"` tokens, plain tokens,\n");
    s.push_str("//! trivia; the unmatched sentinel lands last.\n\n");
    s.push_str("/// Node-rule kinds (declaration order).\n");
    s.push_str("pub mod rules {\n");
    for (idx, kind) in lowered.kind_of_rule.iter().enumerate() {
        if let Some(k) = kind {
            let _ = writeln!(s, "    /// Node rule {idx} (kind {k}).");
            let _ = writeln!(s, "    pub const RULE_{idx}: u16 = {k};");
        }
    }
    s.push_str("}\n\n");
    s.push_str("/// Token kinds (declaration order).\n");
    s.push_str("pub mod tokens {\n");
    for (idx, kind) in lowered.kind_of_token.iter().enumerate() {
        let _ = writeln!(s, "    /// Token {idx} (kind {kind}).");
        let _ = writeln!(s, "    pub const TOKEN_{idx}: u16 = {kind};");
    }
    s.push_str("}\n\n");
    let bad = lowered.bad_kind;
    let _ = writeln!(s, "/// The unmatched-character sentinel kind.");
    let _ = writeln!(s, "pub const BAD_KIND: u16 = {bad};");
    if let Some(t) = lowered.trivia_kind {
        let _ = writeln!(s, "/// The trivia token kind.");
        let _ = writeln!(s, "pub const TRIVIA_KIND: u16 = {t};");
    }
    s
}

/// The `src/scanner.rs`: serialize the DFA as Rust literals + rebuild the
/// scanner once via `OnceLock`, reusing `lexgen::Scanner::scan` exactly.
fn scanner_rs(
    scanner: &Scanner,
    lowered: &Lowered,
    grammar_name: &str,
    grammar_version: &str,
) -> String {
    let banner = banner(grammar_name, grammar_version);
    let dfa = scanner.dfa();
    let trivia_flags: Vec<bool> = (0..scanner.token_count())
        .map(|i| scanner.is_trivia(u16::try_from(i).expect("token count fits u16")))
        .collect();
    let mut s = String::new();
    s.push_str(&banner);
    s.push_str("//! Generated scanner (issue 045): the lexgen DFA serialized as Rust\n");
    s.push_str("//! literals and rebuilt once via `OnceLock`. `scan` reuses\n");
    s.push_str("//! `lexgen::Scanner::scan` exactly — no second lexer engine.\n\n");
    s.push_str("use std::sync::OnceLock;\n\n");
    s.push_str("use lexgen::{CharClasses, Dfa, Lexeme, Scanner};\n\n");
    // Char-class starts.
    let starts = &dfa.classes.starts;
    s.push_str("#[rustfmt::skip]\n");
    let _ = writeln!(s, "static CLASS_STARTS: [u32; {}] = [", starts.len());
    emit_u32_slice(&mut s, starts);
    s.push_str("];\n\n");
    // Accepting states: Option<u16> → encode as u16 (0xFFFF = None).
    s.push_str("#[rustfmt::skip]\n");
    let _ = writeln!(s, "static ACCEPT: [u16; {}] = [", dfa.accept.len());
    let accept_flat: Vec<u32> = dfa
        .accept
        .iter()
        .map(|a| u32::from(a.unwrap_or(0xFFFF)))
        .collect();
    emit_u32_slice(&mut s, &accept_flat);
    s.push_str("];\n\n");
    // Transitions: flat [state*nclasses] of Option<u32> → u32 (0xFFFFFFFF = None).
    let nclasses = dfa.classes.starts.len();
    let nstates = dfa.transitions.len();
    let trans_flat: Vec<u32> = dfa
        .transitions
        .iter()
        .flat_map(|row| row.iter())
        .map(|c| c.unwrap_or(0xFFFF_FFFF))
        .collect();
    s.push_str("#[rustfmt::skip]\n");
    let _ = writeln!(s, "static TRANSITIONS: [u32; {}] = [", trans_flat.len());
    emit_u32_slice(&mut s, &trans_flat);
    s.push_str("];\n\n");
    // Trivia flags.
    s.push_str("#[rustfmt::skip]\n");
    let _ = writeln!(s, "static TRIVIA: [bool; {}] = [", trivia_flags.len());
    s.push_str("    ");
    for (idx, &t) in trivia_flags.iter().enumerate() {
        if idx > 0 {
            s.push(' ');
        }
        let _ = write!(s, "{t},");
        if (idx + 1) % 16 == 0 {
            s.push_str("\n    ");
        }
    }
    s.push_str("\n];\n\n");
    let trivia_kind = lowered.trivia_kind;
    let _ = writeln!(
        s,
        "/// The trivia token kind (from the lowering), for the tree integration."
    );
    let _ = writeln!(
        s,
        "pub static TRIVIA_KIND: Option<u16> = {trivia_kind:?};\n"
    );
    let _ = writeln!(s, "/// The number of DFA states (for the rebuild).");
    let _ = writeln!(s, "pub const DFA_STATES: usize = {nstates};");
    let _ = writeln!(s, "/// The number of char classes (for the rebuild).");
    let _ = writeln!(s, "pub const DFA_CLASSES: usize = {nclasses};\n");
    // The kind-remap table (token-kind → generated-kind).
    let kind_of = &lowered.kind_of_token;
    s.push_str("#[rustfmt::skip]\n");
    let _ = writeln!(s, "static KIND_OF_TOKEN: [u16; {}] = [", kind_of.len());
    let kind_flat: Vec<u32> = kind_of.iter().map(|&k| u32::from(k)).collect();
    emit_u32_slice(&mut s, &kind_flat);
    s.push_str("];\n\n");
    let bad_kind = lowered.bad_kind;
    // The rebuild fn.
    s.push_str("/// The rebuilt scanner (constructed once, reused for every parse).\n");
    s.push_str("#[must_use]\n");
    s.push_str("pub fn scanner() -> &'static Scanner {\n");
    s.push_str("    static SCANNER: OnceLock<Scanner> = OnceLock::new();\n");
    s.push_str("    SCANNER.get_or_init(|| {\n");
    s.push_str("        let classes = CharClasses { starts: CLASS_STARTS.to_vec() };\n");
    s.push_str("        let transitions: Vec<Vec<Option<u32>>> = (0..DFA_STATES)\n");
    s.push_str("            .map(|state| {\n");
    s.push_str("                (0..DFA_CLASSES)\n");
    s.push_str("                    .map(|class| {\n");
    s.push_str("                        let cell = TRANSITIONS[state * DFA_CLASSES + class];\n");
    s.push_str("                        (cell != 0xFFFF_FFFF).then_some(cell)\n");
    s.push_str("                    })\n");
    s.push_str("                    .collect()\n");
    s.push_str("            })\n");
    s.push_str("            .collect();\n");
    s.push_str("        let accept: Vec<Option<u16>> = ACCEPT\n");
    s.push_str("            .iter()\n");
    s.push_str("            .map(|&v| (v != 0xFFFF).then_some(v))\n");
    s.push_str("            .collect();\n");
    s.push_str("        let dfa = Dfa {\n");
    s.push_str("            transitions,\n");
    s.push_str("            accept,\n");
    s.push_str("            start: 0,\n");
    s.push_str("            classes,\n");
    s.push_str("        };\n");
    s.push_str("        Scanner::from_parts(dfa, TRIVIA.to_vec())\n");
    s.push_str("    })\n");
    s.push_str("}\n\n");
    // The feed bridge: `Lowered::feed` reproduced in emitted code.
    s.push_str("/// Bridge the scanner to the engine's clean-feed contract\n");
    s.push_str("/// (reproduces `Lowered::feed`): scanner kinds remap into the\n");
    s.push_str("/// generated kind space, unmatched characters become the bad\n");
    s.push_str("/// sentinel kind, trivia lexemes drop (ADR-008 gap re-derivation).\n");
    s.push_str("#[must_use]\n");
    s.push_str("pub fn feed(source: &str) -> Vec<(u16, std::ops::Range<u32>)> {\n");
    s.push_str("    scanner().scan(source).into_iter().filter_map(|lexeme| match lexeme {\n");
    s.push_str("        Lexeme::Token { kind, range, trivia: false } => {\n");
    s.push_str("            Some((KIND_OF_TOKEN[usize::from(kind)], range))\n");
    s.push_str("        }\n");
    let _ = writeln!(
        s,
        "        Lexeme::Error {{ range }} => Some(({bad_kind}, range)),"
    );
    s.push_str("        Lexeme::Token { trivia: true, .. } => None,\n");
    s.push_str("    }).collect()\n");
    s.push_str("}\n");
    s
}

/// Emit a `&[u32]` slice as wrapped static-array contents (12 per line).
fn emit_u32_slice(out: &mut String, items: &[u32]) {
    out.push_str("    ");
    for (idx, &v) in items.iter().enumerate() {
        if idx > 0 {
            out.push(' ');
        }
        let _ = write!(out, "{v},");
        if (idx + 1) % 12 == 0 {
            out.push_str("\n    ");
        }
    }
    if !items.is_empty() && items.len() % 12 != 0 {
        out.push('\n');
    }
}

// ---------------------------------------------------------------------------
// Parser emission
// ---------------------------------------------------------------------------

/// The `src/parser.rs`: one `fn pat_<n>` per pattern node + `fn rule_<id>`
/// per node rule, each mirroring `descent::Engine::pat`/`rule` exactly.
///
/// Two-pass design (so pattern fns are emitted as top-level methods, never
/// nested inside the rule fns that reference them):
/// 1. **Allocate** — walk every pattern (start, rule bodies, pratt atoms),
///    assign each a unique `pat_<n>` label, queue it for emission.
/// 2. **Emit** — emit the engine struct + helpers, the rule fns (referencing
///    the allocated labels), the pratt fns, then every queued `fn pat_<n>`.
fn parser_rs(
    lowered: &Lowered,
    grammar_name: &str,
    grammar_version: &str,
) -> Result<String, EmitError> {
    let banner = banner(grammar_name, grammar_version);
    let mut e = Codegen::new(banner, lowered);
    // Pass 1: allocate labels for the start pattern + every rule body + every
    // pratt atom (these are the pattern-entry points; nested patterns are
    // allocated recursively inside `alloc_pat`).
    let start_label = e.alloc_pat(&lowered.desc.start.clone())?;
    let rule_labels: Vec<usize> = lowered
        .desc
        .rules
        .iter()
        .map(|r| e.alloc_pat(&r.body.clone()))
        .collect::<Result<_, _>>()?;
    let pratt_atom_labels: Vec<usize> = lowered
        .desc
        .exprs
        .iter()
        .map(|t| e.alloc_pat(&t.atom.clone()))
        .collect::<Result<_, _>>()?;
    // Detect whether any emitted pattern uses `Pat::Node` (so `pat_node`
    // is only emitted when reachable — the zero-warning gate treats dead
    // code as a defect, issue 045).
    let uses_pat_node = e
        .pending
        .iter()
        .any(|entry| matches!(&entry.pat, Pat::Node(_, _)));
    // Pass 2: emit.
    e.emit_engine_struct_and_helpers(start_label, uses_pat_node);
    e.emit_rules(&rule_labels);
    e.emit_pratt(&pratt_atom_labels);
    e.emit_all_pats();
    Ok(e.finish())
}

/// The codegen state: a string builder plus the lowered program and a
/// queue of patterns awaiting emission.
struct Codegen<'a> {
    out: String,
    lowered: &'a Lowered,
    /// Counter for generated `pat_<n>` function labels.
    pat_counter: usize,
    /// Patterns queued for emission: entry per allocated label. Emitted
    /// in allocation order (deterministic) after the rule/pratt fns.
    pending: Vec<PatEntry>,
    /// Interning table: structurally-equal `Pat`s share one label (no
    /// duplicate `fn pat_<n>` definitions). The key is a serialized form
    /// of the pattern (Pat is not Hash/Eq by default and adding those
    /// derives upstream would be invasive; a stable string key keeps
    /// emission self-contained).
    interner: std::collections::HashMap<String, usize>,
}

/// One queued pattern: its label, the pattern itself, and the labels of
/// its direct children (allocated by `alloc_pat` so `emit_pat_fn` can
/// reference them without re-walking).
#[derive(Debug, Clone)]
struct PatEntry {
    /// This pattern's `pat_<n>` label.
    label: usize,
    /// The pattern.
    pat: Pat,
    /// Labels of the direct child patterns (in structural order).
    children: Vec<usize>,
}

impl<'a> Codegen<'a> {
    fn new(banner: String, lowered: &'a Lowered) -> Self {
        Self {
            out: banner,
            lowered,
            pat_counter: 0,
            pending: Vec::new(),
            interner: std::collections::HashMap::new(),
        }
    }

    fn finish(self) -> String {
        self.out
    }

    /// Allocate the next `pat_<n>` label and queue `pat` for emission.
    /// Children are allocated first (so their labels are lower and get
    /// emitted first); the parent stores the child labels. Structurally-
    /// equal patterns (including equal child labels) are interned — one
    /// `fn pat_<n>` per distinct pattern, no duplicates.
    fn alloc_pat(&mut self, pat: &Pat) -> Result<usize, EmitError> {
        // Walk children first, recording their labels.
        let children: Vec<usize> = match pat {
            Pat::Tok(_) | Pat::Rule(_) | Pat::Expr(_) | Pat::Recover => Vec::new(),
            Pat::Seq(items) | Pat::Alt(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(self.alloc_pat(item)?);
                }
                out
            }
            Pat::Opt(inner) | Pat::Rep(inner) | Pat::Node(_, inner) => {
                vec![self.alloc_pat(inner)?]
            }
            Pat::Peek { pat, .. } | Pat::PeekSet { pat, .. } => {
                vec![self.alloc_pat(pat)?]
            }
        };
        // Build the interning key: shape + payload + child labels. Two
        // patterns with the same shape, payload, and child labels are the
        // same function — intern them.
        let key = pat_key(pat, &children);
        if let Some(&existing) = self.interner.get(&key) {
            return Ok(existing);
        }
        // Claim a new label and queue the pattern.
        let label = self.pat_counter;
        self.pat_counter += 1;
        self.interner.insert(key, label);
        self.pending.push(PatEntry {
            label,
            pat: pat.clone(),
            children,
        });
        Ok(label)
    }

    /// The engine struct + the shared helpers (`mark`/`reset`,
    /// `fill_trivia`, `bump`, `recover`, `recover_step`, `pat_tok`,
    /// `pat_node`, `parse_document`). `pat_node` is emitted only when the
    /// lowered program contains a `Pat::Node` (the zero-warning gate treats
    /// dead code as a defect).
    fn emit_engine_struct_and_helpers(&mut self, start_label: usize, uses_pat_node: bool) {
        self.out.push_str(
            "//! Generated parser (issue 045): one `fn pat_<n>` per pattern node and\n\
             //! `fn rule_<id>` per node rule, each mirroring `descent::Engine` line\n\
             //! for line so the emitted parser reproduces the engine's `Run`\n\
             //! byte-for-byte.\n\n",
        );
        self.out.push_str("use std::ops::Range;\n\n");
        self.out.push_str("use runtime_core::ERROR_KIND;\n");
        self.out.push_str("use runtime_core::events::Event;\n");
        self.out.push_str("use crate::scanner;\n\n");
        self.out
            .push_str("/// The diagnostic side-table index meaning \"syntax error\".\n");
        self.out.push_str("pub const DIAG_SYNTAX: u32 = 0;\n\n");
        self.out
            .push_str("/// Interpreter state (mirrors `descent::Engine`).\n");
        self.out.push_str("pub struct Engine<'src> {\n");
        self.out.push_str("    feed: Vec<(u16, Range<u32>)>,\n");
        self.out.push_str("    source: &'src str,\n");
        self.out.push_str("    pos: usize,\n");
        self.out.push_str("    last_end: u32,\n");
        self.out.push_str("    pub(crate) events: Vec<Event>,\n");
        self.out
            .push_str("    pub(crate) tokens: Vec<(u16, Range<u32>)>,\n");
        self.out
            .push_str("    pub(crate) trivia: Vec<(Range<u32>, bool)>,\n");
        self.out.push_str("    pub(crate) errors: usize,\n");
        self.out.push_str("}\n\n");
        self.out.push_str("impl<'src> Engine<'src> {\n");
        self.out
            .push_str("    /// Construct the engine over `source` (the feed is scanned once).\n");
        self.out.push_str("    #[must_use]\n");
        self.out
            .push_str("    pub fn new(source: &'src str) -> Self {\n");
        self.out.push_str("        Self {\n");
        self.out
            .push_str("            feed: scanner::feed(source),\n");
        self.out.push_str("            source,\n");
        self.out.push_str("            pos: 0,\n");
        self.out.push_str("            last_end: 0,\n");
        self.out.push_str("            events: Vec::new(),\n");
        self.out.push_str("            tokens: Vec::new(),\n");
        self.out.push_str("            trivia: Vec::new(),\n");
        self.out.push_str("            errors: 0,\n");
        self.out.push_str("        }\n");
        self.out.push_str("    }\n\n");
        // mark / reset
        self.out
            .push_str("    fn mark(&self) -> (usize, usize, usize, u32) {\n");
        self.out
            .push_str("        (self.pos, self.events.len(), self.tokens.len(), self.last_end)\n");
        self.out.push_str("    }\n\n");
        self.out
            .push_str("    fn reset(&mut self, mark: (usize, usize, usize, u32)) {\n");
        self.out.push_str("        self.pos = mark.0;\n");
        self.out.push_str("        self.events.truncate(mark.1);\n");
        self.out.push_str("        self.tokens.truncate(mark.2);\n");
        self.out.push_str("        self.last_end = mark.3;\n");
        self.out.push_str("    }\n\n");
        // fill_trivia
        self.out.push_str("    fn fill_trivia(&mut self) {\n");
        self.out.push_str("        let start = self.last_end;\n");
        self.out
            .push_str("        let end = self.feed.get(self.pos).map_or(\n");
        self.out
            .push_str("            u32::try_from(self.source.len()).unwrap_or(u32::MAX),\n");
        self.out.push_str("            |(_, r)| r.start,\n");
        self.out.push_str("        );\n");
        self.out.push_str("        if end > start {\n");
        self.out
            .push_str("            let text = &self.source[start as usize..end as usize];\n");
        self.out
            .push_str("            let trailing = !text.contains('\\n');\n");
        self.out
            .push_str("            self.trivia.push((start..end, trailing));\n");
        self.out.push_str("        }\n");
        self.out.push_str("    }\n\n");
        // bump — extract (kind, range_end) without holding a borrow across
        // the &mut self call to fill_trivia (Range<u32> is not Copy).
        self.out.push_str("    fn bump(&mut self) -> bool {\n");
        self.out
            .push_str("        let Some(entry) = self.feed.get(self.pos) else {\n");
        self.out.push_str("            return false;\n");
        self.out.push_str("        };\n");
        self.out.push_str("        let kind = entry.0;\n");
        self.out.push_str("        let range_end = entry.1.end;\n");
        self.out.push_str("        self.fill_trivia();\n");
        self.out
            .push_str("        self.events.push(Event::Token { kind, n_raw: 1 });\n");
        self.out.push_str("        self.tokens.push((kind, self.feed.get(self.pos).expect(\"checked above\").1.clone()));\n");
        self.out.push_str("        self.last_end = range_end;\n");
        self.out.push_str("        self.pos += 1;\n");
        self.out.push_str("        true\n");
        self.out.push_str("    }\n\n");
        // recover
        self.out
            .push_str("    fn recover(&mut self, sync: &[u16], depth_budget: &mut u32) {\n");
        self.out.push_str("        self.errors += 1;\n");
        self.out
            .push_str("        self.events.push(Event::Error { diag: DIAG_SYNTAX });\n");
        self.out.push_str(
            "        self.events.push(Event::Start { kind: ERROR_KIND, forward_parent: None });\n",
        );
        self.out
            .push_str("        let offending = self.feed.get(self.pos).map(|(kind, _)| *kind);\n");
        self.out.push_str("        match offending {\n");
        self.out
            .push_str("            Some(kind) if !sync.contains(&kind) => {\n");
        self.out.push_str("                self.bump();\n");
        self.out.push_str("            }\n");
        self.out.push_str("            _ => {}\n");
        self.out.push_str("        }\n");
        self.out
            .push_str("        self.events.push(Event::Finish);\n");
        self.out
            .push_str("        *depth_budget = depth_budget.saturating_sub(1);\n");
        self.out.push_str("    }\n\n");
        // recover_step
        self.out.push_str("    fn recover_step(&mut self) {\n");
        self.out
            .push_str("        if self.feed.get(self.pos).is_none() {\n");
        self.out.push_str("            return;\n");
        self.out.push_str("        }\n");
        self.out.push_str("        self.errors += 1;\n");
        self.out
            .push_str("        self.events.push(Event::Error { diag: DIAG_SYNTAX });\n");
        self.out.push_str(
            "        self.events.push(Event::Start { kind: ERROR_KIND, forward_parent: None });\n",
        );
        self.out.push_str("        self.bump();\n");
        self.out
            .push_str("        self.events.push(Event::Finish);\n");
        self.out.push_str("    }\n\n");
        // pat_tok
        self.out
            .push_str("    fn pat_tok(&mut self, kind: u16, _sync: &[u16]) -> bool {\n");
        self.out
            .push_str("        if self.feed.get(self.pos).is_some_and(|(k, _)| *k == kind) {\n");
        self.out.push_str("            self.bump()\n");
        self.out.push_str("        } else {\n");
        self.out.push_str("            false\n");
        self.out.push_str("        }\n");
        self.out.push_str("    }\n\n");
        // pat_node — emitted only when the lowered program contains a
        // `Pat::Node` (else the zero-warning gate flags it as dead code).
        if uses_pat_node {
            self.out.push_str(
                "    fn pat_node<F>(&mut self, kind: u16, inner: F, sync: &[u16]) -> bool\n",
            );
            self.out.push_str("    where\n");
            self.out
                .push_str("        F: FnOnce(&mut Self, &[u16]) -> bool,\n");
            self.out.push_str("    {\n");
            self.out.push_str("        let mark = self.mark();\n");
            self.out.push_str(
                "        self.events.push(Event::Start { kind, forward_parent: None });\n",
            );
            self.out.push_str("        if inner(self, sync) {\n");
            self.out
                .push_str("            self.events.push(Event::Finish);\n");
            self.out.push_str("            true\n");
            self.out.push_str("        } else {\n");
            self.out.push_str("            self.reset(mark);\n");
            self.out.push_str("            false\n");
            self.out.push_str("        }\n");
            self.out.push_str("    }\n\n");
        }
        // parse_document — drives the start pattern then fill_trivia.
        self.out
            .push_str("    /// Drive the document start pattern (the start rule + the\n");
        self.out
            .push_str("    /// trailing recovery sweep) — mirrors `parse_events`'s body.\n");
        self.out
            .push_str("    pub fn parse_document(&mut self) {\n");
        let _ = writeln!(self.out, "        let _ = self.pat_{start_label}(&[]);");
        self.out.push_str("        self.fill_trivia();\n");
        self.out.push_str("    }\n\n");
    }

    /// Emit `fn rule_<id>` per node rule — mirrors `descent::Engine::rule`.
    fn emit_rules(&mut self, rule_labels: &[usize]) {
        for (id, rule) in self.lowered.desc.rules.iter().enumerate() {
            let sync = &rule.sync;
            let kind = rule.node_kind;
            let sync_arr = format_array_u16(sync);
            let body_label = rule_labels[id];
            let _ = writeln!(self.out, "    fn rule_{id}(&mut self) -> bool {{");
            let _ = writeln!(self.out, "        let sync: &[u16] = {sync_arr};");
            let _ = writeln!(
                self.out,
                "        self.events.push(Event::Start {{ kind: {kind}, forward_parent: None }});"
            );
            let _ = writeln!(self.out, "        let start_pos = self.pos;");
            let _ = writeln!(self.out, "        let start_events = self.events.len();");
            let _ = writeln!(self.out, "        let mut budget: u32 = 8;");
            let _ = writeln!(
                self.out,
                "        if !self.pat_{body_label}(sync) && (self.pos != start_pos || self.events.len() > start_events + 1) {{"
            );
            let _ = writeln!(self.out, "            self.recover(sync, &mut budget);");
            let _ = writeln!(self.out, "        }}");
            let _ = writeln!(self.out, "        self.events.push(Event::Finish);");
            let _ = writeln!(self.out, "        true");
            let _ = writeln!(self.out, "    }}\n");
        }
    }

    /// Emit `fn pratt_<id>` + `fn climb_primary_<id>` + the matcher helpers
    /// per Pratt table — mirrors `descent::Engine::pratt_climb`.
    fn emit_pratt(&mut self, pratt_atom_labels: &[usize]) {
        for (id, table) in self.lowered.desc.exprs.iter().enumerate() {
            self.emit_pratt_table(id, table, pratt_atom_labels[id]);
        }
        // Close the impl block — the pat_<n> fns are emitted as separate
        // methods in a second impl block (Rust allows multiple impl blocks).
        // The second impl's methods don't reference 'src, so elide it
        // (clippy::elidable_lifetime_names).
        self.out.push_str("}\n\n");
        self.out.push_str("impl Engine<'_> {\n");
    }

    fn emit_pratt_table(&mut self, id: usize, table: &PrattDesc, atom_label: usize) {
        let infix = &table.infix;
        let postfix = &table.postfix;
        let prefix = &table.prefix;
        let group = table.group;
        let _ = writeln!(
            self.out,
            "    fn pratt_{id}(&mut self, min_bp: u8) -> bool {{"
        );
        let _ = writeln!(self.out, "        let mark = self.mark();");
        let _ = writeln!(self.out, "        let sub_start = self.events.len();");
        // Group dispatch.
        if let Some((open, close)) = group {
            let _ = writeln!(self.out, "        let group_open: u16 = {open};");
            let _ = writeln!(self.out, "        let group_close: u16 = {close};");
            let _ = writeln!(
                self.out,
                "        if self.feed.get(self.pos).is_some_and(|(k, _)| *k == group_open) {{"
            );
            let _ = writeln!(self.out, "            self.bump();");
            let _ = writeln!(self.out, "            if !self.pratt_{id}(0) {{");
            let _ = writeln!(self.out, "                self.reset(mark);");
            let _ = writeln!(self.out, "                return false;");
            let _ = writeln!(self.out, "            }}");
            let _ = writeln!(
                self.out,
                "            if !self.pat_tok(group_close, &[]) {{"
            );
            let _ = writeln!(self.out, "                let mut budget: u32 = 4;");
            let _ = writeln!(self.out, "                self.recover(&[], &mut budget);");
            let _ = writeln!(self.out, "            }}");
            let _ = writeln!(self.out, "        }} else {{");
            let _ = writeln!(
                self.out,
                "            if !self.climb_primary_{id}(sub_start) {{"
            );
            let _ = writeln!(self.out, "                self.reset(mark);");
            let _ = writeln!(self.out, "                return false;");
            let _ = writeln!(self.out, "            }}");
            let _ = writeln!(self.out, "        }}");
        } else {
            let _ = writeln!(
                self.out,
                "        if !self.climb_primary_{id}(sub_start) {{"
            );
            let _ = writeln!(self.out, "            self.reset(mark);");
            let _ = writeln!(self.out, "            return false;");
            let _ = writeln!(self.out, "        }}");
        }
        // The climb loop.
        let _ = writeln!(
            self.out,
            "        while let Some((kind, _)) = self.feed.get(self.pos) {{"
        );
        let _ = writeln!(self.out, "            let kind = *kind;");
        // Infix match.
        if !infix.is_empty() {
            let _ = writeln!(
                self.out,
                "            let infix = infix_match_{id}(kind, min_bp);"
            );
            let _ = writeln!(self.out, "            if let Some((node, rbp)) = infix {{");
            let _ = writeln!(
                self.out,
                "                self.events.insert(sub_start, Event::Start {{ kind: node, forward_parent: None }});"
            );
            let _ = writeln!(self.out, "                self.bump();");
            let _ = writeln!(self.out, "                if !self.pratt_{id}(rbp) {{");
            let _ = writeln!(self.out, "                    let mut budget: u32 = 4;");
            let _ = writeln!(
                self.out,
                "                    self.recover(&[], &mut budget);"
            );
            let _ = writeln!(self.out, "                }}");
            let _ = writeln!(self.out, "                self.events.push(Event::Finish);");
            let _ = writeln!(self.out, "            }}");
            if !postfix.is_empty() {
                let _ = writeln!(self.out, "            else {{");
            }
        }
        // Postfix match.
        if postfix.is_empty() {
            let _ = writeln!(self.out, "            else {{ break; }}");
        } else {
            let indent = if infix.is_empty() { "" } else { "    " };
            let _ = writeln!(
                self.out,
                "            {indent}let postfix = postfix_match_{id}(kind, min_bp);"
            );
            let _ = writeln!(
                self.out,
                "            {indent}if let Some(node) = postfix {{"
            );
            let _ = writeln!(
                self.out,
                "            {indent}    self.events.insert(sub_start, Event::Start {{ kind: node, forward_parent: None }});"
            );
            let _ = writeln!(self.out, "            {indent}    self.bump();");
            let _ = writeln!(
                self.out,
                "            {indent}    self.events.push(Event::Finish);"
            );
            let _ = writeln!(self.out, "            {indent}}} else {{");
            let _ = writeln!(self.out, "            {indent}    break;");
            let _ = writeln!(self.out, "            {indent}}}");
        }
        if !infix.is_empty() && !postfix.is_empty() {
            let _ = writeln!(self.out, "            }}");
        }
        let _ = writeln!(self.out, "        }}");
        let _ = writeln!(self.out, "        true");
        let _ = writeln!(self.out, "    }}\n");
        // climb_primary_<id>: prefix or atom.
        let _ = writeln!(
            self.out,
            "    fn climb_primary_{id}(&mut self, _sub_start: usize) -> bool {{"
        );
        if prefix.is_empty() {
            let _ = writeln!(self.out, "        self.pat_{atom_label}(&[])");
        } else {
            let _ = writeln!(
                self.out,
                "        let prefix = prefix_match_{id}(self.feed.get(self.pos).map(|(k, _)| *k).unwrap_or(0));"
            );
            let _ = writeln!(self.out, "        if let Some((node, bp)) = prefix {{");
            let _ = writeln!(
                self.out,
                "            self.events.push(Event::Start {{ kind: node, forward_parent: None }});"
            );
            let _ = writeln!(self.out, "            self.bump();");
            let _ = writeln!(self.out, "            if !self.pratt_{id}(bp) {{");
            let _ = writeln!(self.out, "                return false;");
            let _ = writeln!(self.out, "            }}");
            let _ = writeln!(self.out, "            self.events.push(Event::Finish);");
            let _ = writeln!(self.out, "            true");
            let _ = writeln!(self.out, "        }} else {{");
            let _ = writeln!(self.out, "            self.pat_{atom_label}(&[])");
            let _ = writeln!(self.out, "        }}");
        }
        let _ = writeln!(self.out, "    }}\n");
        // The matcher helpers.
        self.emit_infix_matcher(id, infix);
        self.emit_postfix_matcher(id, postfix);
        self.emit_prefix_matcher(id, prefix);
    }

    fn emit_infix_matcher(&mut self, id: usize, infix: &[descent::InfixDesc]) {
        let _ = writeln!(
            self.out,
            "    fn infix_match_{id}(kind: u16, min_bp: u8) -> Option<(u16, u8)> {{"
        );
        if infix.is_empty() {
            let _ = writeln!(self.out, "        None");
        } else {
            let _ = writeln!(self.out, "        match (kind, min_bp) {{");
            for entry in infix {
                let _ = writeln!(
                    self.out,
                    "            ({token}, m) if {lbp} >= m => Some(({node}, {rbp})),",
                    token = entry.token,
                    lbp = entry.lbp,
                    node = entry.node,
                    rbp = entry.rbp
                );
            }
            let _ = writeln!(self.out, "            _ => None,");
            let _ = writeln!(self.out, "        }}");
        }
        let _ = writeln!(self.out, "    }}\n");
    }

    fn emit_postfix_matcher(&mut self, id: usize, postfix: &[descent::UnaryDesc]) {
        let _ = writeln!(
            self.out,
            "    fn postfix_match_{id}(kind: u16, min_bp: u8) -> Option<u16> {{"
        );
        if postfix.is_empty() {
            let _ = writeln!(self.out, "        None");
        } else {
            let _ = writeln!(self.out, "        match (kind, min_bp) {{");
            for entry in postfix {
                let _ = writeln!(
                    self.out,
                    "            ({token}, m) if {bp} >= m => Some({node}),",
                    token = entry.token,
                    bp = entry.bp,
                    node = entry.node
                );
            }
            let _ = writeln!(self.out, "            _ => None,");
            let _ = writeln!(self.out, "        }}");
        }
        let _ = writeln!(self.out, "    }}\n");
    }

    fn emit_prefix_matcher(&mut self, id: usize, prefix: &[descent::UnaryDesc]) {
        let _ = writeln!(
            self.out,
            "    fn prefix_match_{id}(kind: u16) -> Option<(u16, u8)> {{"
        );
        if prefix.is_empty() {
            let _ = writeln!(self.out, "        None");
        } else {
            let _ = writeln!(self.out, "        match kind {{");
            for entry in prefix {
                let _ = writeln!(
                    self.out,
                    "            {token} => Some(({node}, {bp})),",
                    token = entry.token,
                    node = entry.node,
                    bp = entry.bp
                );
            }
            let _ = writeln!(self.out, "            _ => None,");
            let _ = writeln!(self.out, "        }}");
        }
        let _ = writeln!(self.out, "    }}\n");
    }

    /// Emit every queued `fn pat_<n>` as a method — mirrors
    /// `descent::Engine::pat` arm for arm.
    fn emit_all_pats(&mut self) {
        // Drain the pending queue (allocation order = label order).
        let pending = std::mem::take(&mut self.pending);
        for entry in pending {
            self.emit_pat_fn(entry.label, &entry.pat, &entry.children);
        }
        // Close the second impl block.
        self.out.push_str("}\n");
    }

    /// Emit `fn pat_<label>(&mut self, sync: &[u16]) -> bool` for `pat`.
    /// `children` holds the direct child labels (allocated by `alloc_pat`).
    fn emit_pat_fn(&mut self, label: usize, pat: &Pat, children: &[usize]) {
        // Variants that never reference `sync` name it `_sync` to satisfy
        // the `unused_variables` lint the emitted crate's pedantic posture
        // treats as a warning (zero-warning gate, issue 045).
        let uses_sync = !matches!(pat, Pat::Rule(_) | Pat::Expr(_) | Pat::Recover);
        let sync_param = if uses_sync { "sync" } else { "_sync" };
        let _ = writeln!(
            self.out,
            "    fn pat_{label}(&mut self, {sync_param}: &[u16]) -> bool {{"
        );
        match pat {
            Pat::Tok(kind) => {
                let _ = writeln!(self.out, "        self.pat_tok({kind}, sync)");
            }
            Pat::Seq(items) => {
                if items.is_empty() {
                    let _ = writeln!(self.out, "        true");
                } else {
                    let _ = writeln!(self.out, "        let mut all = true;");
                    for child in children {
                        let _ = writeln!(
                            self.out,
                            "        if !self.pat_{child}(sync) {{ all = false; }}"
                        );
                    }
                    let _ = writeln!(self.out, "        all");
                }
            }
            Pat::Alt(_) => {
                let _ = writeln!(self.out, "        let mut matched = false;");
                for child in children {
                    let _ = writeln!(self.out, "        if !matched {{");
                    let _ = writeln!(self.out, "            let mark = self.mark();");
                    let _ = writeln!(self.out, "            if self.pat_{child}(sync) {{");
                    let _ = writeln!(self.out, "                matched = true;");
                    let _ = writeln!(self.out, "            }} else {{");
                    let _ = writeln!(self.out, "                self.reset(mark);");
                    let _ = writeln!(self.out, "            }}");
                    let _ = writeln!(self.out, "        }}");
                }
                let _ = writeln!(self.out, "        matched");
            }
            Pat::Opt(_) => {
                let child = children[0];
                let _ = writeln!(self.out, "        let mark = self.mark();");
                let _ = writeln!(self.out, "        if !self.pat_{child}(sync) {{");
                let _ = writeln!(self.out, "            self.reset(mark);");
                let _ = writeln!(self.out, "        }}");
                let _ = writeln!(self.out, "        true");
            }
            Pat::Rep(_) => {
                let child = children[0];
                let _ = writeln!(self.out, "        loop {{");
                let _ = writeln!(self.out, "            let mark = self.mark();");
                let _ = writeln!(self.out, "            if !self.pat_{child}(sync) {{");
                let _ = writeln!(self.out, "                self.reset(mark);");
                let _ = writeln!(self.out, "                break;");
                let _ = writeln!(self.out, "            }}");
                let _ = writeln!(self.out, "            if self.pos == mark.0 {{ break; }}");
                let _ = writeln!(self.out, "        }}");
                let _ = writeln!(self.out, "        true");
            }
            Pat::Node(kind, _) => {
                let child = children[0];
                let _ = writeln!(
                    self.out,
                    "        self.pat_node({kind}, |eng, sync| eng.pat_{child}(sync), sync)"
                );
            }
            Pat::Rule(id) => {
                let _ = writeln!(self.out, "        self.rule_{id}()");
            }
            Pat::Expr(id) => {
                let _ = writeln!(self.out, "        self.pratt_{id}(0)");
            }
            Pat::Peek { .. } | Pat::PeekSet { .. } => {
                let child = children[0];
                match pat {
                    Pat::Peek { tok, .. } => {
                        let _ = writeln!(
                            self.out,
                            "        if self.feed.get(self.pos).is_some_and(|(k, _)| *k == {tok}) {{"
                        );
                    }
                    Pat::PeekSet { toks, .. } => {
                        let arr = format_array_u16_bracketed(toks);
                        let _ = writeln!(
                            self.out,
                            "        let cur = self.feed.get(self.pos).map(|(k, _)| *k);"
                        );
                        let _ = writeln!(
                            self.out,
                            "        if cur.is_some_and(|k| {arr}.contains(&k)) {{"
                        );
                    }
                    _ => unreachable!("the outer arm already narrowed Peek/PeekSet"),
                }
                let _ = writeln!(self.out, "            self.pat_{child}(sync)");
                let _ = writeln!(self.out, "        }} else {{");
                let _ = writeln!(self.out, "            false");
                let _ = writeln!(self.out, "        }}");
            }
            Pat::Recover => {
                let _ = writeln!(self.out, "        self.recover_step();");
                let _ = writeln!(self.out, "        true");
            }
        }
        let _ = writeln!(self.out, "    }}\n");
    }
}

/// Format a `&[u16]` as a Rust array literal (with the `&[]` wrapper).
/// Emits `&[3, 4, 5]` — slice context forces u16 inference at the call site.
fn format_array_u16(items: &[u16]) -> String {
    let inner: Vec<String> = items.iter().map(u16::to_string).collect();
    format!("&[{}]", inner.join(", "))
}

/// Format a `&[u16]` as a Rust array literal `[3, 4, 5]` (no leading `&`).
/// Used where the call site needs `[T; N]` (e.g. `[T; N]::contains` returns
/// `bool`, but `&[T; N]::contains` returns `&bool`).
fn format_array_u16_bracketed(items: &[u16]) -> String {
    let inner: Vec<String> = items.iter().map(u16::to_string).collect();
    format!("[{}]", inner.join(", "))
}

/// A stable interning key for a `Pat` + its allocated child labels. Two
/// patterns with the same key are the same generated function.
fn pat_key(pat: &Pat, children: &[usize]) -> String {
    match pat {
        Pat::Tok(k) => format!("Tok({k})"),
        Pat::Rule(id) => format!("Rule({id})"),
        Pat::Expr(id) => format!("Expr({id})"),
        Pat::Recover => "Recover".to_owned(),
        Pat::Seq(_) => {
            let kids = children
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",");
            format!("Seq[{kids}]")
        }
        Pat::Alt(_) => {
            let kids = children
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(",");
            format!("Alt[{kids}]")
        }
        Pat::Opt(_) => format!("Opt({})", children[0]),
        Pat::Rep(_) => format!("Rep({})", children[0]),
        Pat::Node(k, _) => format!("Node({k},{})", children[0]),
        Pat::Peek { tok, .. } => format!("Peek({tok},{})", children[0]),
        Pat::PeekSet { toks, .. } => {
            let t = toks
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(",");
            format!("PeekSet[{t}],{}", children[0])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use descent_lower::lower;
    use gir_syntax::load_grammar;

    fn json_fixture() -> (Lowered, Scanner) {
        let grammar = load_grammar(descent_lower::JSON_GRAMMAR).expect("fixture loads");
        let lowered = lower(&grammar).expect("fixture lowers");
        let scanner = lexgen::compile(&grammar).expect("fixture lexes");
        (lowered, scanner)
    }

    #[test]
    fn emission_is_deterministic_byte_identical() {
        let (lowered, scanner) = json_fixture();
        let a = emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");
        let b = emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");
        assert_eq!(a.files.len(), b.files.len());
        for (fa, fb) in a.files.iter().zip(b.files.iter()) {
            assert_eq!(fa.path, fb.path, "paths match");
            assert_eq!(fa.contents, fb.contents, "byte-identical {}", fa.path);
        }
    }

    #[test]
    fn crate_name_is_sanitized() {
        let (lowered, scanner) = json_fixture();
        let c = emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");
        assert_eq!(c.name, "syntact_json_generated");
    }

    #[test]
    fn emitted_cargo_toml_forbids_unsafe_and_requires_missing_docs() {
        let (lowered, scanner) = json_fixture();
        let c = emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");
        let toml = c.file("Cargo.toml").expect("Cargo.toml emitted");
        assert!(toml.contains("unsafe_code = \"forbid\""));
        assert!(toml.contains("missing_docs = \"warn\""));
        assert!(toml.contains("pedantic = { level = \"warn\""));
        assert!(toml.contains("runtime-core"));
        assert!(toml.contains("lexgen"));
    }

    #[test]
    fn emitted_parser_has_one_fn_per_node_rule() {
        let (lowered, scanner) = json_fixture();
        let c = emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");
        let parser = c.file("src/parser.rs").expect("parser.rs emitted");
        // The JSON fixture has 3 node rules (object, pair, array).
        // Count only definitions (4-space indent + `fn rule_`), not the
        // module doc-comment mention.
        let defs: Vec<&str> = parser
            .lines()
            .filter(|l| l.starts_with("    fn rule_"))
            .collect();
        assert_eq!(defs.len(), 3, "three node-rule fns; got: {defs:?}");
    }

    #[test]
    fn emitted_parser_has_no_allow_attributes() {
        let (lowered, scanner) = json_fixture();
        let c = emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");
        for f in &c.files {
            assert!(
                !f.contents.contains("#[allow") && !f.contents.contains("#![allow"),
                "{} has an #[allow] attribute",
                f.path
            );
        }
    }
}
