# CLI Prototype MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a usable CLI prototype that scans a directory and searches file names in one command.

**Architecture:** Keep command behavior in `ai-file-search-cli` and reuse `ai-file-search-indexer` for scanning and in-memory indexing. The CLI remains intentionally stateless until the durable index store is added in a later step.

**Tech Stack:** Rust standard library, existing workspace crates, cargo test, clippy.

---

## Files

- Modify: `crates/cli/Cargo.toml`
- Modify: `crates/cli/src/main.rs`
- Create: `crates/cli/src/lib.rs`
- Create: `crates/cli/tests/cli_tests.rs`
- Modify: `docs/superpowers/plans/2026-06-18-cli-prototype-mvp.md`

## Task 1: Search Command Function

- [x] Write a failing test showing `run(["search", root, query])` returns matching relative paths.
- [x] Add `ai-file-search-indexer` dependency to CLI crate.
- [x] Implement `run`.
- [x] Verify the test passes.

## Task 2: Usage Errors

- [x] Write a failing test showing missing arguments returns usage text and non-zero exit code.
- [x] Implement `CliResult`.
- [x] Verify the test passes.

## Task 3: Binary Entrypoint

- [x] Wire `main` to call `run`.
- [x] Print stdout and stderr in the correct streams.
- [x] Exit with the returned status code.
- [x] Verify tests pass.

## Task 4: Verification And Push

- [x] Run `cargo fmt --check`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Commit the CLI prototype MVP.
- [x] Push the branch to GitHub.
