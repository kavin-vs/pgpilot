---
name: release-pgpilot
description: "Use when cutting a release of pgpilot (the Postgres monitoring TUI in this repo). Triggers: 'cut a pgpilot release', 'bump the version', 'publish a new pgpilot build'."
---

# Release pgpilot

No release process exists yet in this repo (no CI, no git tags, `Cargo.toml`'s version has been `0.1.0` since the initial commit). This is a minimal process, not an invented CI/changelog pipeline — nothing here beyond what's needed to cut and tag a build.

1. Confirm the working tree is clean and on `master`. Run `cargo build` and `cargo clippy`, both must be clean. (No test suite exists yet — don't invent one as part of a release.)
2. Bump `version` in `Cargo.toml` (semver: patch for fixes, minor for new panels/tabs/features).
3. `cargo build --release`. Smoke-test the resulting `target/release/pgpilot` binary against a real or throwaway Postgres: connect, cycle every tab, confirm nothing sits in a stuck error/loading state.
4. Confirm CLAUDE.md's Architecture section reflects whatever changed in this release (standing rule already in that file).
5. `git tag v<version>` — **ask the user before pushing the tag**; pushing is a shared/visible action.

Explicitly out of scope: changelog file generation, crates.io publishing, a CI pipeline. None of these have been requested, and commit history plus CLAUDE.md's dated version notes already serve as the change record.
