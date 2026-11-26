//! Example: emit the PGO pipeline for the JSON pack.
//!
//! `syntact pgo` (roadmap §6.1 pass 13, ADR-014) turns a profile corpus
//! into a deterministic instrument → train → merge → use → bench
//! artifact set. This example drives the library seam (`cli::pgo`) over
//! a small fixture corpus, writes the artifacts under `target/`, and
//! prints the pipeline a release build would run.
//!
//! Run: `cargo run -p cli --example pgo_pipeline`

use std::fs;
use std::path::PathBuf;

fn main() {
    // The profile corpus: representative documents for the pack (the
    // §5.2 `profile/` idea). Real runs point at real files; this
    // example writes two fixtures under target/.
    let dir = PathBuf::from("target/pgo-example/corpus");
    fs::create_dir_all(&dir).expect("create corpus dir");
    let docs = [
        ("flat.json", "{\"name\": \"syntact\", \"n\": 42}"),
        (
            "nested.json",
            "{\"list\": [1, 2, 3], \"deep\": {\"ok\": true}}",
        ),
    ];
    let mut corpus = Vec::new();
    for (name, body) in docs {
        let path = dir.join(name);
        fs::write(&path, body).expect("write fixture");
        corpus.push(path.to_string_lossy().into_owned());
    }

    // Plan the pipeline (pure; registry-routed, named refusals)…
    let plan = cli::pgo::plan("json", &corpus, "target/pgo-example").expect("json plans");

    // …emit the artifacts (byte-stable, ADR-013)…
    let artifacts = cli::pgo::emit(&plan).expect("emit artifacts");
    println!(
        "pack `{}` → {} artifacts: {}",
        plan.pack,
        plan.out_dir,
        artifacts.files.join(", ")
    );
    println!("phases: {}", plan.phases.join(" → "));
    println!("corpus (sorted, deduplicated): {:?}", plan.corpus);

    // …and inspect the machine-readable report.
    println!("--json report:\n{}", cli::pgo::render_json(&plan));
}
