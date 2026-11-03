# 003 — git cliff changelog release

- **Epic:** Phase 0
- **Crates:** workspace
- **Status:** done

## Work

git-cliff configured (cliff.toml, conventional commits); release workflow publishes changelog on tags.

## TDD requirements

Changelog generated from conventional commits.

## Exit criteria

cliff.toml + release workflow present.

## Outcome
- cliff.toml (conventional commits) at the workspace root; release.yml runs orhun/git-cliff-action@v4 with the checked-in config and emits CHANGELOG.md (git cliff --latest) on tags.
