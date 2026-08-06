---
name: release-pgpilot
description: "Use when cutting a release of pgpilot (the Postgres monitoring TUI in this repo). Triggers: 'cut a pgpilot release', 'bump the version', 'publish a new pgpilot build'."
---

# Release pgpilot

`.github/workflows/release.yml` builds and publishes automatically once a matching tag is
pushed — cross-platform binaries (Linux/macOS-arm64/macOS-intel/Windows) attached to a
GitHub Release via `gh release create --generate-notes`. This checklist is what happens
*before* that tag gets pushed; nothing here duplicates what CI already does.

1. Confirm the working tree is clean and on `master`. Run `cargo build` and `cargo clippy`, both must be clean. (No test suite exists yet — don't invent one as part of a release.)
2. Bump `version` in `Cargo.toml` (semver: patch for fixes, minor for new panels/tabs/features). CI's `check-version` job will fail the whole run if the tag you push in step 5 doesn't match this exactly, so don't skip it.
3. `cargo build --release`. Smoke-test the resulting `target/release/pgpilot` binary against a real or throwaway Postgres: connect, cycle every tab, confirm nothing sits in a stuck error/loading state.
4. Confirm CLAUDE.md's Architecture section reflects whatever changed in this release (standing rule already in that file).
5. `git tag v<version>` — **ask the user before pushing the tag**; pushing is a shared/visible action, and for this repo specifically it also triggers the release workflow (4 platform builds + a public GitHub Release), not just a local git operation.
6. Once pushed, watch the Actions run: `check-version` → 4-target `build` → `publish`. If any leg fails, the tag already exists — fix forward with a new patch version and tag rather than force-pushing/deleting the old tag.

Explicitly out of scope: changelog file generation (GitHub's `--generate-notes` covers this), crates.io publishing, a Homebrew tap (would need a second public repo — not set up, see CLAUDE.md's Versioning & releases section).
