# File Index Store MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a tiny persistent index store that can save indexed paths to disk, reload them, and search after restart.

**Architecture:** Add `FileIndexStore` in `ai-file-search-indexer` as an MVP persistence layer. It stores one normalized path per line and delegates query behavior to `MemoryIndexStore`. This is intentionally simple and will be replaced by SQLite/Tantivy later behind the same behavior.

**Tech Stack:** Rust standard library, existing workspace crates, cargo test, clippy.

---

## Files

- Modify: `crates/indexer/src/lib.rs`
- Modify: `crates/indexer/src/store.rs`
- Create: `crates/indexer/tests/file_store_tests.rs`
- Modify: `docs/superpowers/plans/2026-06-18-file-index-store-mvp.md`

## Task 1: Save And Load

- [x] Write a failing test showing an upserted file can be saved and found after reopening the store.
- [x] Implement `FileIndexStore::open`.
- [x] Implement `FileIndexStore::upsert_file`.
- [x] Implement `FileIndexStore::save`.
- [x] Implement `FileIndexStore::search_by_name`.
- [x] Verify the test passes.

## Task 2: Remove Persistence

- [x] Write a failing test showing removed files stay removed after save and reload.
- [x] Implement `FileIndexStore::remove_path`.
- [x] Verify the test passes.

## Task 3: Verification And Push

- [x] Run `cargo fmt --check`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Commit the file index store MVP.
- [x] Push the branch to GitHub.
