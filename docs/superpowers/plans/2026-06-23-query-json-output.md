# Query JSON Output Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `query <index-file> <query> --json` so AI tools can consume search results with metadata.

**Architecture:** Extend the existing lightweight CLI parser with a boolean `--json` flag. Keep plain-text query output unchanged and add a small internal JSON formatter for query results without introducing new dependencies.

**Tech Stack:** Rust workspace, existing CLI crate tests, existing file-backed index store.

---

### Task 1: JSON Query Output

**Files:**
- Modify: `crates/cli/tests/cli_tests.rs`
- Modify: `crates/cli/src/lib.rs`
- Modify: `README.md`

- [ ] **Step 1: Write failing tests**

Add a CLI test for `query <index-file> <query> --json` returning:

```json
{"files":[{"path":"Documents/quarterly-report.pdf","size_bytes":6,"modified_unix_seconds":<mtime>}]}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ai-file-search-cli json -- --nocapture`

Expected: FAIL because `--json` is currently rejected by the parser.

- [ ] **Step 3: Implement minimal code**

Parse `--json`, allow it only on `query`, and format matching `IndexedFile` values as JSON with path, size, and modified time.

- [ ] **Step 4: Run target tests**

Run: `cargo test -p ai-file-search-cli json -- --nocapture`

Expected: PASS.

- [ ] **Step 5: Update docs and verify**

Update README and run:

```bash
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 6: Commit and push**

Commit message: `feat: add JSON query output`
