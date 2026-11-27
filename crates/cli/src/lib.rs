//! # cli — the `syntact` command line (issue 020)
//!
//! Subcommands over the real pipeline (everything that exists today, no
//! stubs):
//!
//! - `check <GRAMMAR.gir | DOCUMENT> [--pack <PACK>]` — grammar
//!   validation (KDL → IR) or a document parse check routed through the
//!   shared pack registry: the file extension picks the pack when
//!   `--pack` is absent (stdin defaults to JSON), and unknown
//!   extensions refuse with the registered pack list (issue 046).
//! - `build <GRAMMAR.gir> [-o <DIR>]` — compile the front half of the
//!   pipeline (KDL → IR, lexer compilation, LR(1) shadow automaton) and
//!   print the per-stage stats; with `-o <DIR>`, emit a standalone,
//!   compiling Rust parser crate (the codegen-rust descent backend,
//!   issue 045 / roadmap §6.1 pass 12 / §12.3 P0 "compiling crate" row).
//! - `conflicts <GRAMMAR.gir>` — the determinism report: per-rule
//!   backend classification plus every LL(1) conflict site.
//! - `conformance <FILES…> [--pack <PACK>]` — run documents through a
//!   registered pack (default `json`) and gate byte-faithfulness
//!   (`render(parse(x)) == x`).
//! - `packs` — list the registered packs: name, language id, extensions
//!   (the pack-registry seam, issue 046).
//! - `trace <PACK> <FILE>` — per-region recovery events (offset, kind,
//!   repair ops, resync point) for every registered pack: JSON keeps
//!   the shadow-automaton CPCT+ repair loop, the other nine packs report
//!   their reference-parser recovery regions (issue 029, issue 046).
//! - `lsp` — the `.gir` grammar language server over stdio
//!   (JSON-RPC 2.0, LSP 3.17 subset; issue 041).
//! - `pack-lsp` — the pack language server over stdio: diagnostics,
//!   outline symbols, folding, semantic tokens, highlights, and
//!   selection ranges for the ten shipped pack languages (issue 042).
//! - `pgo <FILES…> [--pack <PACK>] --out <DIR>` — the PGO pipeline
//!   emission (roadmap §6.1 pass 13, ADR-014): a deterministic
//!   instrument → train → merge → use → bench artifact set over the
//!   profile corpus for any registered pack.
//! - `import <GRAMMAR.js> [-o <FILE>]` — the tree-sitter adoption wedge
//!   (roadmap §9.1, ADR-015): convert a `grammar.js` into gir-surface
//!   KDL through the importer (issue 037), gated to load back through
//!   `gir-syntax`, with the mapped/dropped/why conversion report.
//! - `test-all <GRAMMAR.gir> <FILES…>` — the cross-backend differential
//!   oracle (roadmap §7.4 "Cross-backend oracle", §11.1 #4): descent vs
//!   the table-driven LR(1) driver over the corpus; the backends must
//!   agree on every document (`--json` for machine-readable output).
//!
//! [`run`] is a pure function from arguments to an exit code, so the
//! integration tests drive it directly.

use std::io::Read as _;

use clap::{Parser as ClapParser, Subcommand};
use gir_ir::Grammar;
use pack_registry::PackRegistry;
use runtime_core::input::Source;

pub mod import;
pub mod pgo;
pub mod testall;
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
    /// Validate a grammar (.gir KDL) or a document through the pack its
    /// extension selects (`--pack` overrides; `-` defaults to JSON).
    Check {
        /// The file to check (`-` reads stdin).
        #[arg(value_name = "FILE")]
        file: String,
        /// The pack to check through (default: the file extension's
        /// registered pack; stdin and extensionless input default to
        /// `json`).
        #[arg(short = 'p', long = "pack", value_name = "PACK")]
        pack: Option<String>,
    },
    /// Compile a grammar through the pipeline front half (loader, lexer,
    /// shadow automaton) and report the stats — or, with `-o <DIR>`, emit
    /// a standalone compiling Rust parser crate (the codegen-rust descent
    /// backend, issue 045 / roadmap §6.1 pass 12 / §12.3 P0 "compiling
    /// crate" row).
    Build {
        /// The .gir grammar file.
        #[arg(value_name = "GRAMMAR")]
        grammar: String,
        /// Emit the generated parser crate into this directory (created if
        /// missing). When omitted, the front-half stats are printed instead.
        #[arg(short = 'o', long = "out", value_name = "DIR")]
        out: Option<String>,
    },
    /// Print the determinism report: per-rule backends + conflict sites.
    Conflicts {
        /// The .gir grammar file.
        #[arg(value_name = "GRAMMAR")]
        grammar: String,
    },
    /// Run documents through a registered pack and gate
    /// byte-faithfulness.
    Conformance {
        /// Documents to check (`-` reads stdin once).
        #[arg(value_name = "FILE", num_args = 1..)]
        files: Vec<String>,
        /// The pack to run (default `json`).
        #[arg(short = 'p', long = "pack", value_name = "PACK")]
        pack: Option<String>,
    },
    /// List the registered packs: name, language id, extensions.
    Packs,
    /// Print per-region recovery events for a document through a pack.
    Trace {
        /// The pack to trace through (any registered pack name; see
        /// `syntact packs`).
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
    /// Emit the PGO pipeline for a registered pack: instrument, train,
    /// merge, and profile-use scripts over the profile corpus (roadmap
    /// §6.1 pass 13, ADR-014).
    Pgo {
        /// Profile-corpus documents to train on (the §5.2 `profile/`
        /// idea; stdin (`-`) cannot be baked into the emitted pipeline).
        #[arg(value_name = "FILE", num_args = 1..)]
        files: Vec<String>,
        /// The pack to drive (default `json`).
        #[arg(short = 'p', long = "pack", value_name = "PACK")]
        pack: Option<String>,
        /// Directory for the emitted artifacts (created if missing).
        #[arg(short = 'o', long = "out", value_name = "DIR")]
        out: Option<String>,
        /// Emit machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
    },
    /// Import a tree-sitter `grammar.js` into the gir surface (roadmap
    /// §9.1, ADR-015): the converted KDL goes to stdout or `-o <FILE>`,
    /// with the conversion report (mapped/dropped/why).
    Import {
        /// The tree-sitter grammar.js file (`-` reads stdin).
        #[arg(value_name = "GRAMMAR_JS")]
        file: String,
        /// Write the converted KDL grammar here instead of stdout.
        #[arg(short = 'o', long = "out", value_name = "FILE")]
        out: Option<String>,
        /// Print the conversion report as JSON instead of the grammar.
        #[arg(long)]
        json: bool,
    },
    /// Differential-test the descent engine against the table-driven
    /// LR(1) backend over a corpus through one grammar (roadmap §7.4
    /// "Cross-backend oracle", §11.1 #4): the backends must agree on
    /// every document.
    TestAll {
        /// The .gir grammar file.
        #[arg(value_name = "GRAMMAR")]
        grammar: String,
        /// Corpus documents to differential-test (`-` reads stdin once).
        #[arg(value_name = "FILE", num_args = 1..)]
        files: Vec<String>,
        /// Emit machine-readable JSON instead of text.
        #[arg(long)]
        json: bool,
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
        Command::Check { file, pack } => cmd_check(&file, pack.as_deref()),
        Command::Build { grammar, out } => cmd_build(&grammar, out.as_deref()),
        Command::Conflicts { grammar } => cmd_conflicts(&grammar),
        Command::Conformance { files, pack } => cmd_conformance(&files, pack.as_deref()),
        Command::Packs => cmd_packs(),
        Command::Trace { pack, file, json } => cmd_trace(&pack, &file, json),
        Command::Lsp { max_message } => cmd_lsp(max_message),
        Command::PackLsp { max_message } => cmd_pack_lsp(max_message),
        Command::Pgo {
            files,
            pack,
            out,
            json,
        } => cmd_pgo(&files, pack.as_deref(), out.as_deref(), json),
        Command::Import { file, out, json } => cmd_import(&file, out.as_deref(), json),
        Command::TestAll {
            grammar,
            files,
            json,
        } => cmd_testall(&grammar, &files, json),
    })
}

/// The shared reference registry for command dispatch.
fn registry() -> PackRegistry {
    PackRegistry::reference()
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

/// Emit the PGO pipeline for a pack over its profile corpus (roadmap
/// §6.1 pass 13, ADR-014): plan (registry-routed, named refusals), then
/// write `manifest.kdl` + `pgo.sh` into the out dir.
fn cmd_pgo(files: &[String], pack: Option<&str>, out: Option<&str>, json: bool) -> i32 {
    if files.is_empty() {
        eprintln!("pgo: no corpus documents given");
        return USAGE;
    }
    let Some(out_dir) = out else {
        eprintln!("pgo: --out <DIR> is required (the artifacts have to land somewhere)");
        return USAGE;
    };
    // Stdin has no path to bake into the emitted train phase, and every
    // corpus file must exist now: the plan hardcodes the paths.
    if files.iter().any(|file| file.as_str() == "-") {
        eprintln!(
            "pgo: corpus `-` (stdin) cannot be baked into the emitted pipeline; pass file paths"
        );
        return USAGE;
    }
    for file in files {
        if std::fs::metadata(file).is_err() {
            eprintln!("{file}: unreadable");
            return USAGE;
        }
    }
    let plan = match pgo::plan(pack.unwrap_or("json"), files, out_dir) {
        Ok(plan) => plan,
        Err(error) => {
            eprintln!("{error}");
            return USAGE;
        }
    };
    match pgo::emit(&plan) {
        Ok(artifacts) => {
            if json {
                print!("{}", pgo::render_json(&plan));
            } else {
                println!(
                    "emitted the pgo pipeline for `{}` → {} ({})",
                    plan.pack,
                    plan.out_dir,
                    artifacts.files.join(", ")
                );
                println!("phases: {}", plan.phases.join(" → "));
                println!(
                    "train on {} corpus document(s); next: sh {}/pgo.sh instrument",
                    plan.corpus.len(),
                    plan.out_dir
                );
            }
            OK
        }
        Err(error) => {
            eprintln!("{}: {error}", plan.out_dir);
            USAGE
        }
    }
}

/// Differential-test the descent engine against the table-driven LR(1)
/// backend over a corpus (roadmap §7.4): the seam plans and runs, the
/// command reads the grammar and the corpus documents and renders the
/// verdict — OK when every document agrees on an independent second run.
fn cmd_testall(grammar_path: &str, files: &[String], json: bool) -> i32 {
    if files.is_empty() {
        eprintln!("test-all: no corpus documents given");
        return USAGE;
    }
    let Ok(grammar_text) = read_input(grammar_path) else {
        return USAGE;
    };
    let mut corpus = Vec::with_capacity(files.len());
    for file in files {
        match read_input(file) {
            Ok(source) => corpus.push(testall::CorpusDoc {
                name: if file == "-" {
                    "stdin".to_owned()
                } else {
                    file.clone()
                },
                source,
            }),
            Err(error) => {
                eprintln!("{error}");
                return USAGE;
            }
        }
    }
    match testall::run(&grammar_text, &corpus) {
        Ok(report) => {
            if json {
                print!("{}", testall::render_json(&report));
            } else {
                println!(
                    "test-all: grammar `{}` — descent vs table (LR(1)) over {} document(s)",
                    report.grammar,
                    report.documents.len()
                );
                for document in &report.documents {
                    let verdict = if document.agree { "ok  " } else { "FAIL" };
                    print!(
                        "{verdict} {:<24} {:>5} B {:>4} tokens  ",
                        document.name, document.bytes, document.tokens
                    );
                    if document.descent_valid && document.lr_accepted {
                        println!("both accept");
                    } else if !document.descent_valid && !document.lr_accepted {
                        print!(
                            "both reject (descent {} error(s); table: ",
                            document.descent_errors
                        );
                        match &document.lr_error {
                            Some(error) => println!(
                                "expected {} at byte {})",
                                error.expected.join(" "),
                                error.offset
                            ),
                            None => println!("rejected)"),
                        }
                    } else {
                        println!(
                            "DISAGREE (descent errors={}, table accepted={})",
                            document.descent_errors, document.lr_accepted
                        );
                    }
                }
                println!(
                    "verdict: {}/{} agree, {} — the backends {} each other honest",
                    report.agreements,
                    report.documents.len(),
                    if report.deterministic {
                        "deterministic"
                    } else {
                        "NON-DETERMINISTIC"
                    },
                    if report.disagreements == 0 {
                        "keep"
                    } else {
                        "do NOT keep"
                    }
                );
            }
            if report.disagreements == 0 && report.deterministic {
                OK
            } else {
                FAIL
            }
        }
        Err(error) => {
            eprintln!("{error}");
            USAGE
        }
    }
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

fn cmd_check(file: &str, pack: Option<&str>) -> i32 {
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
    // Document check through the registry (issue 046): `--pack` wins,
    // else the file extension's registered pack; `-` (stdin) has no
    // extension, so it keeps the historical JSON default. Unknown
    // extensions refuse with the registered list instead of guessing a
    // parser.
    let registry = registry();
    let Some(entry) = resolve_pack(&registry, pack, file) else {
        return USAGE;
    };
    // The pack recovers, so the verdict is the syntax-error count from
    // parse_events, not the Result.
    let (_events, _ranges, errors) = (entry.parse_events)(&text);
    if errors == 0 {
        println!("document ok: {} bytes", text.len());
        OK
    } else {
        eprintln!("document invalid: {errors} syntax error(s)");
        FAIL
    }
}

/// Resolve the pack for a document command: explicit `--pack` first,
/// then the file extension, then the `json` default for stdin. Prints
/// the named refusal and returns `None` when nothing resolves.
fn resolve_pack<'r>(
    registry: &'r PackRegistry,
    pack: Option<&str>,
    file: &str,
) -> Option<&'r pack_registry::PackEntry> {
    if let Some(name) = pack {
        let Some(entry) = registry.get(name) else {
            eprintln!(
                "unknown pack `{name}` (registered: {})",
                registry.names().join(", ")
            );
            return None;
        };
        return Some(entry);
    }
    if file == "-" {
        // Stdin carries no extension; keep the historical JSON default.
        return registry.get("json");
    }
    let extension = std::path::Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str());
    let Some(extension) = extension else {
        eprintln!(
            "no extension on `{file}`; pass --pack (registered: {})",
            registry.names().join(", ")
        );
        return None;
    };
    let Some(entry) = registry.for_extension(extension) else {
        eprintln!(
            "unknown document extension `.{extension}` (registered: {})",
            registry.names().join(", ")
        );
        return None;
    };
    Some(entry)
}

fn cmd_build(grammar_path: &str, out: Option<&str>) -> i32 {
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
    // Stage 4: determinism classification, with the §6.2 ambiguity
    // marks attributed from the automaton's recorded conflicts.
    let report = gir_analysis::DeterminismReport::compute_with_ambiguities(&grammar, &automaton);

    // When `-o <DIR>` is given, emit the compiling parser crate (issue 045,
    // roadmap §6.1 pass 12 / §12.3 P0 "compiling crate" row). Lowering
    // refusals surface as named FAIL diagnostics — the compiler never
    // silently skips emission.
    if let Some(out_dir) = out {
        let lowered = match descent_lower::lower(&grammar) {
            Ok(lowered) => lowered,
            Err(error) => {
                eprintln!("lowering refused: {error}");
                return FAIL;
            }
        };
        let crate_ =
            match codegen_rust::emit_crate(&lowered, &scanner, &grammar.name, &grammar.version) {
                Ok(crate_) => crate_,
                Err(error) => {
                    eprintln!("emission refused: {error}");
                    return FAIL;
                }
            };
        let root = std::path::Path::new(out_dir);
        if let Err(error) = std::fs::create_dir_all(root) {
            eprintln!("{out_dir}: {error}");
            return USAGE;
        }
        for file in &crate_.files {
            let path = root.join(&file.path);
            if let Some(parent) = path.parent() {
                if let Err(error) = std::fs::create_dir_all(parent) {
                    eprintln!("{}: {error}", path.display());
                    return USAGE;
                }
            }
            if let Err(error) = std::fs::write(&path, &file.contents) {
                eprintln!("{}: {error}", path.display());
                return USAGE;
            }
        }
        println!(
            "emitted `{}` v{} → {} ({} files, crate `{}`)",
            grammar.name,
            grammar.version,
            root.display(),
            crate_.files.len(),
            crate_.name,
        );
        return OK;
    }

    // No `-o`: keep today's stats output (the front-half summary).
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
    // Token-class inference (roadmap §6.1 pass 7): the never-
    // distinguished grouping over the resolved table — declared tokens
    // no reachable state acts on (dead/reserved tokens) collapse here.
    let classes = lr_shadow::TokenClasses::compute(&automaton, &grammar);
    println!(
        "token classes: {} tokens -> {} classes ({} never distinguished)",
        classes.token_count(),
        classes.len(),
        classes.merged(),
    );
    let reachable = report
        .verdicts
        .iter()
        .filter(|verdict| verdict.reachable)
        .count();
    println!("determinism: {reachable} reachable rules classified");
    // Ambiguity marks (roadmap §6.2, ADR-007): regions the shadow
    // automaton resolved by policy, with the island candidates the
    // `@allow_ambiguous` annotation accepted.
    let islands = report
        .ambiguities
        .iter()
        .filter(|ambiguity| ambiguity.allowed)
        .count();
    if !report.ambiguities.is_empty() {
        println!(
            "ambiguities: {} region(s) recorded; islands: {} accepted (@allow_ambiguous)",
            report.ambiguities.len(),
            islands
        );
    }
    OK
}

fn cmd_conflicts(grammar_path: &str) -> i32 {
    let Ok(text) = read_input(grammar_path) else {
        return USAGE;
    };
    let Ok(grammar) = load_grammar(&text) else {
        return FAIL;
    };
    // The report rides the automaton so ambiguity marks (roadmap §6.2,
    // ADR-007) render with their two counter-example derivations.
    let automaton = lr_shadow::Automaton::build(&grammar);
    let report = gir_analysis::DeterminismReport::compute_with_ambiguities(&grammar, &automaton);
    print!("{report}");
    let islands = report
        .ambiguities
        .iter()
        .filter(|ambiguity| ambiguity.allowed)
        .count();
    if islands > 0 {
        println!("island candidates: {islands} region(s) accepted (@allow_ambiguous)");
    }
    if report.all_deterministic() {
        println!("all reachable rules deterministic (descent/pratt)");
        OK
    } else {
        println!("table fallback present");
        FAIL
    }
}

fn cmd_conformance(files: &[String], pack: Option<&str>) -> i32 {
    if files.is_empty() {
        eprintln!("conformance: no documents given");
        return USAGE;
    }
    // The pack comes from the registry (issue 046): default `json`,
    // `--pack <NAME>` picks any registered pack; unknown names refuse
    // with the registered list.
    let registry = registry();
    let name = pack.unwrap_or("json");
    let Some(entry) = registry.get(name) else {
        eprintln!(
            "unknown pack `{name}` (registered: {})",
            registry.names().join(", ")
        );
        return USAGE;
    };
    let mut failures = 0;
    for file in files {
        let Ok(text) = read_input(file) else {
            eprintln!("{file}: unreadable");
            failures += 1;
            continue;
        };
        let (_, _ranges, errors) = (entry.parse_events)(&text);
        let faithful = (entry.parse)(&text).is_ok_and(|tree| {
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

/// List the registered packs: one line per pack in registration order
/// (deterministic), sourced from the pack-registry seam (issue 046).
fn cmd_packs() -> i32 {
    let registry = registry();
    println!("registered packs ({}):", registry.len());
    for entry in registry.entries() {
        let extensions = entry
            .extensions
            .iter()
            .map(|extension| format!(".{extension}"))
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {:<10} language `{:<10}` extensions: {}",
            entry.name, entry.language_id, extensions
        );
    }
    OK
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

/// Import a tree-sitter `grammar.js` (roadmap §9.1, ADR-015): convert
/// through the importer (issue 037), gate the surface through
/// `gir-syntax`, then print or write the KDL with its conversion
/// report.
fn cmd_import(file: &str, out: Option<&str>, json: bool) -> i32 {
    let Ok(text) = read_input(file) else {
        return USAGE;
    };
    let imported = match import::import_document(&text) {
        Ok(imported) => imported,
        Err(error) => {
            eprintln!("{error}");
            return FAIL;
        }
    };
    if json {
        print!("{}", import::render_json(&imported.report));
        return OK;
    }
    if let Some(out_path) = out {
        if let Err(error) = std::fs::write(out_path, &imported.kdl) {
            eprintln!("{out_path}: {error}");
            return USAGE;
        }
        print!("{}", imported.report);
        println!("wrote {out_path} ({} bytes)", imported.kdl.len());
        return OK;
    }
    print!("{}", imported.kdl);
    print!("{}", imported.report);
    OK
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
