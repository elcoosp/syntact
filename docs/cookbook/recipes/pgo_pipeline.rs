// Recipe: emit the PGO pipeline for a pack.
//
// `syntact pgo` (roadmap §6.1 pass 13, ADR-014) turns a profile corpus
// into a deterministic instrument → train → merge → use → bench
// artifact set for any registered pack. This recipe drives the library
// seam (`cli::pgo`) and pins the pass-13 obligations in the emitted
// commands.

use cli::pgo;

#[test]
fn emit_the_pgo_pipeline() {
    // The profile corpus: representative documents for the pack (the
    // §5.2 `profile/` idea). The planner sorts and deduplicates it and
    // refuses paths the emitted shell script could not carry.
    let corpus = vec![
        "profile/real.json".to_string(),
        "profile/synthetic.json".to_string(),
        "profile/real.json".to_string(),
    ];

    // Plan: registry-routed and pure — no filesystem, byte-stable for
    // identical inputs (ADR-013).
    let plan = pgo::plan("json", &corpus, "pgo/json").expect("the json pack plans");
    assert_eq!(plan.corpus, ["profile/real.json", "profile/synthetic.json"]);
    assert_eq!(plan.phases, ["instrument", "train", "merge", "use", "bench"]);

    // The pass-13 obligations ("instrument profile corpus, emit cargo
    // pgo scripts, merge profiles"), visible in the emitted commands:
    assert!(plan.script.contains("-Cprofile-generate=pgo/json/profiles"));
    assert!(plan.script.contains("syntact conformance"));
    assert!(plan.script.contains("--pack json"));
    assert!(plan.script.contains("llvm-profdata merge"));
    assert!(plan.script.contains("-Cprofile-use=pgo/json/syntact.profdata"));

    // Emit into a scratch dir (the CLI writes wherever `--out` names);
    // the artifacts are the manifest and the phased script.
    let out = std::env::temp_dir().join(format!("syntact-recipe-pgo-{}", std::process::id()));
    let plan = pgo::plan(
        "json",
        &corpus,
        out.to_str().expect("utf-8 temp path"),
    )
    .expect("the json pack plans");
    let artifacts = pgo::emit(&plan).expect("emit the artifacts");
    assert_eq!(artifacts.files, ["manifest.kdl", "pgo.sh"]);
    assert!(out.join("pgo.sh").exists());

    // The machine-readable report (`syntact pgo --json`) mirrors the
    // plan: pack, corpus, artifacts, phases.
    assert!(pgo::render_json(&plan).contains("\"pack\": \"json\""));
}
