# 002 — ci github actions

- **Epic:** Phase 0
- **Crates:** .github
- **Status:** done

## Work

GitHub Actions CI: fmt check, clippy -D warnings (pedantic), cargo test --workspace, doc build.

## TDD requirements

CI runs on push/PR and gates merges.

## Exit criteria

workflow yml files present; gates green.

## Outcome
- .github/workflows/ci.yml: checkout@v4 + dtolnay/rust-toolchain@stable (clippy, rustfmt) + Swatinem/rust-cache; gates cargo fmt --check, clippy --workspace --all-targets -D warnings, test --workspace, doc --workspace with RUSTDOCFLAGS=-D warnings.
