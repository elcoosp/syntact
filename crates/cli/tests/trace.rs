//! `syntact trace` integration tests (issue 029 TDD): JSON output shape
//! on a mutated fixture, byte-identical determinism, exit codes.

use cli::trace::{render_json, render_text, trace_json};

/// A scratch file for the exit-code tests.
fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    path.push(format!("syntact-trace-{stamp}-{name}"));
    std::fs::write(&path, content).expect("write fixture");
    path
}

/// The mutated fixture: object closed with `]` instead of `}`.
const MUTATED: &str = "{\"a\": 1 ]";
/// A clean document.
const CLEAN: &str = "{\"a\": [1, 2], \"b\": null}";
/// A long broken document: the repair window cannot see the document
/// tail, so the planner reports no complete repair for the region and
/// the trace records the unrepairable verdict.
fn hopeless() -> String {
    let mut source = String::from("[1");
    for _ in 0..80 {
        source.push_str(", , 1");
    }
    source.push(']');
    source
}

#[test]
fn json_output_shape_on_mutated_fixture() {
    let grammar = harness::metrics::json_fixture();
    let trace = trace_json(MUTATED);
    assert!(!trace.regions.is_empty(), "the broken closer is a region");
    assert!(trace.complete, "one repair closes the gap");
    let json = render_json(&grammar, &trace);
    // Shape contract: bytes, regions with offset/ops/resync, complete.
    assert!(json.starts_with("{\"bytes\":9,\"regions\":["));
    assert!(json.contains("\"ops\":[[\"insert\",\"rbrace\"],[\"delete\",\"rbrack\"]]"));
    assert!(json.contains("\"resync\":9"));
    assert!(json.ends_with(",\"complete\":true}"));
    // Canonical op order (Insert < Delete at the cost tie).
    let grammar = harness::metrics::json_fixture();
    let region = &trace.regions[0];
    assert_eq!(region.offset, 8);
    let expect_look = |name: &str| {
        let token = grammar
            .tokens
            .iter()
            .find(|t| grammar.interner.lookup(t.name) == name)
            .expect("fixture token");
        lr_shadow::Look::token(token.id)
    };
    assert_eq!(
        region.ops,
        vec![
            recovery::RepairOp::Insert(expect_look("rbrace")),
            recovery::RepairOp::Delete(expect_look("rbrack")),
        ]
    );
    assert_eq!(region.resync, 9);
}

#[test]
fn text_output_names_the_repair() {
    let grammar = harness::metrics::json_fixture();
    let trace = trace_json(MUTATED);
    let text = render_text(&grammar, &trace);
    assert!(text.contains("region 1 @ 8: repair [insert rbrace, delete rbrack] → resync @ 9"));
    assert!(text.ends_with("verdict: repaired\n"));
}

#[test]
fn determinism_byte_identical_runs() {
    let grammar = harness::metrics::json_fixture();
    for source in [CLEAN, MUTATED, hopeless().as_str()] {
        let a = trace_json(source);
        let b = trace_json(source);
        assert_eq!(a, b, "trace structure deterministic on {source:?}");
        assert_eq!(render_json(&grammar, &a), render_json(&grammar, &b));
        assert_eq!(render_text(&grammar, &a), render_text(&grammar, &b));
    }
}

#[test]
fn clean_document_has_no_regions() {
    let trace = trace_json(CLEAN);
    assert!(trace.regions.is_empty(), "{:?}", trace.regions);
    assert!(trace.complete);
    assert_eq!(trace.bytes, u32::try_from(CLEAN.len()).expect("small"));
    let grammar = harness::metrics::json_fixture();
    let json = render_json(&grammar, &trace);
    assert_eq!(json, "{\"bytes\":24,\"regions\":[],\"complete\":true}");
}

#[test]
fn hopeless_document_is_unrepaired_with_regions() {
    // More broken regions than the cap: the trace stops at the safety
    // limit and reports the verdict (deterministically).
    let source = hopeless();
    let trace = trace_json(&source);
    let grammar = harness::metrics::json_fixture();
    let text = render_text(&grammar, &trace);
    assert!(text.ends_with("verdict: unrepaired\n"));
    assert_eq!(trace.regions.len(), 1, "the unplannable region");
    assert!(!trace.complete);
    let again = trace_json(&source);
    assert_eq!(render_text(&grammar, &trace), render_text(&grammar, &again));
}

/// The str form of a fixture path.
fn path(p: &std::path::Path) -> &str {
    p.to_str().expect("path")
}

#[test]
fn exit_codes_over_the_cli_entry() {
    let mutated = temp_file("trace-mutated.json", MUTATED);
    let clean = temp_file("trace-clean.json", CLEAN);
    let hopeless = temp_file("trace-hopeless.json", &hopeless());

    assert_eq!(
        cli::run(["trace", "json", path(&mutated)]).expect("runs"),
        cli::OK
    );
    assert_eq!(
        cli::run(["trace", "json", "--json", path(&mutated)]).expect("runs"),
        cli::OK
    );
    assert_eq!(
        cli::run(["trace", "json", path(&clean)]).expect("runs"),
        cli::OK
    );
    assert_eq!(
        cli::run(["trace", "json", path(&hopeless)]).expect("runs"),
        cli::FAIL
    );
    assert_eq!(
        cli::run(["trace", "json", "/no/such/file"]).expect("runs"),
        cli::USAGE
    );
    assert_eq!(
        cli::run(["trace", "toml", path(&clean)]).expect("runs"),
        cli::USAGE
    );

    let _ = std::fs::remove_file(mutated);
    let _ = std::fs::remove_file(clean);
    let _ = std::fs::remove_file(hopeless);
}
