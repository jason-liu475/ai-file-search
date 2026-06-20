# Refresh Index CLI MVP Implementation Plan

**Goal:** Add a `refresh <root> <index-file>` command that makes a saved index match the current contents of a scanned root.

**Architecture:** Reuse the existing scanner and file-backed store. The refresh command performs a full rescan and replaces the saved index contents with the current file set, which removes stale paths without introducing file watching or a new database layer.

**Tech Stack:** Rust workspace, `ai-file-search-cli`, `ai-file-search-indexer`, standard library filesystem APIs.

---

## Tasks

- [x] Write a CLI test showing `refresh` removes a stale indexed path after the file is deleted.
- [x] Add an index-store replacement API that swaps the current in-memory contents with scanned files.
- [x] Implement `refresh <root> <index-file>` in the CLI.
- [x] Update usage text and README command docs.
- [x] Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.
