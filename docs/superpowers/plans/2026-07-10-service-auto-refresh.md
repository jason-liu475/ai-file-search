# Service Auto Refresh Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the daemon service optionally refresh its persistent index on a bounded, low-memory interval without restarting, while keeping automatic work serialized with JSON-RPC requests.

**Architecture:** Parse and persist one optional refresh interval at service startup. Refactor scanning and index comparison into a shared internal operation that can either always save (manual `refresh`) or save only on change (scheduled refresh). The existing platform IPC loops own a Tokio interval and `select!` between one IPC accept/connect operation and an automatic refresh tick, so one process and one index mutation path remain authoritative.

**Tech Stack:** Rust 2021 workspace, Tokio named-pipe/Unix-socket IPC, serde JSON state, `tempfile` fixtures, cargo test/clippy/fmt.

---

## Preconditions

- Work directly on `main`; do not create a development branch or worktree.
- Keep the default service behavior unchanged: automatic refresh is disabled unless explicitly configured.
- Keep the option range inclusive: `30..=86_400` seconds.
- Preserve the established CLI convention: usage errors print to stderr and return exit code `2`.
- Do not add a watcher, polling worker process, background thread, telemetry stream, or a new public JSON-RPC method in this change.

## Task 1: Parse and Persist the Refresh Configuration

**Files:**
- Modify: `crates/daemon/src/lib.rs`
- Modify: `crates/daemon/src/main.rs`
- Modify: `crates/daemon/src/service.rs`
- Modify: `crates/daemon/tests/service_cli_tests.rs`
- Modify: `crates/daemon/tests/service_state_tests.rs`

- [ ] **Step 1: Write failing state round-trip and legacy-compatibility tests.**

In `crates/daemon/tests/service_state_tests.rs`, add coverage that writes a `ServiceState` with `auto_refresh_seconds: Some(300)`, reads it back, and checks the value survives. Add a separate fixture containing the legacy JSON shape without the field and assert that deserialization produces `None`.

```rust
let expected = ServiceState {
    endpoint: "auto-refresh-test".to_owned(),
    pid: 42,
    index_path: PathBuf::from("index.json"),
    started_unix_seconds: 1,
    auto_refresh_seconds: Some(300),
};
write_service_state(&state_path, &expected).unwrap();
assert_eq!(read_service_state(&state_path).unwrap(), Some(expected));

std::fs::write(
    &state_path,
    r#"{"endpoint":"legacy","pid":42,"index_path":"index.json","started_unix_seconds":1}"#,
)
.unwrap();
assert_eq!(read_service_state(&state_path).unwrap().unwrap().auto_refresh_seconds, None);
```

Add renderer assertions for both output modes:

```rust
assert!(render_status_json(&ServiceStatus::Running(running_with_auto.clone()))
    .contains("\"auto_refresh_seconds\":300"));
assert!(render_status_text(&ServiceStatus::Running(running_with_auto))
    .contains("auto refresh: 300s"));
assert!(!render_status_text(&ServiceStatus::Running(running_without_auto))
    .contains("auto refresh:"));
```

- [ ] **Step 2: Run the focused state test and confirm it fails for the missing field.**

Run:

```powershell
cargo test -p ai-file-search-daemon --test service_state_tests
```

Expected: compile failures referring to the missing `auto_refresh_seconds` field or assertions failing because the renderer has no configuration output.

- [ ] **Step 3: Add the state field with backward-compatible serde behavior.**

In `crates/daemon/src/service.rs`, extend the exact persisted structure:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceState {
    pub endpoint: String,
    pub pid: u32,
    pub index_path: PathBuf,
    pub started_unix_seconds: u64,
    #[serde(default)]
    pub auto_refresh_seconds: Option<u64>,
}
```

In `render_status_json` and `render_status_text`, retain the current fields and only append `auto_refresh_seconds` to the running JSON payload when the option is `Some`. For text output, append exactly `auto refresh: <seconds>s` only when configured. Do not synthesize an `auto refresh: disabled` line; an absent configuration is the established default.

- [ ] **Step 4: Write failing CLI parser tests.**

In `crates/daemon/tests/service_cli_tests.rs`, add table-driven calls to the existing command test helper. Cover:

```text
service start index.json --auto-refresh-seconds 300                  => success
service start index.json --endpoint endpoint --auto-refresh-seconds 300 => success
service start index.json --auto-refresh-seconds 300 --endpoint endpoint => success
service start index.json --auto-refresh-seconds 29                   => exit 2
service start index.json --auto-refresh-seconds 86401                => exit 2
service start index.json --auto-refresh-seconds nope                 => exit 2
service start index.json --auto-refresh-seconds                      => exit 2
service start index.json --auto-refresh-seconds 300 --auto-refresh-seconds 301 => exit 2
```

For parser-only tests, arrange the daemon state fixture so the command does not need to launch a real child process, following the existing `run_with_state` patterns. Assert an error message contains the option name and allowed range rather than pinning the complete wording.

Also add a hidden-command test that invokes:

```text
service-run index.json endpoint --auto-refresh-seconds 300
```

and verifies that `main.rs` accepts the argument shape and forwards it to the library. Keep that test non-blocking by testing parsing/dispatch extraction rather than running a persistent IPC server.

- [ ] **Step 5: Run the focused CLI test and confirm it fails.**

Run:

```powershell
cargo test -p ai-file-search-daemon --test service_cli_tests
```

Expected: the new flag is rejected as an unknown/invalid `service start` argument and hidden `service-run` rejects four arguments.

- [ ] **Step 6: Implement one canonical service-start argument parser.**

In `crates/daemon/src/lib.rs`, replace the positional-only `parse_service_start_args` result with a private parsed configuration type, for example:

```rust
const MIN_AUTO_REFRESH_SECONDS: u64 = 30;
const MAX_AUTO_REFRESH_SECONDS: u64 = 86_400;

struct ServiceStartArgs<'a> {
    index_path: &'a str,
    endpoint: String,
    auto_refresh_seconds: Option<u64>,
}
```

Parse flags in a small left-to-right loop after the required index path. Permit `--endpoint <name>` and `--auto-refresh-seconds <seconds>` in either order, reject duplicates, missing values, unknown flags, zero/non-numeric values, and values outside the inclusive constants. Return the existing command-level error shape so `run_async` returns `2` for usage errors.

Keep the interval validation in a reusable private helper:

```rust
fn parse_auto_refresh_seconds(value: &str) -> Result<u64, String> {
    let seconds = value.parse::<u64>().map_err(|_| {
        format!("--auto-refresh-seconds must be an integer from {MIN_AUTO_REFRESH_SECONDS} to {MAX_AUTO_REFRESH_SECONDS}")
    })?;
    if !(MIN_AUTO_REFRESH_SECONDS..=MAX_AUTO_REFRESH_SECONDS).contains(&seconds) {
        return Err(format!("--auto-refresh-seconds must be between {MIN_AUTO_REFRESH_SECONDS} and {MAX_AUTO_REFRESH_SECONDS} seconds"));
    }
    Ok(seconds)
}
```

Thread `auto_refresh_seconds` through these existing call sites without changing the no-flag path:

```rust
service_start(index_path, endpoint, auto_refresh_seconds, state_path).await
spawn_service_child(index_path, endpoint, auto_refresh_seconds)
wait_for_started_service(endpoint, index_path, auto_refresh_seconds, state_path, pid).await
service_run(index_path, endpoint, auto_refresh_seconds).await
```

When writing `ServiceState`, store the parsed value. In `crates/daemon/src/main.rs`, accept exactly the three-argument and five-argument hidden forms, validate the optional flag through the same public service-run entry point, and leave it hidden from normal command help.

- [ ] **Step 7: Run formatting and focused tests.**

Run:

```powershell
cargo fmt --check
cargo test -p ai-file-search-daemon --test service_state_tests
cargo test -p ai-file-search-daemon --test service_cli_tests
```

Expected: all pass, including legacy state parsing and invalid option paths.

- [ ] **Step 8: Commit and push the configuration slice.**

```powershell
git add crates/daemon/src/lib.rs crates/daemon/src/main.rs crates/daemon/src/service.rs crates/daemon/tests/service_cli_tests.rs crates/daemon/tests/service_state_tests.rs
git commit -m "feat: configure service auto refresh"
git push origin main
```

## Task 2: Share the Scan/Compare Path and Avoid Unchanged Writes

**Files:**
- Modify: `crates/daemon/src/lib.rs`
- Add or modify unit tests in: `crates/daemon/src/lib.rs`
- Modify if needed: `crates/daemon/Cargo.toml`

- [ ] **Step 1: Add failing internal refresh tests beside the private implementation.**

Add a `#[cfg(test)] mod auto_refresh_tests` at the bottom of `crates/daemon/src/lib.rs`, where tests can exercise private helpers without expanding the crate's public API. Reuse `tempfile::tempdir()` and existing test fixture conventions. Cover these cases:

1. An unchanged index returns `saved == false` and leaves the index bytes exactly unchanged.
2. A newly created file returns `saved == true`, increments the added summary, and persists the new entry.
3. A deleted file returns `saved == true`, increments removed, and removes the persisted entry.
4. The index file and daemon state file are not indexed when the stored root is relative but the supplied index/state paths resolve to absolute paths.
5. A missing or unreadable stored root returns an error and does not overwrite the old index file.

Use a small result type internal to the module, not a JSON-RPC response:

```rust
let result = refresh_if_changed(&index_path).unwrap();
assert!(!result.saved);
assert_eq!(std::fs::read(&index_path).unwrap(), before);
```

- [ ] **Step 2: Run the library unit tests and confirm the helper does not exist.**

Run:

```powershell
cargo test -p ai-file-search-daemon --lib
```

Expected: compilation failure for `refresh_if_changed` and its result type, or failing placeholders added in the preceding step.

- [ ] **Step 3: Extract a private scan-and-compare primitive without altering RPC contracts.**

In `crates/daemon/src/lib.rs`, identify the common work now repeated by `refresh` and `index_status`:

1. Open the store and resolve the persisted root or an explicitly supplied root.
2. Normalize index and service-state paths for self-exclusion.
3. Walk the root and create candidate indexed files.
4. Compare candidates to the stored entries to produce `added`, `updated`, `removed`, and `unchanged`.

Represent that private result using owned data sufficient to apply it later, for example:

```rust
struct ScannedIndex {
    store: IndexStore,
    files: Vec<IndexedFile>,
    summary: RefreshSummary,
}
```

The exact existing store/file types should be used rather than duplicating serialized structures. Preserve current error messages for the public `refresh` and `index_status` methods by converting shared helper errors at their current response boundaries.

Implement the scheduled path as a private operation:

```rust
struct AutoRefreshResult {
    summary: RefreshSummary,
    saved: bool,
}

fn refresh_if_changed(index_path: &Path) -> Result<AutoRefreshResult, String> {
    let scanned = scan_stored_index(index_path)?;
    let saved = scanned.summary.changed();
    if saved {
        scanned.store.replace_files(scanned.files);
        scanned.store.save(index_path).map_err(|error| error.to_string())?;
    }
    Ok(AutoRefreshResult { summary: scanned.summary, saved })
}
```

Use the project's existing definition of whether a refresh changed the index; if there is no helper today, define `RefreshSummary::has_changes()` as `added + updated + removed > 0`. Do not rewrite an unchanged store and do not update service-state JSON per tick.

Manual JSON-RPC `refresh` must keep its existing externally observable behavior: scan once, replace/save once, and return its current summary. JSON-RPC `index_status` must remain read-only and must continue to discard the scanned file list after comparison.

- [ ] **Step 4: Preserve failure safety and self-exclusion behavior.**

Ensure `refresh_if_changed` completes all scanning and comparison before calling `replace_files` and `save`. On scan/root failure it returns an error without mutating in-memory data that will be persisted. Reuse the existing canonicalization-with-fallback comparison for exclusions; do not introduce raw-string path comparisons.

- [ ] **Step 5: Run unit, handler, and workspace tests.**

Run:

```powershell
cargo fmt --check
cargo test -p ai-file-search-daemon --lib
cargo test -p ai-file-search-daemon --test handler_tests
cargo test --workspace
```

Expected: the new no-write/change/failure tests pass and existing manual refresh plus `index_status` behavior remains stable.

- [ ] **Step 6: Commit and push the internal refresh slice.**

```powershell
git add crates/daemon/src/lib.rs crates/daemon/Cargo.toml
git commit -m "feat: refresh service index only when changed"
git push origin main
```

Only include `crates/daemon/Cargo.toml` if Task 3 test timing requires Tokio's `test-util` feature; otherwise omit it from this commit.

## Task 3: Run the Cooperative Scheduler Inside Each IPC Loop

**Files:**
- Modify: `crates/daemon/src/lib.rs`
- Modify: `crates/daemon/Cargo.toml` only if needed for deterministic paused-time tests
- Modify: `crates/daemon/tests/transport_tests.rs`
- Modify: `README.md`

- [ ] **Step 1: Add deterministic scheduler tests before wiring production loops.**

Add private Tokio tests in `lib.rs` for a small scheduler construction helper. If using Tokio paused time, add `test-util` to the existing Tokio feature list in `crates/daemon/Cargo.toml` and use:

```rust
#[tokio::test(start_paused = true)]
async fn first_auto_refresh_tick_waits_for_the_configured_period() {
    let mut interval = auto_refresh_interval(Some(Duration::from_secs(30))).unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(0), interval.tick()).await.is_err());
    tokio::time::advance(Duration::from_secs(30)).await;
    interval.tick().await;
}
```

Add a second test that advances several periods while refresh work is considered occupied and verifies that the interval uses `MissedTickBehavior::Skip`, so at most one overdue refresh is handled when control returns. Keep the unit under test a scheduler helper or an injected `Interval`; do not make a test wait for real 30-second wall time.

In `transport_tests.rs`, add a regression test that starts the IPC handler with automatic refresh disabled and verifies the existing request/response path is unchanged. This guards the default configuration while the platform loops are refactored.

- [ ] **Step 2: Run focused tests and confirm the scheduler helper is absent.**

Run:

```powershell
cargo test -p ai-file-search-daemon --lib
cargo test -p ai-file-search-daemon --test transport_tests
```

Expected: failures for `auto_refresh_interval` or the missing timer behavior, while the pre-existing transport test should remain green.

- [ ] **Step 3: Create an optional interval with delayed first tick and skip semantics.**

In `crates/daemon/src/lib.rs`, import Tokio time types needed for an interval:

```rust
use tokio::time::{interval_at, sleep, Duration, Instant, Interval, MissedTickBehavior};
```

Implement a private constructor that creates no timer for `None` and a timer whose first tick is one complete period in the future:

```rust
fn auto_refresh_interval(period: Option<Duration>) -> Option<Interval> {
    let period = period?;
    let mut interval = interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    Some(interval)
}
```

Production input has already been range-validated. Keep the helper generic over `Duration` so paused-time tests can use a short test duration without weakening CLI validation.

- [ ] **Step 4: Extend the service-run path and Unix IPC loop.**

Change the public internal entry point to receive configuration:

```rust
pub async fn service_run(
    index_path: &Path,
    endpoint: &str,
    auto_refresh_seconds: Option<u64>,
) -> i32
```

Convert seconds once to `Duration` and pass the resulting optional period into the platform server loop. Retain a small no-auto wrapper only if existing tests or callers need it.

In the Unix socket loop, initialize one `Option<Interval>` outside the accept loop. With an active timer, use `tokio::select!` between `listener.accept()` and `timer.tick()`; on a tick call `refresh_if_changed(index_path)`. Log the error through the existing daemon stderr/logging convention and continue serving. On accept, retain the existing `handle_json_stream` behavior. With no timer, keep the current direct `accept().await` path to avoid incidental behavior changes.

The tick branch must be awaited to completion before another select begins. This deliberately makes a full scan delay IPC requests, preventing overlapping scans, saves, or store races.

- [ ] **Step 5: Apply the same serialized behavior to the Windows named-pipe loop.**

For each iteration, create the next named-pipe server as today. When a timer exists, select between `server.connect()` and `timer.tick()`. If a tick wins, run `refresh_if_changed`, record any error, drop the unconnected server, and begin the next loop iteration. If connect wins, keep the present JSON stream handling. With no configured timer, leave the existing connect/handler code path unchanged.

Do not create a second named-pipe server, Tokio task, channel, mutex, or background thread. The one loop must own both incoming connections and scheduled work.

- [ ] **Step 6: Handle automatic refresh errors and shutdown correctly.**

An automatic scan error must not terminate the daemon or overwrite the index. Emit a concise diagnostic containing `automatic refresh failed` and let the next interval try again. A shutdown signal that arrives during a scan is observed after that scan completes; no special cancellation or partial write mechanism is added in this MVP.

- [ ] **Step 7: Update user documentation.**

In `README.md`, add the opt-in command next to the existing service start example:

```powershell
ai-file-search-daemon service start <index-file> --auto-refresh-seconds 300
```

Document these exact semantics in the service behavior section:

- disabled by default;
- allowed range is 30 through 86,400 seconds;
- first automatic scan happens after a full interval;
- unchanged scans do not rewrite the index file;
- automatic scans share the daemon loop with IPC and therefore wait/serialize with RPC work;
- changing the interval requires `service stop` then `service start` again;
- automatic failures preserve the last index and retry at the next scheduled interval.

Also update the status output description to state that `auto_refresh_seconds` appears only while configured. Do not claim file-system watcher support.

- [ ] **Step 8: Run full verification.**

Run, in order:

```powershell
cargo fmt --check
cargo test -p ai-file-search-daemon --lib
cargo test -p ai-file-search-daemon --test service_state_tests
cargo test -p ai-file-search-daemon --test service_cli_tests
cargo test -p ai-file-search-daemon --test transport_tests
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
git status --short
```

Then perform a manual process smoke test with an interval that is valid but short enough to inspect without waiting too long only when the environment permits it; otherwise explicitly record that the deterministic paused-time tests cover timer timing. Verify:

```powershell
ai-file-search-daemon service start .\index.json --auto-refresh-seconds 30
ai-file-search-daemon service status --json
ai-file-search-daemon service stop
```

The JSON status must contain `"auto_refresh_seconds":30`; stopping must leave no running service state.

- [ ] **Step 9: Commit and push the scheduler and documentation.**

```powershell
git add crates/daemon/src/lib.rs crates/daemon/Cargo.toml crates/daemon/tests/transport_tests.rs README.md
git commit -m "feat: add cooperative service auto refresh"
git push origin main
```

Stage only files actually changed. Before committing, inspect `git diff --check`, the complete test output, and `git status --short`; do not include unrelated user changes.

## Final Review Checklist

- [ ] `service start` without the new option preserves existing behavior and status output.
- [ ] Invalid refresh interval input exits with code `2`, including duplicate and missing-value cases.
- [ ] Legacy state files load with automatic refresh disabled.
- [ ] A changed automatic scan saves exactly once; an unchanged scan does not alter index bytes.
- [ ] Existing `refresh`, `reindex`, and `index_status` JSON-RPC behavior and root validation remain covered by the workspace tests.
- [ ] Both Unix and Windows IPC loops serialize automatic work with one accepted/connected request at a time.
- [ ] Auto-refresh errors do not crash the daemon or replace its last good index.
- [ ] README states the opt-in/default/range/restart semantics accurately.
