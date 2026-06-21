# Status CLI MVP Implementation Plan

**Goal:** Add a read-only `status <root> <index-file>` command that reports pending index changes without rewriting the saved index.

**Architecture:** Reuse the current scanner, index-file loader, and `RefreshSummary` metadata comparison. The command performs a full scan like `refresh`, but it never calls `replace_all` or `save`.

**Tech Stack:** Rust standard library only, `ai-file-search-cli`, `ai-file-search-indexer`.

---

## Tasks

- [x] Add a CLI test showing `status` reports added, updated, removed, and unchanged counts without mutating the index file.
- [x] Implement `status <root> <index-file>`.
- [x] Update usage text and README command docs.
- [x] Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.
