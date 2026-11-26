//! `syntact pgo` gates (roadmap §6.1 pass 13, ADR-014): the compiler
//! pipeline's 13th pass — "PGO wiring: instrument profile corpus, emit
//! cargo pgo scripts, merge profiles" — as a registry-routed CLI
//! capability. The acceptance criterion pinned by the first test: the
//! emitted pipeline carries all three obligations of the pass-13 row —
//! an instrument phase over the profile corpus (`-Cprofile-generate`),
//! the cargo command chain as scripts, and the merge step
//! (`llvm-profdata merge`) feeding a `-Cprofile-use` rebuild — for any
//! registered pack, deterministically (ADR-013).

use cli::pgo::{self, PgoError};

/// A uniquely named temp path for this process.
fn temp_path(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "syntact-cli-pgo-{}-{nanos}-{name}",
        std::process::id()
    ))
}

/// The pass-13 pipeline: every phase obligation in emission order.
const PASS13_PHASES: [&str; 5] = ["instrument", "train", "merge", "use", "bench"];

#[test]
fn pgo_plan_pins_the_pass13_pipeline() {
    // Roadmap §6.1 pass 13: "PGO wiring: instrument profile corpus,
    // emit cargo pgo scripts, merge profiles". The plan must carry the
    // instrument command, the training drive over the corpus, the
    // profdata merge, and the profile-use rebuild + bench.
    let plan = pgo::plan(
        "json",
        &["corpus/b.json".into(), "corpus/a.json".into()],
        "pgo/json",
    )
    .expect("json plans");
    assert_eq!(plan.pack, "json");
    assert_eq!(plan.target, "syntact");
    assert_eq!(plan.bench_suite.as_deref(), Some("json"));
    assert_eq!(plan.corpus, vec!["corpus/a.json", "corpus/b.json"]);
    assert_eq!(plan.phases, PASS13_PHASES);

    // Instrument: `-Cprofile-generate` over the profile corpus.
    let script = &plan.script;
    assert!(
        script.contains("-Cprofile-generate=pgo/json/profiles"),
        "instrument phase missing: {script}"
    );
    assert!(
        script.contains("cargo build --release -p syntact"),
        "instrument build missing: {script}"
    );
    // Train: the existing CLI conformance drive over the corpus.
    assert!(script.contains("conformance"), "train drive missing");
    assert!(script.contains("corpus/a.json"), "corpus file missing");
    assert!(script.contains("--pack json"), "pack routing missing");
    // Merge: llvm-profdata.
    assert!(
        script.contains("llvm-profdata merge -o pgo/json/syntact.profdata"),
        "merge phase missing: {script}"
    );
    // Use + measure: `-Cprofile-use` rebuild and the pack bench suite.
    assert!(
        script.contains("-Cprofile-use=pgo/json/syntact.profdata"),
        "profile-use rebuild missing: {script}"
    );
    assert!(
        script.contains("cargo bench -p bench --bench json"),
        "bench phase missing: {script}"
    );

    // The manifest names the pipeline data: pack, profile dir, profdata,
    // phases, and the corpus (the profile corpus of pass 13).
    for needle in [
        "pack \"json\"",
        "pgo/json/profiles",
        "pgo/json/syntact.profdata",
        "instrument",
        "train",
        "merge",
        "use",
        "bench",
        "corpus/a.json",
    ] {
        assert!(plan.manifest.contains(needle), "manifest missing {needle}");
    }
}

#[test]
fn pgo_plan_is_byte_deterministic() {
    // ADR-013: no timestamps, fixed ordering, byte-stable emission.
    let corpus = vec!["docs/b.json".to_string(), "docs/a.json".to_string()];
    let first = pgo::plan("json", &corpus, "pgo/json").expect("plans");
    let second = pgo::plan("json", &corpus, "pgo/json").expect("plans");
    assert_eq!(first, second);
}

#[test]
fn pgo_corpus_is_sorted_and_deduplicated() {
    let corpus = vec![
        "z.json".to_string(),
        "a.json".to_string(),
        "z.json".to_string(),
        "m.json".to_string(),
    ];
    let plan = pgo::plan("json", &corpus, "pgo/json").expect("plans");
    assert_eq!(plan.corpus, vec!["a.json", "m.json", "z.json"]);
}

#[test]
fn pgo_other_packs_plan_without_a_bench_suite() {
    // crates/bench registers criterion suites only for some packs
    // (json today); the bench phase is dropped where no suite exists
    // instead of emitting a script that cannot run.
    let plan = pgo::plan("toml", &["corpus.toml".to_string()], "pgo/toml").expect("plans");
    assert_eq!(plan.bench_suite, None);
    assert_eq!(plan.phases, ["instrument", "train", "merge", "use"]);
    assert!(!plan.script.contains("cargo bench"), "no bench phase");
    assert!(plan.script.contains("--pack toml"));
}

#[test]
fn pgo_refuses_unknown_packs_with_the_registered_list() {
    let error = pgo::plan("nope", &["x.json".to_string()], "pgo/nope").expect_err("refused");
    match &error {
        PgoError::UnknownPack { name, registered } => {
            assert_eq!(name, "nope");
            assert!(registered.contains(&"json".to_string()));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let rendered = error.to_string();
    assert!(rendered.contains("unknown pack `nope`"), "{rendered}");
    assert!(rendered.contains("registered:"), "{rendered}");
}

#[test]
fn pgo_refuses_an_empty_corpus() {
    let error = pgo::plan("json", &[], "pgo/json").expect_err("refused");
    assert!(matches!(error, PgoError::EmptyCorpus), "{error:?}");
    assert!(error.to_string().contains("no corpus documents"));
}

#[test]
fn pgo_refuses_shell_hostile_corpus_paths() {
    // The corpus is baked into the emitted shell script; whitespace and
    // shell metacharacters would silently break the train phase.
    for hostile in [
        "with space.json",
        "quote\".json",
        "dollar$.json",
        "semi;.json",
    ] {
        let error = pgo::plan("json", &[hostile.to_string()], "pgo/json").expect_err("refused");
        assert!(matches!(error, PgoError::UnsafePath { .. }), "{error:?}");
    }
}

#[test]
fn pgo_json_report_is_stable() {
    let plan = pgo::plan("json", &["corpus/a.json".to_string()], "pgo/json").expect("plans");
    let report = pgo::render_json(&plan);
    assert!(report.contains("\"pack\": \"json\""), "{report}");
    assert!(report.contains("\"corpus\": ["), "{report}");
    assert!(report.contains("\"corpus/a.json\""), "{report}");
    assert!(report.contains("\"phases\": ["), "{report}");
    assert!(report.contains("\"bench\""), "{report}");
    let again = pgo::render_json(
        &pgo::plan("json", &["corpus/a.json".to_string()], "pgo/json").expect("plans"),
    );
    assert_eq!(report, again);
}

#[test]
fn cmd_pgo_emits_a_deterministic_artifact_tree() {
    let out_dir = temp_path("out");
    let doc = temp_path("doc.json");
    std::fs::write(&doc, "{\"a\": 1}").expect("corpus fixture");
    let doc_path = doc.to_str().expect("utf-8 temp path").to_string();

    let exit = cli::run([
        "pgo",
        "--pack",
        "json",
        "--out",
        out_dir.to_str().expect("path"),
        &doc_path,
    ])
    .expect("runs");
    assert_eq!(exit, cli::OK);

    let manifest = std::fs::read_to_string(out_dir.join("manifest.kdl")).expect("manifest");
    let script = std::fs::read_to_string(out_dir.join("pgo.sh")).expect("script");
    assert!(manifest.contains("pack \"json\""), "{manifest}");
    assert!(script.contains("conformance"), "{script}");
    assert!(script.contains(&doc_path), "{script}");

    // Determinism on disk: re-emitting into the same directory
    // overwrites both artifacts byte-for-byte (the cross-run
    // byte-determinism of identical inputs is pinned at plan level in
    // `pgo_plan_is_byte_deterministic`; paths are baked in, so two
    // different out dirs differ exactly in their recorded paths).
    let exit =
        cli::run(["pgo", "--out", out_dir.to_str().expect("path"), &doc_path]).expect("runs");
    assert_eq!(exit, cli::OK);
    assert_eq!(
        std::fs::read_to_string(out_dir.join("manifest.kdl")).expect("manifest2"),
        manifest
    );
    assert_eq!(
        std::fs::read_to_string(out_dir.join("pgo.sh")).expect("script2"),
        script
    );
}

#[test]
fn cmd_pgo_defaults_to_the_json_pack() {
    // The registry-routing convention (issue 046): commands default to
    // the json pack; the emitted script names the pack it drives.
    let out_dir = temp_path("default-pack");
    let doc = temp_path("default.json");
    std::fs::write(&doc, "{\"a\": 1}").expect("corpus fixture");
    let exit = cli::run([
        "pgo",
        "--out",
        out_dir.to_str().expect("path"),
        doc.to_str().expect("path"),
    ])
    .expect("runs");
    assert_eq!(exit, cli::OK);
    let manifest = std::fs::read_to_string(out_dir.join("manifest.kdl")).expect("manifest");
    assert!(manifest.contains("pack \"json\""), "{manifest}");
}

#[test]
fn cmd_pgo_refuses_unknown_packs_and_missing_files() {
    let out_dir = temp_path("refuse");
    assert_eq!(
        cli::run([
            "pgo",
            "--pack",
            "nope",
            "--out",
            out_dir.to_str().expect("path"),
            "x.json"
        ])
        .expect("runs"),
        cli::USAGE
    );
    assert_eq!(
        cli::run([
            "pgo",
            "--out",
            out_dir.to_str().expect("path"),
            "/no/such/corpus.json"
        ])
        .expect("runs"),
        cli::USAGE
    );
    assert_eq!(
        cli::run(["pgo", "x.json"]).expect("runs"),
        cli::USAGE,
        "--out is required"
    );
    assert!(!out_dir.exists(), "nothing is written on refusal");
}

#[test]
fn pgo_examples_document_the_phase_chain() {
    // The consumer example (M1 bar) drives the public API end-to-end.
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/examples/pgo_pipeline.rs"
    ))
    .expect("example");
    for needle in ["pgo::plan", "pgo::emit", "render_json"] {
        assert!(source.contains(needle), "example missing {needle}");
    }
}
