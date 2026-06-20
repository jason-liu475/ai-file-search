# Lightweight Index Metadata MVP Implementation Plan

**Goal:** Keep the default index store dependency-free while recording enough metadata for future lightweight refresh decisions.

**Architecture:** Extend `IndexedFile` with file size and modified-time metadata populated by the scanner. Persist records in a simple versioned, tab-delimited text format that remains backward-compatible with old path-only index files. Save via a temporary file and rename so normal writes do not leave partial index contents.

**Tech Stack:** Rust standard library only, `ai-file-search-indexer`, `ai-file-search-cli`.

---

## Tasks

- [x] Add scanner coverage for file size and modified-time metadata.
- [x] Add index-store coverage for the new lightweight record format and old path-only compatibility.
- [x] Extend `IndexedFile` and scanner metadata population.
- [x] Update the file index store parser and writer.
- [x] Save index files through a temporary file before replacing the target.
- [x] Update CLI expectations and README wording.
- [x] Run `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.
