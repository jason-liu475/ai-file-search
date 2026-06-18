# Index Store MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a small in-memory index store that can upsert scanned files, remove files, and query by file name substring.

**Architecture:** Keep the MVP inside `ai-file-search-indexer` so scanner output can flow directly into indexing. Define a trait-like API around `MemoryIndexStore` now, then move to a durable SQLite implementation in the next step without changing scanner behavior.

**Tech Stack:** Rust standard library, existing workspace crates, cargo test, clippy.

---

## Files

- Modify: `crates/indexer/src/lib.rs`
- Create: `crates/indexer/src/store.rs`
- Create: `crates/indexer/tests/store_tests.rs`
- Modify: `docs/superpowers/plans/2026-06-18-index-store-mvp.md`

## Task 1: Upsert And Search

- [x] Write a failing test showing an upserted file can be found by substring.
- [x] Implement `MemoryIndexStore::new`.
- [x] Implement `upsert_file`.
- [x] Implement `search_by_name`.
- [x] Verify the test passes.

## Task 2: Stable Replacement

- [x] Write a failing test showing upserting the same path replaces metadata instead of duplicating results.
- [x] Implement path-keyed replacement.
- [x] Verify the test passes.

## Task 3: Remove

- [x] Write a failing test showing removing a path removes it from search results.
- [x] Implement `remove_path`.
- [x] Verify the test passes.

## Task 4: Verification And Push

- [x] Run `cargo fmt --check`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Commit the index store MVP.
- [x] Push the branch to GitHub.
