# Index Status JSON-RPC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a read-only `index_status` daemon method that reports whether the saved index needs refresh while preserving the stored-root safety boundary.

**Architecture:** Extend the existing daemon request dispatcher and method catalog. The new private handler reuses `ScanOptions`, `FileIndexStore`, `Scanner`, `RefreshSummary`, and the existing root-matching logic; it scans and compares but never mutates or saves the store. No crate, dependency, transport, or public Rust API is added.

**Tech Stack:** Rust 2024, `serde_json`, the workspace indexer and protocol crates, Cargo test/fmt/clippy, local JSON-RPC over Named Pipe or Unix Domain Socket.

---

## File Structure

- Modify: `crates/daemon/tests/handler_tests.rs`
  - Defines the complete request/response contract and verifies the index file remains byte-for-byte unchanged.
- Modify: `crates/daemon/src/lib.rs`
  - Dispatches `index_status`, advertises it, validates parameters, scans the stored root, and renders the summary.
- Modify: `README.md`
  - Documents the read-only method, parameters, response purpose, and full-rescan limitation.

No files or modules are created for production code because the operation belongs to the existing daemon handler and uses its private helpers.

### Task 1: Implement the read-only daemon method with TDD

**Files:**
- Modify: `crates/daemon/tests/handler_tests.rs:5-289`
- Modify: `crates/daemon/src/lib.rs:393-586`

- [ ] **Step 1: Extend the test imports and add a scanner-backed index fixture helper**

Change the indexer import in `crates/daemon/tests/handler_tests.rs` to:

```rust
use ai_file_search_indexer::{FileIndexStore, IndexedFile, ScanOptions, Scanner};
```

Add this helper immediately before the existing `save_index` helper:

```rust
fn save_scanned_index(index_path: &Path, root: &Path) {
    let files = Scanner::new(ScanOptions::default())
        .scan(root)
        .expect("fixture root should scan");
    let mut store = FileIndexStore::open(index_path).expect("store should open");
    store.set_root_path(root);
    store.replace_all(files);
    store.save().expect("store should save");
}
```

- [ ] **Step 2: Add all failing behavior tests before production code**

In `handler_returns_method_catalog`, insert the following expected catalog item immediately after `ping`:

```rust
"{\"name\":\"index_status\",\"params\":{\"exclude_names\":\"optional string array\",\"root\":\"optional string; must match stored root\"}},",
```

Add these tests before `struct TestDir`:

```rust
#[test]
fn handler_returns_current_index_status_without_rewriting_index() {
    let fixture = TestDir::new("handler_returns_current_index_status_without_rewriting_index");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).expect("root fixture should be created");
    fs::write(root.join("current.txt"), "current").expect("current fixture should be written");

    let index_path = root.join("index.txt");
    save_scanned_index(&index_path, &root);
    let index_before = fs::read(&index_path).expect("index should be readable");

    let response = handle_json_line(
        &index_path,
        r#"{"id":11,"method":"index_status","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":11,\"result\":{\"added\":0,\"needs_refresh\":false,\"removed\":0,\"scanned_files\":1,\"unchanged\":1,\"updated\":0}}\n"
    );
    assert_eq!(
        fs::read(&index_path).expect("index should remain readable"),
        index_before
    );
}

#[test]
fn handler_returns_stale_index_status_without_rewriting_index() {
    let fixture = TestDir::new("handler_returns_stale_index_status_without_rewriting_index");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).expect("root fixture should be created");
    fs::write(root.join("unchanged.txt"), "same").expect("unchanged fixture should be written");
    fs::write(root.join("updated.txt"), "old").expect("updated fixture should be written");
    fs::write(root.join("removed.txt"), "removed").expect("removed fixture should be written");

    let index_path = fixture.path().join("index.txt");
    save_scanned_index(&index_path, &root);
    fs::write(root.join("updated.txt"), "updated-content")
        .expect("updated fixture should change");
    fs::remove_file(root.join("removed.txt")).expect("removed fixture should be deleted");
    fs::write(root.join("added.txt"), "added").expect("added fixture should be written");
    let index_before = fs::read(&index_path).expect("index should be readable");

    let response = handle_json_line(
        &index_path,
        r#"{"id":12,"method":"index_status","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":12,\"result\":{\"added\":1,\"needs_refresh\":true,\"removed\":1,\"scanned_files\":3,\"unchanged\":1,\"updated\":1}}\n"
    );
    assert_eq!(
        fs::read(&index_path).expect("index should remain readable"),
        index_before
    );
}

#[test]
fn handler_rejects_index_status_root_that_differs_from_stored_root() {
    let fixture = TestDir::new("handler_rejects_index_status_root_that_differs_from_stored_root");
    let allowed_root = fixture.path().join("allowed-root");
    let denied_root = fixture.path().join("denied-root");
    fs::create_dir_all(&allowed_root).expect("allowed root should be created");
    fs::create_dir_all(&denied_root).expect("denied root should be created");

    let index_path = fixture.path().join("index.txt");
    save_scanned_index(&index_path, &allowed_root);
    let index_before = fs::read(&index_path).expect("index should be readable");
    let request = serde_json::json!({
        "id": 13,
        "method": "index_status",
        "params": { "root": denied_root.to_string_lossy() }
    })
    .to_string();

    let response = handle_json_line(&index_path, &request);

    assert_eq!(
        response.to_json_line(),
        "{\"id\":13,\"error\":{\"message\":\"root does not match stored index root\"}}\n"
    );
    assert_eq!(
        fs::read(&index_path).expect("index should remain readable"),
        index_before
    );
}

#[test]
fn handler_requires_index_status_root_without_metadata() {
    let fixture = TestDir::new("handler_requires_index_status_root_without_metadata");
    let index_path = fixture.path().join("index.txt");
    save_index(&index_path, vec![indexed_file("saved.txt", 1, 1)]);

    let response = handle_json_line(
        &index_path,
        r#"{"id":14,"method":"index_status","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":14,\"error\":{\"message\":\"missing string param: root\"}}\n"
    );
}

#[test]
fn handler_rejects_invalid_index_status_exclusions() {
    let fixture = TestDir::new("handler_rejects_invalid_index_status_exclusions");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).expect("root fixture should be created");
    let index_path = fixture.path().join("index.txt");
    save_scanned_index(&index_path, &root);

    let response = handle_json_line(
        &index_path,
        r#"{"id":15,"method":"index_status","params":{"exclude_names":"node_modules"}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":15,\"error\":{\"message\":\"exclude_names must be an array of strings\"}}\n"
    );
}
```

- [ ] **Step 3: Run the focused tests and verify RED**

Run:

```bash
cargo test -p ai-file-search-daemon --test handler_tests
```

Expected: FAIL. New direct calls return `unknown method: index_status`, and the catalog assertion lacks the new item. Confirm failures are caused by the missing method, not fixture or syntax errors.

- [ ] **Step 4: Dispatch and advertise `index_status`**

In `handle_json_request`, add this match arm before `refresh | reindex`:

```rust
"index_status" => HandlerOutcome {
    response: index_status(index_path, &request),
    shutdown_requested: false,
},
```

In `method_catalog`, add this item immediately after `ping`:

```rust
{
    "name": "index_status",
    "params": {
        "root": "optional string; must match stored root",
        "exclude_names": "optional string array",
    },
},
```

- [ ] **Step 5: Add the minimal read-only handler**

Rename `refresh_root` to `index_root` and update the existing call in `refresh`:

```rust
let root = match index_root(&store, &request.params) {
    Ok(root) => root,
    Err(message) => return Response::error(request.id, message),
};
```

Place this function immediately before `refresh`:

```rust
fn index_status(index_path: &Path, request: &Request) -> Response {
    let options = match scan_options(&request.params) {
        Ok(options) => options,
        Err(message) => return Response::error(request.id, message),
    };

    let store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => return Response::error(request.id, format!("index open failed: {error}")),
    };
    let root = match index_root(&store, &request.params) {
        Ok(root) => root,
        Err(message) => return Response::error(request.id, message),
    };

    let files = match scan_files_for_index(&root, index_path, options) {
        Ok(files) => files,
        Err(error) => return Response::error(request.id, format!("scan failed: {error}")),
    };
    let scanned_files = files.len();
    let summary = RefreshSummary::compare(&store.all_files(), &files);
    let needs_refresh = summary.added > 0 || summary.updated > 0 || summary.removed > 0;

    Response::success(
        request.id,
        json!({
            "scanned_files": scanned_files,
            "added": summary.added,
            "updated": summary.updated,
            "removed": summary.removed,
            "unchanged": summary.unchanged,
            "needs_refresh": needs_refresh,
        }),
    )
}
```

Rename the helper declaration without changing its body or error strings:

```rust
fn index_root(
    store: &FileIndexStore,
    params: &serde_json::Value,
) -> Result<PathBuf, &'static str> {
```

Do not call `set_root_path`, `replace_all`, or `save` inside `index_status`.

- [ ] **Step 6: Run focused tests and verify GREEN**

Run:

```bash
cargo test -p ai-file-search-daemon --test handler_tests
```

Expected: PASS, including all existing handler tests and the five new `index_status` tests.

- [ ] **Step 7: Run implementation verification**

Run each command and require exit code 0:

```bash
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: formatting clean, all workspace tests pass, and clippy reports no warnings.

- [ ] **Step 8: Commit and push the tested implementation**

```bash
git add crates/daemon/src/lib.rs crates/daemon/tests/handler_tests.rs
git commit -m "feat: add daemon index status method"
git push origin main
```

Expected: the commit is created on `main`, ordinary HTTPS push succeeds, and `git status --short --branch` reports `main...origin/main` with no pending changes.

### Task 2: Document the AI-facing contract and finish verification

**Files:**
- Modify: `README.md:163-190`

- [ ] **Step 1: Document the safety boundary and method contract**

Update the service safety bullet to include `index_status`:

```markdown
- `ai-file-search-daemon service start` requires an index file with stored root metadata; `index_status`, `refresh`, and `reindex` reject explicit roots that differ from that stored root.
```

Add the method between `ping` and `refresh` in the JSON-RPC method list:

```text
index_status -> params {"root":"optional; must match stored root","exclude_names":["optional"]}; returns needs_refresh and change counts without saving
```

Update the file-watching limitation so the full-rescan cost is explicit:

```markdown
- File watching and true incremental updates are not implemented yet; `index_status` and `refresh` currently perform full rescans.
```

- [ ] **Step 2: Verify the README exposes the complete method**

Run:

```bash
rg -n "index_status|needs_refresh|full rescans" README.md
```

Expected: matches in the safety bullet, JSON-RPC method list, and MVP limitation.

- [ ] **Step 3: Run final repository verification**

Run each command and inspect the complete output:

```bash
cargo fmt --check
cargo test -p ai-file-search-daemon --test handler_tests
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
```

Expected: every command exits 0; the focused handler suite includes the new tests; the full workspace has zero failures; clippy and diff checks are clean.

- [ ] **Step 4: Commit and push the documentation**

```bash
git add README.md
git commit -m "docs: document daemon index status method"
git push origin main
```

Expected: the documentation commit is on `main`, ordinary HTTPS push succeeds, `HEAD` equals `origin/main`, and the worktree is clean.
