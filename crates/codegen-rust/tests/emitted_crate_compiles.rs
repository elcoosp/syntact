//! Integration gate (issue 045): emit a compiling Rust parser crate from
//! the JSON fixture, then run `cargo test` on it — the emitted crate must
//! both **compile** (`forbid(unsafe_code)`, `missing_docs`,
//! `clippy::pedantic`-clean posture) and produce **byte-identical**
//! `(events, tokens, error_count)` to the `descent` engine on the valid +
//! malformed corpus (three-way parity: generated vs engine vs reference),
//! plus **determinism** (emit twice, byte-equal).
//!
//! This is the §12.3 P0 "compiling crate" gate and the §6.1 pass-12
//! codegen exit criterion in one test.

use std::path::{Path, PathBuf};
use std::process::Command;

use codegen_rust::emit_crate;
use descent_lower::{JSON_GRAMMAR, lower};
use gir_syntax::load_grammar;

/// The workspace root (the repo holding this crate): `crates/codegen-rust`
/// is two levels below it, so `CARGO_MANIFEST_DIR/../..` reaches it.
fn workspace_root() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest)
        .ancestors()
        .nth(2)
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Emit the JSON fixture crate into `target/codegen/json/` (a stable path
/// inside the workspace so the emitted `../../../crates/*` deps resolve).
fn emit_into(dir: &Path) {
    let grammar = load_grammar(JSON_GRAMMAR).expect("fixture loads");
    let lowered = lower(&grammar).expect("fixture lowers");
    let scanner = lexgen::compile(&grammar).expect("fixture lexes");
    let crate_ = emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");
    for file in &crate_.files {
        let path = dir.join(&file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("dirs");
        }
        std::fs::write(&path, &file.contents).expect("write emitted file");
    }
    // Append a parity test module: the generated `parse_events` vs the
    // engine on the valid + malformed corpus (the three-way parity gate,
    // with the reference pack as the cross-check the engine already passes).
    let parity_test = r##"
#[cfg(test)]
mod parity {
    use super::*;
    use descent::parse_events;

    const VALID: &[&str] = &[
        "{}", "[]", r#"{"a": 1}"#, "[1, 2, 3]",
        r#"{"name": "syntact", "version": 1, "ok": true, "bits": [true, false, null]}"#,
        "3.14e-2", "-17", r#""str with \u00e9 escape""#, "  [ 1 , 2 ]  ",
        r#"{"a": [1, 2, {"b": null}], "c": true}"#, "1 2", r#""""#, "",
    ];
    const MALFORMED: &[&str] = &["{\"a\": }", "[1, 2", "tru", "@#$", "{\"a\": 1", "[", "}"];

    fn engine_run(source: &str) -> (Vec<runtime_core::events::Event>, Vec<(u16, std::ops::Range<u32>)>, usize) {
        let grammar = gir_syntax::load_grammar(descent_lower::JSON_GRAMMAR).unwrap();
        let lowered = descent_lower::lower(&grammar).unwrap();
        let scanner = lexgen::compile(&grammar).unwrap();
        let tokens = lowered.feed(&scanner, source);
        let run = parse_events(&lowered.desc, &tokens, source);
        (run.events, run.tokens, run.errors)
    }

    #[test]
    fn valid_corpus_events_match_engine() {
        for src in VALID {
            let generated = crate::parse_events(src);
            let eng = engine_run(src);
            assert_eq!(generated.0, eng.0, "events mismatch on {src:?}");
            assert_eq!(generated.1, eng.1, "tokens mismatch on {src:?}");
            assert_eq!(generated.2, eng.2, "error count mismatch on {src:?}");
        }
    }

    #[test]
    fn malformed_corpus_events_match_engine() {
        for src in MALFORMED {
            let generated = crate::parse_events(src);
            let eng = engine_run(src);
            assert_eq!(generated.0, eng.0, "events mismatch on {src:?}");
            assert_eq!(generated.1, eng.1, "tokens mismatch on {src:?}");
            assert_eq!(generated.2, eng.2, "error count mismatch on {src:?}");
        }
    }

    #[test]
    fn valid_corpus_is_byte_faithful() {
        for src in VALID {
            let tree = crate::parse(src).expect("parses");
            let input = runtime_core::input::Source::from_str_unchecked(src);
            assert_eq!(tree.render(&input), *src, "render(parse(x)) == x for {src:?}");
        }
    }
}
"##;
    let lib_path = dir.join("src/lib.rs");
    let mut lib = std::fs::read_to_string(&lib_path).expect("read emitted lib.rs");
    lib.push_str(parity_test);
    std::fs::write(&lib_path, lib).expect("write parity test");

    // Add dev-dependencies the parity test needs.
    let toml_path = dir.join("Cargo.toml");
    let mut toml = std::fs::read_to_string(&toml_path).expect("read emitted Cargo.toml");
    toml.push_str("\n[dev-dependencies]\n");
    toml.push_str("descent = { path = \"../../../crates/descent\" }\n");
    toml.push_str("descent-lower = { path = \"../../../crates/descent-lower\" }\n");
    toml.push_str("gir-syntax = { path = \"../../../crates/gir-syntax\" }\n");
    toml.push_str("lexgen = { path = \"../../../crates/lexgen\" }\n");
    toml.push_str("runtime-core = { path = \"../../../crates/runtime-core\" }\n");
    std::fs::write(&toml_path, toml).expect("write Cargo.toml");
}

/// The compile + parity + determinism gate: emit the crate, emit it again
/// (byte-equal check), then `cargo test` the emitted crate (compile +
/// parity). Skipped when `CI_SKIP_CODEGEN_COMPILE` is set (a local-only
/// escape hatch for environments without a working cargo on the emitted
/// crate's path resolution).
#[test]
fn emitted_crate_compiles_and_matches_engine() {
    let ws = workspace_root();
    let dir = ws.join("target").join("codegen").join("json");
    // Clean any stale emission.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create emit dir");

    // Determinism gate: emit twice, compare bytes.
    emit_into(&dir);
    let first: Vec<(String, String)> = {
        let grammar = load_grammar(JSON_GRAMMAR).expect("loads");
        let lowered = lower(&grammar).expect("lowers");
        let scanner = lexgen::compile(&grammar).expect("lexes");
        let c = emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");
        c.files
            .iter()
            .map(|f| (f.path.clone(), f.contents.clone()))
            .collect()
    };
    let grammar = load_grammar(JSON_GRAMMAR).expect("loads");
    let lowered = lower(&grammar).expect("lowers");
    let scanner = lexgen::compile(&grammar).expect("lexes");
    let second = emit_crate(&lowered, &scanner, "json", "1.0.0").expect("emits");
    for (a, b) in first.iter().zip(second.files.iter()) {
        assert_eq!(a.0, b.path, "path match");
        assert_eq!(a.1, b.contents, "byte-identical emission on {}", b.path);
    }

    // Compile + parity gate: `cargo test` the emitted crate.
    if std::env::var("CI_SKIP_CODEGEN_COMPILE").is_ok() {
        eprintln!("CI_SKIP_CODEGEN_COMPILE set — skipping cargo test on emitted crate");
        return;
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let status = Command::new(cargo)
        .arg("test")
        .current_dir(&dir)
        .status()
        .expect("cargo test on emitted crate");
    assert!(
        status.success(),
        "emitted crate `cargo test` failed (see output above)"
    );
}
