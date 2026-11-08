//! The runner (issue 017): per-case gates — budgets, losslessness,
//! balance, determinism — over the packs-json reference parser.

use runtime_core::events::{self, Event};
use runtime_core::input::Source;
use runtime_core::tree::GreenNode;
use std::sync::Arc;

use crate::corpus::Case;

/// The outcome of one case through the runner gates.
#[derive(Debug, Clone, PartialEq, Eq)]
// reports one independent conformance property
pub struct Report {
    /// The case name.
    pub name: String,
    /// Recovered errors the parser reported.
    pub errors: usize,
    /// The fixture's tolerated-error budget.
    pub tolerated: usize,
    /// `render(parse(x)) == x`.
    pub lossless: bool,
    /// The wrapped event stream validates (`events::process`).
    pub balanced: bool,
    /// Two parse runs produced identical streams.
    pub deterministic: bool,
    /// All gates passed.
    pub pass: bool,
}

impl Report {
    /// The failing gate, for diagnostics.
    #[must_use]
    pub fn failure(&self) -> Option<&'static str> {
        if self.errors > self.tolerated {
            Some("budget")
        } else if !self.lossless {
            Some("lossless")
        } else if !self.balanced {
            Some("balanced")
        } else if !self.deterministic {
            Some("deterministic")
        } else {
            None
        }
    }
}

/// Aggregate over a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Summary {
    /// Cases run.
    pub total: usize,
    /// Cases passing all gates.
    pub passed: usize,
    /// Cases failing at least one gate.
    pub failed: usize,
}

impl Summary {
    /// Whether every case passed.
    #[must_use]
    pub fn all_green(&self) -> bool {
        self.failed == 0 && self.total == self.passed
    }
}

/// Run one case through all gates (packs-json reference parser).
#[must_use]
pub fn run_case(case: &Case) -> Report {
    let (events, tokens, errors) = packs_json::reference::parse_events(&case.source);
    let second = packs_json::reference::parse_events(&case.source);
    let deterministic = (events == second.0) && (tokens == second.1) && (errors == second.2);

    // Balance: wrap in the implicit root and validate.
    let mut wrapped = Vec::with_capacity(events.len() + 2);
    wrapped.push(Event::Start {
        kind: packs_json::reference::ROOT_KIND,
        forward_parent: None,
    });
    wrapped.extend(events.iter().copied());
    wrapped.push(Event::Finish);
    let balanced = events::process(&wrapped).is_ok();

    // Losslessness through the full tree integration.
    let tree: Option<Arc<GreenNode>> = packs_json::parse(&case.source).ok();
    let lossless =
        tree.is_some_and(|t| t.render(&Source::from_str_unchecked(&case.source)) == case.source);

    let report = Report {
        name: case.name.clone(),
        errors,
        tolerated: case.tolerated,
        lossless,
        balanced,
        deterministic,
        pass: false,
    };
    Report {
        pass: report.failure().is_none(),
        ..report
    }
}

/// Run a whole corpus (fixture order).
#[must_use]
pub fn run_corpus(corpus: &crate::corpus::Corpus) -> (Vec<Report>, Summary) {
    let mut reports = Vec::with_capacity(corpus.len());
    let mut summary = Summary::default();
    for case in corpus.cases() {
        let report = run_case(case);
        summary.total += 1;
        if report.pass {
            summary.passed += 1;
        } else {
            summary.failed += 1;
        }
        reports.push(report);
    }
    (reports, summary)
}

#[cfg(test)]
mod tests {
    use super::{run_case, run_corpus};
    use crate::corpus::{Case, Corpus};

    #[test]
    fn valid_case_passes_all_gates() {
        let case = Case {
            name: "object".into(),
            source: r#"{"a": [1, true, null]}"#.into(),
            tolerated: 0,
        };
        let report = run_case(&case);
        assert_eq!(report.failure(), None, "{report:?}");
        assert!(report.pass);
        assert!(report.lossless && report.balanced && report.deterministic);
    }

    #[test]
    fn budget_gate_flags_overruns() {
        let case = Case {
            name: "broken".into(),
            source: "{\"a\": }".into(),
            tolerated: 0, // but recovery reports 2
        };
        let report = run_case(&case);
        assert_eq!(report.failure(), Some("budget"));
        assert!(!report.pass);
        // Within budget it passes (losslessness still holds).
        let ok = Case {
            tolerated: 2,
            ..case
        };
        assert_eq!(run_case(&ok).failure(), None);
    }

    #[test]
    fn corpus_run_summarizes_and_orders() {
        let corpus =
            Corpus::parse("=== a\ntolerated 0\n---\n[]\n=== b\ntolerated 2\n---\n{\"a\": }\n")
                .expect("parses");
        let (reports, summary) = run_corpus(&corpus);
        assert_eq!(summary.total, 2);
        assert!(summary.all_green(), "{reports:?}");
        assert_eq!(reports[0].name, "a");
        assert_eq!(reports[1].name, "b");
    }

    #[test]
    fn failing_case_fails_the_summary() {
        let corpus = Corpus::parse("=== bad\ntolerated 0\n---\n@#$\n").expect("parses");
        let (_, summary) = run_corpus(&corpus);
        assert_eq!(summary.failed, 1);
        assert!(!summary.all_green());
    }
}
