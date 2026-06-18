# Bench CLI MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a small benchmark command that measures scan and search time for a real directory.

**Architecture:** Keep benchmarking in the CLI layer and reuse `Scanner` plus `MemoryIndexStore`. The command reports deterministic counters and best-effort timing values; tests assert counters and output shape, not exact durations.

**Tech Stack:** Rust standard library, existing workspace crates, cargo test, clippy.

---

## Files

- Modify: `crates/cli/src/lib.rs`
- Modify: `crates/cli/tests/cli_tests.rs`
- Modify: `docs/superpowers/plans/2026-06-18-bench-cli-mvp.md`

## Task 1: Bench Command

- [x] Write a failing test showing `bench <root> <query>` reports files, matches, scan_ms, and search_ms.
- [x] Implement the `bench` command.
- [x] Verify the test passes.

## Task 2: Usage Text

- [x] Update usage text to include `bench <root> <query>`.
- [x] Verify missing-argument tests pass.

## Task 3: Verification And Push

- [x] Run `cargo fmt --check`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Commit the bench CLI MVP.
- [x] Push the branch to GitHub.
