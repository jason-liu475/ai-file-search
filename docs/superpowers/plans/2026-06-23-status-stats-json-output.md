# Status and Stats JSON Output Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `stats --json` and `status --json` so AI tools can inspect index scale and freshness with machine-readable output.

**Architecture:** Reuse the existing `--json` parser flag and output-format enum. Allow JSON on `stats` and `status`, keep text output unchanged, and add small JSON formatters for index totals and refresh summaries.

**Tech Stack:** Rust workspace, existing CLI tests, existing file-backed index store and refresh summary.

---

### Task 1: JSON Stats and Status Output

**Files:**
- Modify: `crates/cli/tests/cli_tests.rs`
- Modify: `crates/cli/src/lib.rs`
- Modify: `README.md`

- [ ] **Step 1: Write failing tests**

Add tests for `stats <index-file> --json` and `status <root> <index-file> --json`.

- [ ] **Step 2: Run target tests to verify failure**

Run: `cargo test -p ai-file-search-cli json -- --nocapture`

Expected: FAIL because `--json` is not yet accepted by `stats` or `status`.

- [ ] **Step 3: Implement minimal code**

Allow `--json` for `stats` and `status`, and format results as JSON while preserving existing text output.

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

Commit message: `feat: add JSON stats and status output`
