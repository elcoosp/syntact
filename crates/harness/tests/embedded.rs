//! Harness integration tests (issue 017 TDD): runner mechanics on
//! embedded corpora — budgets, losslessness, balance, determinism — plus
//! the differential stage (reference vs descriptor engine).

use harness::{Corpus, Differential, Summary, run_corpus};

/// The embedded conformance corpus (all JSON families + malformed with
/// budgets) — the same discipline the flagship packs embed.
const CORPUS_TEXT: &str = "\
# json conformance corpus (issue 017)
=== empty
tolerated 0
---
=== scalars
tolerated 0
---
[1, -2.5e3, true, false, null, \"s\"]
=== object
tolerated 0
---
{\"a\": 1, \"b\": [true, null], \"c\": {\"d\": []}}
=== nested
tolerated 0
---
  [ 1 , [ 2 , [ 3 ] ] ]
=== unicode-string
tolerated 0
---
\"str with \\u00e9 escape\"
=== scalar-document
tolerated 0
---
1 2
=== trailing-comma
tolerated 0
---
{\"a\": 1,}
=== missing-value
tolerated 2
---
{\"a\": }
=== unterminated-array
tolerated 1
---
[1, 2
=== bad-literal
tolerated 1
---
tru
=== garbage
tolerated 1
---
@#$
";

#[test]
fn embedded_corpus_is_all_green() {
    let corpus = Corpus::parse(CORPUS_TEXT).expect("embedded corpus parses");
    assert_eq!(corpus.len(), 11);
    let (reports, summary) = run_corpus(&corpus);
    assert_eq!(summary.total, 11);
    for report in &reports {
        assert!(report.pass, "case {}: {:?}", report.name, report.failure());
    }
    assert!(summary.all_green());
    assert_eq!(
        summary,
        Summary {
            total: 11,
            passed: 11,
            failed: 0,
        }
    );
}

#[test]
fn runner_flags_budget_overruns_deterministically() {
    // Tighten the garbage fixture's budget to 0: the runner must flag it
    // (and only it) as a budget failure.
    let text = CORPUS_TEXT.replace("=== garbage\ntolerated 1", "=== garbage\ntolerated 0");
    let corpus = Corpus::parse(&text).expect("parses");
    let (reports, summary) = run_corpus(&corpus);
    let failures: Vec<&str> = reports
        .iter()
        .filter(|r| !r.pass)
        .map(|r| r.name.as_str())
        .collect();
    assert_eq!(failures, ["garbage"]);
    assert_eq!(summary.failed, 1);
}

#[test]
fn differential_stage_agrees_on_valid_corpus() {
    let rig = Differential::json();
    let sources = [
        "[]",
        "[1, 2, 3]",
        "{}",
        r#"{"a": 1}"#,
        r#"{"a": [1, {"b": null}], "c": true}"#,
        "1 2",
        "",
    ];
    for source in sources {
        assert!(rig.agrees(source).expect("structural ok"), "on {source:?}");
    }
}

#[test]
fn differential_stage_reports_disagreement() {
    // Garbage: the engine errors (case out of scope) while the reference
    // recovers — the driver reports divergence, never a silent pass.
    let rig = Differential::json();
    let agrees = rig.agrees("@#$").expect("structural ok");
    assert!(!agrees, "garbage diverges");
}

#[test]
fn differential_none_on_engine_error() {
    let rig = Differential::json();
    let compared = rig.compare("@#$").expect("structural ok");
    assert!(compared.is_none(), "engine errored — case out of scope");
}
