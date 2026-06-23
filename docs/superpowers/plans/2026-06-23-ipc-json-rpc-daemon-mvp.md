# IPC JSON-RPC Daemon MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a first-stage background daemon that exposes saved-index `stats` and `search` over local IPC using newline-delimited JSON-RPC.

**Architecture:** Create a small `protocol` crate for request/response JSON lines and a `daemon` crate for request handling plus platform IPC. Windows uses Named Pipe, macOS/Linux use Unix Domain Socket, and both share the same handler and JSON protocol.

**Tech Stack:** Rust, serde/serde_json, Tokio IPC primitives, existing file-backed index store.

---

### Task 1: Protocol and Handler

**Files:**
- Modify: `Cargo.toml`
- Create: `crates/protocol/*`
- Create: `crates/daemon/*`
- Modify: `README.md`

- [ ] **Step 1: Write failing tests**

Add protocol tests for parsing requests and formatting responses, plus daemon handler tests for `stats`, `search`, and unknown method errors.

- [ ] **Step 2: Run target tests to verify failure**

Run: `cargo test -p ai-file-search-protocol -p ai-file-search-daemon`

- [ ] **Step 3: Implement minimal protocol and handler**

Implement request/response types and handler functions using the existing index store.

- [ ] **Step 4: Add IPC serve/request binary**

Implement `serve <index-file> <endpoint>` and `request <endpoint> <json-line>`.

- [ ] **Step 5: Verify**

Run:

```bash
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

- [ ] **Step 6: Commit and push**

Commit message: `feat: add IPC JSON-RPC daemon MVP`
