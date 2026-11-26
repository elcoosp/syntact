# The PGO pipeline

The compiler pipeline ends in a thirteenth pass that the other twelve
feed: profile-guided optimization wiring (roadmap §6.1 pass 13, ADR-014
— "PGO/BOLT/LTO pipeline live from the first generated parser"). The
premise is the one the roadmap repeats since v1's own correction:
instrumentation retrofits badly, so the pipeline must carry its PGO
machinery from the start, not bolt it on when a release approaches.
`syntact pgo` is that machinery as a registry-routed command: it takes
a registered pack and its profile corpus — representative documents,
the §5.2 `profile/` idea — and emits the complete optimization
pipeline as two deterministic artifacts, `manifest.kdl` (the data of
record: pack, profile dir, profdata path, phases, corpus) and `pgo.sh`
(one phase per cargo/LLVM step).

The pipeline reuses the engine end-to-end instead of inventing a
training harness: the instrumented `syntact` binary drives its
`conformance` command over the corpus through the named pack, so the
profile lands on the real parse path — registry routing, the descent
engine, recovery, the event stream — exactly the path a release build
runs. Five phases cover the pass-13 obligations: `instrument` rebuilds
the binary with `-Cprofile-generate`, `train` parses the corpus,
`merge` folds the `.profraw` files with `llvm-profdata`, `use` rebuilds
with `-Cprofile-use`, and `bench` re-runs the pack's criterion suite
under the optimized build so the win is measured, not asserted (§10.2:
no optimization without a bench win). The bench phase exists only
where `crates/bench` registers a suite for the pack — `json` today —
so the script can never reference a suite that does not run.

Determinism is the ADR-013 contract carried into the build pipeline:
the same pack, corpus, and out dir emit the same bytes, so the
artifact tree cannot churn between runs. Every invalid input is a
named refusal — unknown packs (with the registered list), empty
corpora, and paths the emitted script could not carry (whitespace,
shell metacharacters) — the pipeline is never silently narrowed.

```rust
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
```

Run the real command after a release build trains the corpus:

```bash
cargo build --release -p syntact
target/release/syntact pgo --pack json --out pgo/json docs/cookbook/recipes/parse_json.rs
sh pgo/json/pgo.sh instrument && sh pgo/json/pgo.sh train
sh pgo/json/pgo.sh merge && sh pgo/json/pgo.sh use && sh pgo/json/pgo.sh bench
```

The library seam behind the command is `cli::pgo`: `plan` is pure
(registry-routed, byte-stable), `emit` writes the artifacts, and
`render_json` produces the `--json` report — the same pure/render
split the trace command uses.
