# IPC Transport MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add local IPC transport to the daemon so clients can call the JSON-RPC handler without HTTP.

**Architecture:** Introduce a shared async stream handler, then attach it to Windows Named Pipe and Unix Domain Socket listeners using Tokio. Keep `stdio` as the lightest transport and add `ipc` / `ipc-request` commands for local IPC smoke tests.

**Tech Stack:** Rust, Tokio IO/net primitives, existing daemon handler and protocol crates.

---

### Task 1: Shared Stream Handler and IPC Commands

**Files:**
- Modify: `crates/daemon/Cargo.toml`
- Modify: `crates/daemon/src/lib.rs`
- Modify: `crates/daemon/src/main.rs`
- Test: `crates/daemon/tests/transport_tests.rs`
- Modify: `README.md`

- [x] **Step 1: Write failing stream handler test**

Use `tokio::io::duplex` to verify that two newline-delimited JSON-RPC requests receive two responses on one stream.

- [x] **Step 2: Run target test to verify failure**

Run: `cargo test -p ai-file-search-daemon stream_handler -- --nocapture`

- [x] **Step 3: Implement stream handler and IPC commands**

Implement `handle_json_stream`, `ipc`, and `ipc-request`.

- [x] **Step 4: Verify**

Run:

```bash
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- [x] **Step 5: Commit and push**

Commit message: `feat: add daemon IPC transport`
