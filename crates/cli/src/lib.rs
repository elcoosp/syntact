//! # cli — the `syntact` command line (issue 020)
//!
//! Subcommands over the real pipeline (everything that exists today, no
//! stubs):
//!
//! - `check <GRAMMAR.gir | DOCUMENT.json>` — grammar validation (KDL →
//!   IR) or a document parse check through the JSON pack.
//! - `build <GRAMMAR.gir>` — compile the front half of the pipeline:
//!   KDL → IR, lexer compilation (regex → NFA → DFA) and the LR(1) shadow
//!   automaton; prints the per-stage stats a codegen backend would
//!   consume (Rust table emission is issue 015).
//! - `conflicts <GRAMMAR.gir>` — the determinism report: per-rule
//!   backend classification plus every LL(1) conflict site.
//! - `conformance <FILES…>` — run the JSON pack over documents and gate
//!   byte-faithfulness (`render(parse(x)) == x`).
//! - `trace <PACK> <FILE>` — per-region recovery events (offset, kind,
//!   repair ops, resync point) from the pack's reference parser plus,
//!   for JSON, the shadow-automaton CPCT+ repair loop (issue 029).
//! - `lsp` — the `.gir` grammar language server over stdio
//!   (JSON-RPC 2.0, LSP 3.17 subset; issue 041).
//! - `pack-lsp` — the pack language server over stdio: diagnostics,
//!   outline symbols, folding, semantic tokens, highlights, and
//!   selection ranges for the ten shipped pack languages (issue 042).
//!
//! [`run`] is a pure function from arguments to an exit code, so the
//! integration tests drive it directly.

use std::io::Read as _;

use clap::{Parser as ClapParser, Subcommand};
use gir_ir::Grammar;
use runtime_core::input::Source;

pub mod trace;

/// The `syntact` command line.
#[derive(Debug, ClapParser)]
pub struct Cli {
    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Validate a grammar (.gir KDL) or a JSON document.
    Check {
        /// The file to check (`-` reads stdin).
        #[arg(value_name = "FILE")]
        file: String,
    },
    /// Compile a grammar through the pipeline front half (loader, lexer,
    /// shadow automaton) and report the stats.
    Build {
        /// The .gir grammar file.
        #[arg(value_name = "GRAMMAR")]
        grammar: String,
    },
    /// Print the determinism report: per-rule backends + conflict sites.
    Conflicts {
        /// The .gir grammar file.
        #[arg(value_name = "GRAMMAR")]
        grammar: String,
    },
    /// Run JSON documents through the pack and gate byte-faithfulness.
    Conformance {
        /// Documents to check (`-` reads stdin once).
        #[arg(value_name = "FILE", num_args = 1..)]
        files: Vec<String>,
    },
    /// Print per-region recovery events for a document through a pack.
    Trace {
        /// The pack to trace through (`json`, `toml`).
        #[arg(value_name = "PACK")]
        pack: String,
        /// The document to trace (`-` reads stdin).
        #[arg(value_name = "FILE")]
        file: String,
        /// Emit machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Serve the `.gir` grammar language server over stdio (issue 041).
    Lsp {
        /// Bound one framed inbound message at this many bytes.
        #[arg(long, default_value_t = lsp::transport::DEFAULT_MAX_MESSAGE)]
        max_message: usize,
    },
    /// Serve the pack language server over stdio (issue 042).
    PackLsp {
        /// Bound one framed inbound message at this many bytes.
        #[arg(long, default_value_t = lsp::transport::DEFAULT_MAX_MESSAGE)]
        max_message: usize,
    },
}

/// Exit codes: 0 ok, 1 negative verdict, 2 usage/IO error.
pub const OK: i32 = 0;
/// Negative verdict (check failed, conflicts exist, conformance
/// failures).
pub const FAIL: i32 = 1;
/// Usage or IO error (bad arguments, unreadable file).
pub const USAGE: i32 = 2;

/// Run the CLI; returns the process exit code.
///
/// # Errors
/// Argument-parse failures come back as `Err` (the binary formats them
/// through clap's help machinery).
pub fn run<I, T>(args: I) -> Result<i32, String>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = Cli::try_parse_from(
        std::iter::once(std::ffi::OsString::from("syntact"))
            .chain(args.into_iter().map(Into::into)),
    )
    .map_err(|error| error.to_string())?;
    Ok(match cli.command {
        Command::Check { file } => cmd_check(&file),
        Command::Build { grammar } => cmd_build(&grammar),
        Command::Conflicts { grammar } => cmd_conflicts(&grammar),
        Command::Conformance { files } => cmd_conformance(&files),
        Command::Trace { pack, file, json } => cmd_trace(&pack, &file, json),
        Command::Lsp { max_message } => cmd_lsp(max_message),
        Command::PackLsp { max_message } => cmd_pack_lsp(max_message),
    })
}

/// Serve the grammar language server over stdin/stdout.
fn cmd_lsp(max_message: usize) -> i32 {
    use std::io::BufReader;
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    lsp::transport::serve(&mut BufReader::new(&mut stdin), &mut stdout, max_message)
}

/// Serve the pack language server over stdin/stdout.
fn cmd_pack_lsp(max_message: usize) -> i32 {
    use std::io::BufReader;
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    lsp::pack::server::serve(&mut BufReader::new(&mut stdin), &mut stdout, max_message)
}

/// Read a file (or stdin for `-`).
fn read_input(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut buffer = String::new();
        std::io::stdin()
            .read_to_string(&mut buffer)
            .map_err(|error| format!("stdin: {error}"))?;
        Ok(buffer)
    } else {
        std::fs::read_to_string(path).map_err(|error| format!("{path}: {error}"))
    }
}

fn cmd_check(file: &str) -> i32 {
    let Ok(text) = read_input(file) else {
        return USAGE;
    };
    let is_grammar = std::path::Path::new(file)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gir"));
    if is_grammar {
        return match load_grammar(&text) {
            Ok(grammar) => {
                println!(
                    "grammar `{}` v{}: {} tokens, {} rules — ok",
                    grammar.name,
                    grammar.version,
                    grammar.tokens.len(),
                    grammar.rules.len()
                );
                OK
            }
            Err(error) => {
                eprintln!("{error}");
                FAIL
            }
        };
    }
    // JSON document check: the pack recovers, so the verdict is the
    // syntax-error count from parse_events, not the Result.
    let (_events, _ranges, errors) = packs_json::reference::parse_events(&text);
    if errors == 0 {
        println!("document ok: {} bytes", text.len());
        OK
    } else {
        eprintln!("document invalid: {errors} syntax error(s)");
        FAIL
    }
}

fn cmd_build(grammar_path: &str) -> i32 {
    let Ok(text) = read_input(grammar_path) else {
        return USAGE;
    };
    let Ok(grammar) = load_grammar(&text) else {
        return FAIL;
    };
    // Stage 2: lexer compilation (regex → NFA → DFA).
    let Ok(scanner) = lexgen::compile(&grammar) else {
        return FAIL;
    };
    // Stage 3: LR(1) shadow automaton.
    let automaton = lr_shadow::Automaton::build(&grammar);
    // Stage 4: determinism classification.
    let report = gir_analysis::DeterminismReport::compute(&grammar);
    println!(
        "grammar `{}`: {} tokens, {} rules",
        grammar.name,
        grammar.tokens.len(),
        grammar.rules.len()
    );
    println!("lexer: {} DFA states", scanner.dfa_len());
    println!(
        "shadow: {} LR(1) states, {} conflicts",
        automaton.states.len(),
        automaton.conflicts.len()
    );
    let reachable = report
        .verdicts
        .iter()
        .filter(|verdict| verdict.reachable)
        .count();
    println!("determinism: {reachable} reachable rules classified");
    OK
}

fn cmd_conflicts(grammar_path: &str) -> i32 {
    let Ok(text) = read_input(grammar_path) else {
        return USAGE;
    };
    let Ok(grammar) = load_grammar(&text) else {
        return FAIL;
    };
    let report = gir_analysis::DeterminismReport::compute(&grammar);
    print!("{report}");
    if report.all_deterministic() {
        println!("all reachable rules deterministic (descent/pratt)");
        OK
    } else {
        println!("table fallback present");
        FAIL
    }
}

fn cmd_conformance(files: &[String]) -> i32 {
    if files.is_empty() {
        eprintln!("conformance: no documents given");
        return USAGE;
    }
    let mut failures = 0;
    for file in files {
        let Ok(text) = read_input(file) else {
            eprintln!("{file}: unreadable");
            failures += 1;
            continue;
        };
        let (_, _ranges, errors) = packs_json::reference::parse_events(&text);
        let faithful = packs_json::reference::parse(&text).is_ok_and(|tree| {
            let input = Source::from_str_unchecked(&text);
            tree.render(&input) == text
        });
        let verdict = errors == 0 && faithful;
        if verdict {
            println!("{file}: ok");
        } else {
            println!("{file}: FAIL (parse or byte-faithfulness)");
            failures += 1;
        }
    }
    if failures == 0 { OK } else { FAIL }
}

fn cmd_trace(pack: &str, file: &str, json: bool) -> i32 {
    let Ok(text) = read_input(file) else {
        return USAGE;
    };
    match trace::trace(pack, &text) {
        Ok(output) => {
            if json {
                print!("{}", trace::render_json(&output));
            } else {
                print!("{}", trace::render_text(&output));
            }
            if output.ok { OK } else { FAIL }
        }
        Err(error) => {
            eprintln!("{error}");
            USAGE
        }
    }
}

/// Load a grammar from the KDL surface (issue 004 loader).
fn load_grammar(text: &str) -> Result<Grammar, String> {
    gir_syntax::load_grammar(text).map_err(|error| format!("grammar: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsp_subcommand_parses_with_the_default_bound() {
        let cli = Cli::try_parse_from(["syntact", "lsp"]).expect("parses");
        match cli.command {
            Command::Lsp { max_message } => {
                assert_eq!(max_message, lsp::transport::DEFAULT_MAX_MESSAGE);
            }
            _ => panic!("expected the lsp subcommand"),
        }
    }

    #[test]
    fn lsp_subcommand_accepts_a_custom_message_bound() {
        let cli = Cli::try_parse_from(["syntact", "lsp", "--max-message", "4096"]).expect("parses");
        match cli.command {
            Command::Lsp { max_message } => assert_eq!(max_message, 4096),
            _ => panic!("expected the lsp subcommand"),
        }
    }

    #[test]
    fn pack_lsp_subcommand_parses_with_the_default_bound() {
        let cli = Cli::try_parse_from(["syntact", "pack-lsp"]).expect("parses");
        match cli.command {
            Command::PackLsp { max_message } => {
                assert_eq!(max_message, lsp::transport::DEFAULT_MAX_MESSAGE);
            }
            _ => panic!("expected the pack-lsp subcommand"),
        }
    }

    #[test]
    fn pack_lsp_subcommand_accepts_a_custom_message_bound() {
        let cli =
            Cli::try_parse_from(["syntact", "pack-lsp", "--max-message", "2048"]).expect("parses");
        match cli.command {
            Command::PackLsp { max_message } => assert_eq!(max_message, 2048),
            _ => panic!("expected the pack-lsp subcommand"),
        }
    }
}
