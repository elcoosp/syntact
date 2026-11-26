//! `syntact pgo` — the PGO pipeline emission (roadmap §6.1 pass 13, ADR-014).
//!
//! The compiler pipeline's 13th pass — "PGO wiring: instrument profile
//! corpus, emit cargo pgo scripts, merge profiles" — as a
//! registry-routed CLI capability (the issue 046 conventions): the
//! caller names a registered pack and its profile corpus (the §5.2
//! `profile/` idea — representative documents), and [`plan`] returns
//! the complete instrument → train → merge → use → bench pipeline as
//! deterministic artifacts:
//!
//! - `manifest.kdl` — the pipeline data of record: pack, instrumented
//!   target, profile dir, profdata path, phase list, corpus.
//! - `pgo.sh` — one phase per cargo/LLVM step, ready to run from the
//!   repository root (`sh <out>/pgo.sh instrument`, then the rest in
//!   order).
//!
//! Training reuses the existing engine end-to-end: the instrumented
//! `syntact` binary drives its `conformance` command over the corpus
//! through the named pack, so the profile lands on the real parse path
//! (registry routing, recovery, event stream) — no separate training
//! harness. [`plan`] is pure (no filesystem); I/O lives in [`emit`],
//! and the report rendering in [`render_json`] — the
//! [`trace`](crate::trace) split. Emission is byte-stable for identical
//! inputs (ADR-013: no timestamps, fixed ordering, quoted literals), so
//! a re-run of the same command cannot churn a tracked artifact tree.
//!
//! # Refusals
//!
//! Every invalid input is a named [`PgoError`] — the pipeline is never
//! silently narrowed: unknown packs refuse with the registered list
//! (the registry-routing convention), an empty corpus refuses, and
//! corpus paths that would break the emitted shell script (whitespace,
//! shell metacharacters) refuse instead of silently mis-training.

use std::fmt::{self, Write as _};

use pack_registry::PackRegistry;

/// The instrumented cargo target: the `syntact` binary pulls in every
/// pack through the registry, so training it over a corpus profiles the
/// real parse path end-to-end.
const TARGET: &str = "syntact";

/// Pack → criterion suite in `crates/bench` (the `bench` phase needs a
/// registered suite to drive; `json` is the flagship today). A pack
/// without a suite plans the four profile phases without the bench
/// phase instead of emitting a script that cannot run.
const BENCH_SUITES: &[(&str, &str)] = &[("json", "json")];

/// The profile phases in execution order; `bench` is appended when the
/// pack has a criterion suite.
const PROFILE_PHASES: [&str; 4] = ["instrument", "train", "merge", "use"];

/// One pack's PGO pipeline: the plan (pure data) plus the two rendered
/// artifacts, byte-stable for identical inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgoPlan {
    /// The pack the pipeline drives.
    pub pack: String,
    /// The instrumented cargo target (the `syntact` binary).
    pub target: String,
    /// The criterion suite the `bench` phase drives, when `crates/bench`
    /// registers one for the pack.
    pub bench_suite: Option<String>,
    /// The artifact directory as given (trailing `/` trimmed).
    pub out_dir: String,
    /// The profile corpus, sorted and deduplicated.
    pub corpus: Vec<String>,
    /// Phase names in execution order.
    pub phases: Vec<String>,
    /// The `manifest.kdl` artifact: the pipeline data of record.
    pub manifest: String,
    /// The `pgo.sh` artifact: one phase per cargo/LLVM step.
    pub script: String,
}

/// A named plan refusal (the command layer maps every variant to a
/// usage error carrying this message).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PgoError {
    /// The pack is not registered; the list mirrors `syntact packs`.
    UnknownPack {
        /// The refused name.
        name: String,
        /// The registered pack names.
        registered: Vec<String>,
    },
    /// No corpus documents were given — there is nothing to profile.
    EmptyCorpus,
    /// A path would break the emitted shell script (whitespace or a
    /// shell metacharacter); the pipeline refuses instead of silently
    /// mis-training.
    UnsafePath {
        /// The refused path.
        path: String,
    },
}

impl fmt::Display for PgoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPack { name, registered } => write!(
                f,
                "unknown pack `{name}` (registered: {})",
                registered.join(", ")
            ),
            Self::EmptyCorpus => f.write_str("pgo: no corpus documents given"),
            Self::UnsafePath { path } => write!(
                f,
                "pgo: path `{path}` is not shell-safe; whitespace and shell metacharacters \
                 are refused (the path is baked into the emitted script)"
            ),
        }
    }
}

impl std::error::Error for PgoError {}

/// What [`emit`] wrote, in artifact order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgoArtifacts {
    /// Artifact file names inside the out dir (`manifest.kdl`, `pgo.sh`).
    pub files: Vec<String>,
}

/// Whether a path survives being baked into the emitted shell script:
/// no whitespace, no quote, no escape, no expansion.
fn shell_safe(path: &str) -> bool {
    !path.is_empty()
        && !path.chars().any(|c| {
            c.is_whitespace() || matches!(c, '"' | '\'' | '$' | '`' | '\\' | ';' | '|' | '&')
        })
}

/// Whether the pack has a registered criterion suite in `crates/bench`.
fn bench_suite(pack: &str) -> Option<&'static str> {
    BENCH_SUITES
        .iter()
        .find(|(name, _)| *name == pack)
        .map(|(_, suite)| *suite)
}

/// Plan the PGO pipeline for one registered pack over its profile
/// corpus; pure (no filesystem). The corpus is sorted and deduplicated
/// (ADR-013), the artifacts are rendered deterministically, and every
/// invalid input is a named [`PgoError`].
///
/// # Errors
/// [`PgoError::UnknownPack`] when the pack is not registered,
/// [`PgoError::EmptyCorpus`] when the corpus is empty, and
/// [`PgoError::UnsafePath`] when a path would break the emitted shell
/// script.
pub fn plan(pack: &str, corpus: &[String], out_dir: &str) -> Result<PgoPlan, PgoError> {
    let registry = PackRegistry::reference();
    if registry.get(pack).is_none() {
        return Err(PgoError::UnknownPack {
            name: pack.to_string(),
            registered: registry
                .names()
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
        });
    }
    if corpus.is_empty() {
        return Err(PgoError::EmptyCorpus);
    }
    let mut corpus = corpus.to_vec();
    corpus.sort();
    corpus.dedup();
    for path in &corpus {
        if !shell_safe(path) {
            return Err(PgoError::UnsafePath { path: path.clone() });
        }
    }
    let out_dir = out_dir.trim_end_matches('/').to_string();
    if !shell_safe(&out_dir) {
        return Err(PgoError::UnsafePath { path: out_dir });
    }
    let suite = bench_suite(pack);
    let mut phases: Vec<String> = PROFILE_PHASES.iter().copied().map(str::to_string).collect();
    if suite.is_some() {
        phases.push("bench".to_string());
    }
    let profile_dir = format!("{out_dir}/profiles");
    let profdata = format!("{out_dir}/{TARGET}.profdata");
    let manifest = render_manifest(pack, suite, &profile_dir, &profdata, &out_dir, &corpus);
    let script = render_script(pack, suite, &profile_dir, &profdata, &out_dir, &corpus);
    Ok(PgoPlan {
        pack: pack.to_string(),
        target: TARGET.to_string(),
        bench_suite: suite.map(str::to_string),
        corpus,
        phases,
        manifest,
        script,
        out_dir,
    })
}

/// Write the two artifacts into the out dir (created if missing).
///
/// # Errors
/// The filesystem errors of creating the directory or writing either
/// artifact, propagated from [`std::fs`].
pub fn emit(plan: &PgoPlan) -> std::io::Result<PgoArtifacts> {
    let root = std::path::Path::new(&plan.out_dir);
    std::fs::create_dir_all(root)?;
    std::fs::write(root.join("manifest.kdl"), &plan.manifest)?;
    std::fs::write(root.join("pgo.sh"), &plan.script)?;
    Ok(PgoArtifacts {
        files: vec!["manifest.kdl".to_string(), "pgo.sh".to_string()],
    })
}

/// The command summary per phase (manifest comment + script header).
fn phase_summaries(
    suite: Option<&str>,
    profile_dir: &str,
    profdata: &str,
) -> Vec<(&'static str, String)> {
    let mut lines = vec![
        (
            "instrument",
            format!(
                "RUSTFLAGS='-Cprofile-generate={profile_dir}' cargo build --release -p {TARGET}"
            ),
        ),
        (
            "train",
            format!("target/release/{TARGET} conformance <corpus> --pack <pack>"),
        ),
        (
            "merge",
            format!("llvm-profdata merge -o {profdata} {profile_dir}/*.profraw"),
        ),
        (
            "use",
            format!(
                "RUSTFLAGS='-Cprofile-use={profdata} -Cllvm-args=-pgo-warn-missing-function' \
                 cargo build --release -p {TARGET} -p bench"
            ),
        ),
    ];
    if let Some(suite) = suite {
        lines.push((
            "bench",
            format!("RUSTFLAGS='-Cprofile-use={profdata}' cargo bench -p bench --bench {suite}"),
        ));
    }
    lines
}

/// Render the `manifest.kdl` artifact.
fn render_manifest(
    pack: &str,
    suite: Option<&str>,
    profile_dir: &str,
    profdata: &str,
    out_dir: &str,
    corpus: &[String],
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// PGO manifest — emitted by `syntact pgo` (roadmap §6.1 pass 13, ADR-014)."
    );
    let _ = writeln!(
        out,
        "// Deterministic: the same pack, corpus, and out dir emit the same bytes."
    );
    let _ = writeln!(out, "pgo {{");
    let _ = writeln!(out, "    pack \"{pack}\"");
    let _ = writeln!(out, "    instrumented \"{TARGET}\"");
    if let Some(suite) = suite {
        let _ = writeln!(out, "    bench-suite \"{suite}\"");
    }
    let _ = writeln!(out, "    profile-dir \"{profile_dir}\"");
    let _ = writeln!(out, "    profdata \"{profdata}\"");
    let _ = writeln!(out, "    out-dir \"{out_dir}\"");
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "// One `phase` per cargo/LLVM step, in execution order; run them from the"
    );
    let _ = writeln!(out, "// repository root (`sh {out_dir}/pgo.sh <phase>`).");
    for (name, summary) in phase_summaries(suite, profile_dir, profdata) {
        let _ = writeln!(out, "phase \"{name}\" // {summary}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "// The profile corpus (§5.2 `profile/` idea):");
    let _ = writeln!(out, "corpus {{");
    for path in corpus {
        let _ = writeln!(out, "    file \"{path}\"");
    }
    let _ = writeln!(out, "}}");
    out
}

/// Render the `pgo.sh` artifact: one phase per cargo/LLVM step.
fn render_script(
    pack: &str,
    suite: Option<&str>,
    profile_dir: &str,
    profdata: &str,
    out_dir: &str,
    corpus: &[String],
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "#!/bin/sh");
    let _ = writeln!(
        out,
        "# syntact pgo — PGO pipeline for the `{pack}` pack (roadmap §6.1 pass 13, ADR-014)."
    );
    let _ = writeln!(
        out,
        "# Emitted by: syntact pgo --pack {pack} --out {out_dir}"
    );
    let _ = writeln!(out, "# Run each phase from the repository root, in order:");
    for (name, summary) in phase_summaries(suite, profile_dir, profdata) {
        let _ = writeln!(out, "#   {name}: {summary}");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "set -eu");
    let _ = writeln!(out);
    let _ = writeln!(out, "usage() {{");
    let phase_args = if suite.is_some() {
        "instrument|train|merge|use|bench"
    } else {
        "instrument|train|merge|use"
    };
    let _ = writeln!(
        out,
        "    printf 'usage: sh {out_dir}/pgo.sh <{phase_args}>\\n' >&2"
    );
    let _ = writeln!(out, "    exit 2");
    let _ = writeln!(out, "}}");
    let _ = writeln!(out);
    let _ = writeln!(out, "[ \"$#\" -eq 1 ] || usage");
    let _ = writeln!(out);
    let _ = writeln!(out, "case \"$1\" in");
    let _ = writeln!(out, "    instrument)");
    let _ = writeln!(
        out,
        "        RUSTFLAGS=\"-Cprofile-generate={profile_dir}\" cargo build --release -p {TARGET}"
    );
    let _ = writeln!(out, "        ;;");
    let _ = writeln!(out, "    train)");
    let _ = writeln!(out, "        ./target/release/{TARGET} conformance \\");
    for path in corpus {
        let _ = writeln!(out, "            \"{path}\" \\");
    }
    let _ = writeln!(out, "            --pack {pack}");
    let _ = writeln!(out, "        ;;");
    let _ = writeln!(out, "    merge)");
    let _ = writeln!(
        out,
        "        command -v llvm-profdata >/dev/null 2>&1 || {{"
    );
    let _ = writeln!(
        out,
        "            printf 'llvm-profdata not found; install it with `rustup component add \
         llvm-tools-preview`\\n' >&2"
    );
    let _ = writeln!(out, "            exit 1");
    let _ = writeln!(out, "        }}");
    let _ = writeln!(
        out,
        "        llvm-profdata merge -o {profdata} {profile_dir}/*.profraw"
    );
    let _ = writeln!(out, "        ;;");
    let _ = writeln!(out, "    use)");
    let _ = writeln!(
        out,
        "        RUSTFLAGS=\"-Cprofile-use={profdata} -Cllvm-args=-pgo-warn-missing-function\" \
         cargo build --release -p {TARGET} -p bench"
    );
    let _ = writeln!(out, "        ;;");
    if let Some(suite) = suite {
        let _ = writeln!(out, "    bench)");
        let _ = writeln!(
            out,
            "        RUSTFLAGS=\"-Cprofile-use={profdata}\" cargo bench -p bench --bench {suite}"
        );
        let _ = writeln!(out, "        ;;");
    }
    let _ = writeln!(out, "    *)");
    let _ = writeln!(out, "        usage");
    let _ = writeln!(out, "        ;;");
    let _ = writeln!(out, "esac");
    out
}

/// Render the machine-readable report (`--json`), in the `trace`
/// hand-rolled style.
#[must_use]
pub fn render_json(plan: &PgoPlan) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{{");
    let _ = writeln!(out, "  \"pack\": {},", json_str(&plan.pack));
    let _ = writeln!(out, "  \"instrumented\": {},", json_str(&plan.target));
    let bench = match plan.bench_suite.as_deref() {
        Some(suite) => json_str(suite),
        None => "null".to_string(),
    };
    let _ = writeln!(out, "  \"bench_suite\": {bench},");
    let _ = writeln!(out, "  \"out_dir\": {},", json_str(&plan.out_dir));
    let corpus = plan
        .corpus
        .iter()
        .map(|path| json_str(path))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "  \"corpus\": [{corpus}],");
    let _ = writeln!(out, "  \"artifacts\": [\"manifest.kdl\", \"pgo.sh\"],");
    let phases = plan
        .phases
        .iter()
        .map(|phase| json_str(phase))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(out, "  \"phases\": [{phases}]");
    let _ = writeln!(out, "}}");
    out
}

/// Escape one string as a JSON string literal.
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
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
