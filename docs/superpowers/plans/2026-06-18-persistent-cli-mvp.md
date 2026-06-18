# Persistent CLI MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add persistent CLI commands that can build an index file from a root directory and query that index file later.

**Architecture:** Keep the existing stateless `search <root> <query>` command for quick smoke tests. Add `index <root> <index-file>` and `query <index-file> <query>` using `FileIndexStore`. This creates the first restart-safe CLI workflow.

**Tech Stack:** Rust standard library, existing workspace crates, cargo test, clippy.

---

## Files

- Modify: `crates/cli/src/lib.rs`
- Modify: `crates/cli/tests/cli_tests.rs`
- Modify: `docs/superpowers/plans/2026-06-18-persistent-cli-mvp.md`

## Task 1: Index Command

- [x] Write a failing test showing `index <root> <index-file>` writes an index file.
- [x] Implement the `index` command.
- [x] Verify the test passes.

## Task 2: Query Command

- [x] Write a failing test showing `query <index-file> <query>` returns matching paths from a saved index.
- [x] Implement the `query` command.
- [x] Verify the test passes.

## Task 3: Usage Text

- [x] Update usage text to include `search`, `index`, and `query`.
- [x] Verify missing-argument tests pass.

## Task 4: Verification And Push

- [x] Run `cargo fmt --check`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Commit the persistent CLI MVP.
- [x] Push the branch to GitHub.
