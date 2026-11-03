//! Corpus format (issue 017): named fixtures with tolerated-error
//! budgets in a dependency-free text format, parsed in fixture order
//! (deterministic by construction).

/// One conformance case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Case {
    /// The fixture name (from the `===` header).
    pub name: String,
    /// The raw source bytes (verbatim between the header and the next).
    pub source: String,
    /// Tolerated recovered errors; 0 = the fixture must be error-free.
    pub tolerated: usize,
}

/// A parsed corpus: ordered cases.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Corpus(Vec<Case>);

/// A corpus format error (header without body, missing `---`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusError(pub String);

impl std::fmt::Display for CorpusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CorpusError {}

impl Corpus {
    /// Parse the corpus text. Lines starting with `#` before a header are
    /// comments; each case is `=== name`, then `tolerated N`, then `---`,
    /// then the verbatim body until the next `===` (or EOF).
    ///
    /// # Errors
    /// [`CorpusError`] on a header missing `---`, or an unparsable
    /// `tolerated` value.
    pub fn parse(text: &str) -> Result<Self, CorpusError> {
        let mut cases: Vec<Case> = Vec::new();
        let mut name: Option<String> = None;
        let mut tolerated = 0_usize;
        let mut body: Option<Vec<&str>> = None;
        for line in text.lines() {
            if let Some(header) = line.strip_prefix("===") {
                if let Some(finished) = name.replace(header.trim().to_owned()) {
                    cases.push(Case {
                        source: join_body(&body.take().unwrap_or_default()),
                        name: finished,
                        tolerated,
                    });
                }
                tolerated = 0;
                continue;
            }
            if name.is_none() {
                continue; // comments / leading blank lines
            }
            if line == "---" {
                body = Some(Vec::new());
                continue;
            }
            if body.is_none() {
                let meta = line.trim();
                if let Some(value) = meta.strip_prefix("tolerated") {
                    tolerated = value
                        .trim()
                        .parse()
                        .map_err(|_| CorpusError(format!("bad tolerated value: {meta:?}")))?;
                }
                // Unknown meta lines are ignored (forward compatibility).
                continue;
            }
            if let Some(lines) = body.as_mut() {
                lines.push(line);
            }
        }
        if let Some(finished) = name.take() {
            cases.push(Case {
                source: join_body(&body.unwrap_or_default()),
                name: finished,
                tolerated,
            });
        }
        Ok(Self(cases))
    }

    /// The cases in fixture order.
    #[must_use]
    pub fn cases(&self) -> &[Case] {
        &self.0
    }

    /// Number of cases.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the corpus is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn join_body(lines: &[&str]) -> String {
    // The body is verbatim: re-join with newlines and keep exactly the
    // trailing newline the fixture carried (none — the last line has no
    // newline of its own, matching `.lines()` semantics).
    let mut out = lines.join("\n");
    if !lines.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Corpus, CorpusError};

    #[test]
    fn parses_cases_in_order_with_budgets() {
        let text = "\
# leading comment
=== scalars
tolerated 0
---
[1, true, null]
=== broken
tolerated 2
---
{\"a\": }
";
        let corpus = Corpus::parse(text).expect("parses");
        assert_eq!(corpus.len(), 2);
        let first = &corpus.cases()[0];
        assert_eq!(first.name, "scalars");
        assert_eq!(first.tolerated, 0);
        assert_eq!(first.source, "[1, true, null]\n");
        let second = &corpus.cases()[1];
        assert_eq!(second.name, "broken");
        assert_eq!(second.tolerated, 2);
        assert_eq!(second.source, "{\"a\": }\n");
    }

    #[test]
    fn bodies_are_verbatim_including_blank_lines() {
        let text = "=== ws\n---\n  [ 1 ,\n\n  2 ]\n=== next\ntolerated 1\n---\n";
        let corpus = Corpus::parse(text).expect("parses");
        assert_eq!(corpus.cases()[0].source, "  [ 1 ,\n\n  2 ]\n");
        assert_eq!(
            corpus.cases()[1].source,
            "",
            "empty body after the last ---"
        );
        assert_eq!(corpus.cases()[1].tolerated, 1);
    }

    #[test]
    fn bad_tolerated_value_is_an_error() {
        assert_eq!(
            Corpus::parse("=== x\ntolerated many\n---\nbody\n"),
            Err(CorpusError(
                "bad tolerated value: \"tolerated many\"".into()
            ))
        );
    }

    #[test]
    fn empty_corpus_and_missing_body_edge() {
        assert!(
            Corpus::parse("# only comments\n")
                .expect("parses")
                .is_empty()
        );
        let corpus = Corpus::parse("=== head\n---\n").expect("parses");
        assert_eq!(corpus.cases()[0].source, "", "no body lines at all");
    }
}
