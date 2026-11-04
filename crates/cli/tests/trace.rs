//! trace integration tests (issue 029 TDD): JSON output shape on a
//! mutated fixture document, byte-identical determinism, and exit codes
//! — all through the public [`cli::trace`] engine and [`cli::run`].

use std::time::{SystemTime, UNIX_EPOCH};

use cli::trace::{render_json, render_text, trace};
use cli::{FAIL, OK, USAGE};

/// The calibration fixture shape: `{"a": 1}` with the colon deleted.
const MUTATED: &str = "{\"a\" 1}";
/// The clean counterpart.
const CLEAN: &str = "{\"a\": 1}";
/// EOF failure: the closing brace is missing.
const EOF_BROKEN: &str = "{\"a\": 1";
/// A comma where a value belongs (the pack tolerates it; the shadow
/// automaton flags it).
const TIE: &str = "[1, , 3]";
/// One broken pair whose repair script spans three ops.
const MULTI_OP: &str = "{\"a\" 1 \"b\" 2}";
/// Two separated failures — two regions in ascending order.
const TWO_REGIONS: &str = "{\"a\" 1} [2 , 3] {\"b\": }";
/// A broken TOML document (unterminated array, then an empty pair).
const TOML_BROKEN: &str = "key = [1, \nother = \n";
/// A clean TOML document.
const TOML_CLEAN: &str = "key = 1\n";

#[test]
fn json_output_shape_on_mutated_fixture() {
    let output = trace("json", MUTATED).expect("json trace");
    assert!(!output.ok);
    assert_eq!(output.source_len, 7);
    assert_eq!(output.regions.len(), 1, "regions: {:?}", output.regions);
    let region = &output.regions[0];
    assert_eq!(region.offset, 5);
    assert_eq!(region.kind, "syntax error");
    assert_eq!(region.ops.len(), 1);
    assert_eq!(region.ops[0].op, "insert");
    assert_eq!(region.ops[0].token, "colon");
    assert_eq!(region.resync, 5);
    let expected = concat!(
        "{\n",
        "  \"pack\": \"json\",\n",
        "  \"source_len\": 7,\n",
        "  \"ok\": false,\n",
        "  \"regions\": [\n",
        "    {\n",
        "      \"offset\": 5,\n",
        "      \"kind\": \"syntax error\",\n",
        "      \"ops\": [\n",
        "        {\"op\": \"insert\", \"token\": \"colon\"}\n",
        "      ],\n",
        "      \"resync\": 5\n",
        "    }\n",
        "  ]\n",
        "}\n"
    );
    assert_eq!(render_json(&output), expected);
}

#[test]
fn clean_documents_have_no_regions() {
    let output = trace("json", CLEAN).expect("json trace");
    assert!(output.ok);
    let text = render_text(&output);
    assert!(text.contains("no recovery regions"), "text: {text}");
    let json = render_json(&output);
    assert!(json.contains("\"regions\": []"), "json: {json}");
    assert!(json.contains("\"ok\": true"), "json: {json}");
}

#[test]
fn eof_failure_inserts_the_missing_brace() {
    let output = trace("json", EOF_BROKEN).expect("json trace");
    assert_eq!(output.regions.len(), 1, "regions: {:?}", output.regions);
    let region = &output.regions[0];
    assert_eq!(region.offset, output.source_len, "EOF region at end");
    assert_eq!(region.kind, "syntax error");
    assert_eq!(region.ops.len(), 1);
    assert_eq!(region.ops[0].op, "insert");
    assert_eq!(region.ops[0].token, "rbrace");
    assert_eq!(region.resync, output.source_len);
}

#[test]
fn canonical_tie_prefers_insert() {
    // The reference pack tolerates the empty element (no diagnostic);
    // the shadow automaton's stricter grammar flags the comma. The
    // canonical cost-2 repair is the lowest-Look value insert.
    let output = trace("json", TIE).expect("json trace");
    assert_eq!(output.regions.len(), 1, "regions: {:?}", output.regions);
    let region = &output.regions[0];
    assert_eq!(region.offset, 4);
    assert_eq!(region.kind, "unexpected comma");
    assert_eq!(region.ops.len(), 1);
    assert_eq!(region.ops[0].op, "insert");
    assert_eq!(region.ops[0].token, "string");
    assert_eq!(region.resync, 4);
}

#[test]
fn repair_script_batches_multiple_ops() {
    // One plan, three lazily-applied ops: the colon before `1`, the
    // comma before the next pair, the colon before `2`.
    let output = trace("json", MULTI_OP).expect("json trace");
    assert_eq!(output.regions.len(), 1, "regions: {:?}", output.regions);
    let region = &output.regions[0];
    assert_eq!(region.offset, 5);
    assert_eq!(region.kind, "syntax error");
    let ops: Vec<(&str, &str)> = region
        .ops
        .iter()
        .map(|op| (op.op, op.token.as_str()))
        .collect();
    assert_eq!(
        ops,
        [
            ("insert", "colon"),
            ("insert", "comma"),
            ("insert", "colon")
        ],
        "ops: {ops:?}"
    );
    assert_eq!(region.resync, 11);
}

#[test]
fn json_traces_multiple_regions_ascending() {
    let output = trace("json", TWO_REGIONS).expect("json trace");
    assert!(!output.ok);
    assert_eq!(output.regions.len(), 2, "regions: {:?}", output.regions);
    let offsets: Vec<usize> = output.regions.iter().map(|r| r.offset).collect();
    assert_eq!(offsets, [5, 8], "ascending offsets");
    let kinds: Vec<&str> = output.regions.iter().map(|r| r.kind.as_str()).collect();
    // Both failures are flagged by the pack itself (the stray `[2 `
    // garbage token and the missing-colon `1`), so both kinds resolve
    // to the pack's message — the `unexpected <token>` fallback only
    // fires for offsets no pack diagnostic covers.
    assert_eq!(kinds, ["syntax error", "syntax error"]);
    for region in &output.regions {
        assert!(
            region.resync >= region.offset,
            "resync past the failure: {region:?}"
        );
        assert!(!region.ops.is_empty(), "planned repair: {region:?}");
    }
    assert_eq!(output.regions[1].resync, output.source_len);
}

#[test]
fn determinism_byte_identical_runs() {
    for source in [MUTATED, TIE, MULTI_OP, TWO_REGIONS, EOF_BROKEN, CLEAN] {
        let first = trace("json", source).expect("trace");
        let json = render_json(&first);
        let text = render_text(&first);
        for _ in 0..4 {
            let again = trace("json", source).expect("trace");
            assert_eq!(render_json(&again), json, "json stable on {source:?}");
            assert_eq!(render_text(&again), text, "text stable on {source:?}");
        }
    }
}

#[test]
fn toml_pack_reports_native_recovery_regions() {
    let broken = trace("toml", TOML_BROKEN).expect("toml trace");
    assert!(!broken.ok);
    assert_eq!(broken.regions.len(), 2, "regions: {:?}", broken.regions);
    let offsets: Vec<usize> = broken.regions.iter().map(|r| r.offset).collect();
    assert_eq!(offsets, [11, 20], "ascending offsets");
    for region in &broken.regions {
        assert_eq!(region.kind, "syntax error");
        assert!(
            region.ops.is_empty(),
            "native regions carry no ops: {region:?}"
        );
        assert!(region.resync >= region.offset, "region: {region:?}");
    }
    let clean = trace("toml", TOML_CLEAN).expect("toml trace");
    assert!(clean.ok);
}

#[test]
fn toml_back_to_back_errors_get_monotonic_resyncs() {
    // Regression: `b = [2, , 3]` produces two adjacent error regions
    // (`[2,` then `3`). The old resume fill only ever patched the last
    // diag, so the first region kept resync = source length and the
    // printed resyncs ran backwards (19 then 17). Each region's resync
    // must be the first token after its own error token: 16 and 17.
    let source = "a = 1\nb = [2, , 3]\n";
    let output = trace("toml", source).expect("toml trace");
    assert!(!output.ok);
    assert_eq!(output.regions.len(), 2, "regions: {:?}", output.regions);
    let offsets: Vec<usize> = output.regions.iter().map(|r| r.offset).collect();
    assert_eq!(offsets, [14, 16], "ascending offsets");
    let resyncs: Vec<usize> = output.regions.iter().map(|r| r.resync).collect();
    assert_eq!(resyncs, [16, 17], "monotonic, per-region resyncs");
    for window in output.regions.windows(2) {
        assert!(
            window[1].resync >= window[0].resync,
            "resyncs never run backwards: {window:?}"
        );
    }
}

#[test]
fn unknown_pack_is_rejected() {
    let error = trace("cobol", CLEAN).expect_err("unknown pack");
    assert!(error.contains("unknown pack"), "error: {error}");
}

#[test]
fn exit_codes() {
    let good = temp_file("trace-good.json", CLEAN);
    let bad = temp_file("trace-bad.json", MUTATED);
    let missing = temp_dir().join("trace-missing-does-not-exist.json");
    let good_path = good.to_string_lossy().into_owned();
    let bad_path = bad.to_string_lossy().into_owned();
    let missing_path = missing.to_string_lossy().into_owned();

    // Clean document → 0.
    assert_eq!(run(&["trace", "json", &good_path]), OK);
    // Mutated document → 1 (regions found).
    assert_eq!(run(&["trace", "json", &bad_path]), FAIL);
    // Unreadable file → 2.
    assert_eq!(run(&["trace", "json", &missing_path]), USAGE);
    // Unknown pack → 2.
    assert_eq!(run(&["trace", "cobol", &good_path]), USAGE);
    // --json wiring still gates the verdict.
    assert_eq!(run(&["trace", "json", &bad_path, "--json"]), FAIL);
    assert_eq!(run(&["trace", "json", &good_path, "--json"]), OK);
}

#[test]
fn text_renders_human_readable_regions() {
    let output = trace("json", MUTATED).expect("trace");
    let text = render_text(&output);
    assert!(text.contains("pack json"), "text: {text}");
    assert!(text.contains("offset 5"), "text: {text}");
    assert!(text.contains("insert colon"), "text: {text}");
    assert!(text.contains("resync: 5"), "text: {text}");
}

/// Run the CLI; arguments after the program name.
fn run(args: &[&str]) -> i32 {
    cli::run(args.iter().copied()).expect("clap parse")
}

/// Write `content` to a uniquely named temp file; returns its path.
fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let path = temp_dir().join(format!("{name}-{}-{nanos}", std::process::id()));
    std::fs::write(&path, content).expect("write temp file");
    path
}

fn temp_dir() -> std::path::PathBuf {
    std::env::temp_dir()
}
