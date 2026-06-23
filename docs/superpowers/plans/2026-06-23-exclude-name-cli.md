# Exclude Name CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `--exclude-name <name>` to scanning CLI commands so noisy directories can be skipped explicitly.

**Architecture:** Reuse the existing `ScanOptions::exclude_name` scanner capability. Add a small CLI parser that separates positional arguments from repeated `--exclude-name` options and passes the resulting `ScanOptions` to `search`, `index`, `refresh`, `status`, and `bench`.

**Tech Stack:** Rust workspace, existing CLI crate tests, existing indexer scanner.

---

### Task 1: CLI Exclude Option

**Files:**
- Modify: `crates/cli/tests/cli_tests.rs`
- Modify: `crates/cli/src/lib.rs`
- Modify: `README.md`

- [ ] **Step 1: Write failing tests**

Add CLI tests proving `index` excludes a named directory and malformed `--exclude-name` usage returns a usage error.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p ai-file-search-cli exclude`

Expected: tests fail because the CLI does not yet parse `--exclude-name`.

- [ ] **Step 3: Implement minimal parser and option plumbing**

Add a small parser in `crates/cli/src/lib.rs` that supports repeated `--exclude-name <name>` options after positional arguments. Route scanning commands through parsed options and use `Scanner::new(options)`.

- [ ] **Step 4: Run target tests**

Run: `cargo test -p ai-file-search-cli exclude`

Expected: target tests pass.

- [ ] **Step 5: Update docs and full verification**

Update README command reference and run:

```bash
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 6: Commit and push**

Commit message: `feat: add CLI exclude-name option`
