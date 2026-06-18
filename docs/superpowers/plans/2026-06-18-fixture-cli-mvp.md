# Fixture CLI MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a deterministic fixture generator command for repeatable local benchmarks.

**Architecture:** Keep fixture generation in the CLI layer for now. The command creates a predictable directory tree and file names, then later benchmark runs can use that tree as a stable dataset.

**Tech Stack:** Rust standard library, existing workspace crates, cargo test, clippy.

---

## Files

- Modify: `crates/cli/src/lib.rs`
- Modify: `crates/cli/tests/cli_tests.rs`
- Modify: `docs/superpowers/plans/2026-06-18-fixture-cli-mvp.md`

## Task 1: Fixture Command

- [x] Write a failing test showing `fixture <root> <count>` creates deterministic files.
- [x] Implement the `fixture` command.
- [x] Verify the test passes.

## Task 2: Invalid Count

- [x] Write a failing test showing invalid counts return a usage error.
- [x] Implement count parsing errors.
- [x] Verify the test passes.

## Task 3: Usage Text

- [x] Update usage text to include `fixture <root> <count>`.
- [x] Verify missing-argument tests pass.

## Task 4: Verification And Push

- [x] Run `cargo fmt --check`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Commit the fixture CLI MVP.
- [x] Push the branch to GitHub.
