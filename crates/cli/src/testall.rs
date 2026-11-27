//! `syntact test-all` — the cross-backend differential oracle (roadmap
//! §7.4 "Cross-backend oracle", §11.1 #4 "Self-differential").
//!
//! The plan's acceptance criterion, quoted: *"`syntact test-all` emits
//! descent and table backends together and differential-tests them on
//! the corpus (lalrpop `TestAll` pattern) — our own backends keep each
//! other honest."*
//!
//! Both backends consume the SAME scanner feed (one [`descent_lower::Lowered::feed`]
//! per document):
//!
//! - **descent** — the descriptor interpreter ([`descent::parse_events`]);
//!   its verdict is a clean derivation: zero `Event::Error`s over a
//!   non-empty feed (or a nullable start over an empty one — the engine
//!   closes a zero-progress start silently, so the empty-feed case is
//!   decided by the lowering's nullability, and the table backend's ε
//!   derivation agrees by construction).
//! - **table** — the O1-tier table-driven LR(1) driver
//!   ([`lr_shadow::driver::parse`]) over the shadow automaton built from
//!   the same grammar.
//!
//! A document **agrees** when the two verdicts match; any disagreement
//! is one backend mis-classifying the language the other implements.
//! A rejected run also carries the table backend's step error (state,
//! offending byte offset, expected terminal set rendered by token name —
//! the §7.4 expected-set extraction). [`run`] is pure (no filesystem);
//! determinism is asserted by an independent second evaluation, fresh
//! automaton to fresh feed, and the report rendering is byte-stable for
//! identical inputs (ADR-013).

use gir_ir::{Grammar, TokenId};
use gir_syntax::load_grammar;
use lr_shadow::lower::Look;
use lr_shadow::{Automaton, First};

/// One corpus document: a display name and its source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusDoc {
    /// The name the report renders (a path or `stdin`).
    pub name: String,
    /// The document source.
    pub source: String,
}

/// A named refusal — the command layer maps every variant to a usage
/// error carrying this message. The pipeline is never silently narrowed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TestAllError {
    /// The grammar text did not load through `gir-syntax`.
    Grammar(String),
    /// The grammar did not lower into descent descriptors (the compiler
    /// refuses, it never mis-lowers — the `descent-lower` contract).
    Lowering(String),
    /// The lexer compilation refused.
    Lexer(String),
    /// No corpus documents were given — there is nothing to differential-test.
    EmptyCorpus,
}

impl std::fmt::Display for TestAllError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Grammar(message) => write!(f, "grammar refused: {message}"),
            Self::Lowering(message) => write!(f, "lowering refused: {message}"),
            Self::Lexer(message) => write!(f, "lexer refused: {message}"),
            Self::EmptyCorpus => write!(f, "no corpus documents given"),
        }
    }
}

/// The table backend's rejection step, rendered for the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LrRejection {
    /// The state the machine sat in (the automaton's discovery-order id).
    pub state: u32,
    /// The offending byte offset in the document (end of input when the
    /// pseudo-EOF carried no action).
    pub offset: u32,
    /// The expected terminal set, by token name (`eof` last).
    pub expected: Vec<String>,
}

/// One document's two backend verdicts plus the agreement bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocVerdict {
    /// The corpus document's display name.
    pub name: String,
    /// The document size in bytes.
    pub bytes: usize,
    /// Feed length: the non-trivia scanner tokens both backends saw.
    pub tokens: usize,
    /// Syntax errors the descent engine reported.
    pub descent_errors: usize,
    /// The descent verdict: a clean derivation of the whole feed (the
    /// nullable-start clause covers the engine's silent zero-progress
    /// close over an empty feed).
    pub descent_valid: bool,
    /// The table verdict: the LR(1) machine reached the accept action.
    pub lr_accepted: bool,
    /// The table rejection step, when it rejected.
    pub lr_error: Option<LrRejection>,
    /// Whether the backends agree (`descent_valid == lr_accepted`).
    pub agree: bool,
}

/// The oracle's report: every document's verdicts plus the overall
/// agreement counts and the determinism bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestAllReport {
    /// The grammar's name (e.g. `"json"`).
    pub grammar: String,
    /// One verdict per corpus document, in input order.
    pub documents: Vec<DocVerdict>,
    /// An independent second evaluation (fresh loader → automaton →
    /// feeds) produced identical verdicts.
    pub deterministic: bool,
    /// Documents where the backends agree.
    pub agreements: usize,
    /// Documents where the backends disagree (the report's negative
    /// verdict — one backend is wrong about the language).
    pub disagreements: usize,
}

/// Differential-test the descent engine against the table-driven LR(1)
/// driver over `corpus`, all through one grammar.
///
/// # Errors
/// [`TestAllError`] for a grammar that does not load or lower, a lexer
/// compilation refusal, or an empty corpus.
pub fn run(grammar_text: &str, corpus: &[CorpusDoc]) -> Result<TestAllReport, TestAllError> {
    if corpus.is_empty() {
        return Err(TestAllError::EmptyCorpus);
    }
    let first = evaluate(grammar_text, corpus)?;
    let second = evaluate(grammar_text, corpus)?;
    let deterministic = first == second;
    let agreements = first.documents.iter().filter(|doc| doc.agree).count();
    let disagreements = first.documents.len() - agreements;
    Ok(TestAllReport {
        grammar: first.grammar_name,
        documents: first.documents,
        deterministic,
        agreements,
        disagreements,
    })
}

/// One evaluation pass: everything rebuilt from the grammar text, so two
/// passes are genuinely independent runs of both backends.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Evaluation {
    grammar_name: String,
    documents: Vec<DocVerdict>,
}

fn evaluate(grammar_text: &str, corpus: &[CorpusDoc]) -> Result<Evaluation, TestAllError> {
    let grammar =
        load_grammar(grammar_text).map_err(|error| TestAllError::Grammar(error.to_string()))?;
    let lowered = descent_lower::lower(&grammar)
        .map_err(|error| TestAllError::Lowering(error.to_string()))?;
    let scanner =
        lexgen::compile(&grammar).map_err(|error| TestAllError::Lexer(error.to_string()))?;
    let automaton = Automaton::build(&grammar);

    // Feed kind → automaton terminal. Every declared token owns exactly
    // one kind (`descent-lower`'s dense kind space), so the inverse map
    // is total over the real kinds; scanner-unmatched `bad` tokens ride
    // a terminal outside the token table — no action can exist for it,
    // which is exactly the table backend's rejection of input the
    // scanner could not match (the engine sweeps the same tokens into
    // ERROR nodes, so the verdicts align on rejection).
    let stranger = Look(u32::MAX - 1);
    let mut look_of_kind = vec![stranger; usize::from(lowered.bad_kind) + 1];
    for (index, &kind) in lowered.kind_of_token.iter().enumerate() {
        let value = if grammar.tokens[index].trivia {
            // Trivia never enters a feed (`Lowered::feed` drops it); the
            // slot exists so the map is total over kinds.
            stranger
        } else {
            Look::token(TokenId(u16::try_from(index).expect("token count fits u16")))
        };
        look_of_kind[usize::from(kind)] = value;
    }
    look_of_kind[usize::from(lowered.bad_kind)] = stranger;
    let start_nullable =
        First::compute(&automaton.lowered).nullable[automaton.lowered.start.0 as usize];

    let mut documents = Vec::with_capacity(corpus.len());
    for document in corpus {
        let feed = lowered.feed(&scanner, &document.source);
        let looks: Vec<Look> = feed
            .iter()
            .map(|(kind, _)| look_of_kind[usize::from(*kind)])
            .collect();
        let descent_run = descent::parse_events(&lowered.desc, &feed, &document.source);
        let lr = lr_shadow::driver::parse(&automaton, &looks);

        // The verdict rule (module docs): a clean engine derivation of
        // the whole feed — or the nullable-start clause, which the table
        // backend's ε derivation matches by construction.
        let descent_valid = descent_run.errors == 0 && (!feed.is_empty() || start_nullable);
        let lr_error = lr.error.map(|error| LrRejection {
            state: error.state,
            offset: feed.get(error.at).map_or(
                u32::try_from(document.source.len()).unwrap_or(u32::MAX),
                |(_, range)| range.start,
            ),
            expected: error
                .expected
                .iter()
                .map(|&look| look_name(&grammar, look))
                .collect(),
        });
        documents.push(DocVerdict {
            name: document.name.clone(),
            bytes: document.source.len(),
            tokens: feed.len(),
            descent_errors: descent_run.errors,
            descent_valid,
            lr_accepted: lr.accepted,
            lr_error,
            agree: descent_valid == lr.accepted,
        });
    }
    Ok(Evaluation {
        grammar_name: grammar.name,
        documents,
    })
}

/// Render a terminal by name: the token's interned name, or `eof` for
/// the pseudo-EOF sentinel (the `Look` display convention).
fn look_name(grammar: &Grammar, look: Look) -> String {
    match look.as_token() {
        Some(id) => grammar
            .interner
            .lookup(grammar.tokens[usize::from(id.0)].name)
            .to_owned(),
        None if look.is_eof() => "eof".to_owned(),
        None => "unmatched".to_owned(),
    }
}

/// Machine-readable report rendering; byte-stable for identical inputs
/// (ADR-013), documents in input order.
#[must_use]
pub fn render_json(report: &TestAllReport) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"grammar\": {},", json_str(&report.grammar));
    let _ = writeln!(out, "  \"documents\": [");
    for (index, document) in report.documents.iter().enumerate() {
        let comma = if index + 1 == report.documents.len() {
            ""
        } else {
            ","
        };
        let _ = writeln!(out, "    {{");
        let _ = writeln!(out, "      \"name\": {},", json_str(&document.name));
        let _ = writeln!(out, "      \"bytes\": {},", document.bytes);
        let _ = writeln!(out, "      \"tokens\": {},", document.tokens);
        let _ = writeln!(
            out,
            "      \"descent_errors\": {},",
            document.descent_errors
        );
        let _ = writeln!(out, "      \"descent_valid\": {},", document.descent_valid);
        let _ = writeln!(out, "      \"lr_accepted\": {},", document.lr_accepted);
        match &document.lr_error {
            None => {
                let _ = writeln!(out, "      \"lr_error\": null,");
            }
            Some(error) => {
                let expected = error
                    .expected
                    .iter()
                    .map(|name| json_str(name))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(out, "      \"lr_error\": {{");
                let _ = writeln!(out, "        \"state\": {},", error.state);
                let _ = writeln!(out, "        \"offset\": {},", error.offset);
                let _ = writeln!(out, "        \"expected\": [{expected}]");
                let _ = writeln!(out, "      }},");
            }
        }
        let _ = writeln!(out, "      \"agree\": {}", document.agree);
        let _ = writeln!(out, "    }}{comma}");
    }
    let _ = writeln!(out, "  ],");
    let _ = writeln!(out, "  \"deterministic\": {},", report.deterministic);
    let _ = writeln!(out, "  \"agreements\": {},", report.agreements);
    let _ = writeln!(out, "  \"disagreements\": {}", report.disagreements);
    let _ = writeln!(out, "}}");
    out
}

/// JSON string literal (the `pgo` module's escaping discipline).
fn json_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = std::fmt::Write::write_fmt(&mut out, format_args!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
