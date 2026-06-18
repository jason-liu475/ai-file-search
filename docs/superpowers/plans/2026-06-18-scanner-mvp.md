# Scanner MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a deterministic directory scanner that returns normalized file metadata for a configured root.

**Architecture:** Keep scanner logic in a new `ai-file-search-indexer` crate. Reuse `ai-file-search-core::PathId` for normalized relative paths. The scanner should walk directories synchronously for the MVP and report permission or IO failures as skipped entries instead of aborting the whole scan.

**Tech Stack:** Rust standard library, existing workspace crates, cargo test, clippy.

---

## Files

- Create: `crates/indexer/Cargo.toml`
- Create: `crates/indexer/src/lib.rs`
- Create: `crates/indexer/src/scanner.rs`
- Create: `crates/indexer/tests/scanner_tests.rs`
- Modify: `Cargo.toml`

## Task 1: Workspace Crate

- [x] Add `crates/indexer` as `ai-file-search-indexer`.
- [x] Depend on `ai-file-search-core`.
- [x] Export scanner types from crate root.
- [x] Verify `cargo test --workspace` still passes.

## Task 2: Basic Directory Scan

- [x] Write a failing test showing scanner returns files under a root.
- [x] Implement `Scanner::scan`.
- [x] Return relative normalized paths.
- [x] Verify tests pass.

## Task 3: Nested Directories

- [x] Write a failing test for nested file discovery.
- [x] Implement recursive walking.
- [x] Ensure directories themselves are not returned as files.
- [x] Verify tests pass.

## Task 4: Exclusions

- [x] Write a failing test for excluding a directory name.
- [x] Implement `ScanOptions::excluded_names`.
- [x] Ensure excluded directories are not traversed.
- [x] Verify tests pass.

## Task 5: Verification

- [x] Run `cargo fmt --check`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Commit the scanner MVP.
