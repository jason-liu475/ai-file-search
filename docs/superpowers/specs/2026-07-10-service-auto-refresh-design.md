# Service Auto Refresh Design

## Goal

Let a user-level daemon service refresh its saved file index at a conservative, explicitly configured interval without adding a file watcher, a second process, or concurrent index writers.

The feature gives AI clients a fresher local metadata index while preserving the current service's low-memory and local-only behavior.

## Scope

Extend service startup with an optional interval:

```text
ai-file-search-daemon service start <index-file> [--endpoint <name>] [--auto-refresh-seconds <seconds>]
```

Examples:

```text
ai-file-search-daemon service start ./index.txt
ai-file-search-daemon service start ./index.txt --auto-refresh-seconds 300
ai-file-search-daemon service start ./index.txt --endpoint aifs-docs --auto-refresh-seconds 900
```

The option is disabled by default. When set, the service rescans the stored root at the configured interval and saves a new index only when file metadata changed.

## Non-Goals

This MVP does not add:

- File-system watchers or native change notifications
- Automatic refresh by default
- A second worker process or a thread pool dedicated to scanning
- Concurrent index reads and writes
- Runtime interval changes without restarting the service
- Persisted last-refresh timestamps, refresh history, or failure telemetry
- HTTP, network listeners, or remote scheduling
- Content indexing

## Chosen Approach

Use a cooperative timer inside the existing `service-run` IPC loop.

The service loop waits for either an IPC connection or the next timer tick. On a tick it runs one synchronous scan-and-compare operation, saves only when the comparison reports changes, and then resumes accepting IPC requests.

This approach is preferred over an external scheduler because it keeps lifecycle, endpoint, and index-root ownership in one process. It is preferred over a watcher because it has predictable memory use and avoids platform-specific event coalescing, overflow recovery, and permission behavior.

The timer is deliberately cooperative rather than a background refresh task. A full scan may delay IPC handling until that scan completes, but the service never has two index operations in flight and cannot race an automatic writer against a client `refresh` request.

## CLI Contract

### `--auto-refresh-seconds <seconds>`

- Optional; absent means automatic refresh is disabled.
- Accepts an unsigned decimal integer from `30` through `86400`, inclusive.
- Values below `30`, above `86400`, non-numeric values, repeated flags, and missing values are usage errors with exit code `2`.
- The flag may be used with `--endpoint` in either order.
- A service that is already running keeps its current configuration; `service start` returns the existing running result and does not reconfigure it.
- Changing the interval requires `service stop`, followed by `service start` with the new value.

The hidden child invocation carries the interval only when it is configured:

```text
ai-file-search-daemon service-run <index-file> <endpoint> [--auto-refresh-seconds <seconds>]
```

## Service State

Add `auto_refresh_seconds: Option<u64>` to `ServiceState`.

- Existing state files that omit the field deserialize as `None`.
- `service status --json` includes the configured value when automatic refresh is enabled.
- Human-readable running and stale output appends `auto_refresh_seconds=<seconds>` only when configured.
- The state file records startup configuration only. The service does not rewrite it on every timer tick.

## Scheduling Behavior

1. `service start` still validates that the index contains root metadata before spawning the child.
2. The first automatic scan occurs after one complete configured interval. Startup performs no extra scan.
3. Each subsequent tick is scheduled from the configured period.
4. Missed ticks use skip semantics. A slow scan or a suspended machine never causes immediate catch-up scans.
5. The service executes at most one automatic scan at a time.
6. While an automatic scan is running, incoming IPC requests wait. This preserves the current sequential index access model and prevents read/write races without allocating a duplicate in-memory index.
7. A client-issued `refresh` or `reindex` remains supported and uses the existing root safety boundary. It runs in the same service loop, so it cannot overlap an automatic scan.
8. A shutdown request received after a scan starts is processed after that scan completes.

## Refresh Semantics

Automatic refresh reuses the same scanner, index-file exclusion, root resolution, and metadata comparison as the daemon's manual refresh path.

The implementation must factor the scan-and-compare portion so automatic refresh scans only once per tick:

1. Open the saved `FileIndexStore`.
2. Resolve the stored root with the existing root safety logic.
3. Scan the root using default scan options and exclude the index file itself when it is under the root.
4. Compare scanned files with saved files using `RefreshSummary::compare`.
5. When `added`, `updated`, and `removed` are all zero, leave the index file unchanged.
6. When any of those counts is nonzero, replace the saved files, preserve the resolved root metadata, and save the index.

This operation is internal. It does not add a JSON-RPC method and does not change the response contract of `refresh`, `reindex`, or `index_status`.

## Failure Behavior

- A scan, index-open, or index-save failure during an automatic tick does not terminate the service.
- The service preserves the existing index file when the automatic operation fails.
- The next scheduled tick remains eligible to run.
- This MVP does not persist a last-error field or log file. A client can use `index_status` after the service remains reachable to inspect current staleness.

## Internal Boundaries

The implementation remains in the daemon crate.

- `service.rs` owns the serializable startup configuration in `ServiceState` and renders it for `service status`.
- `lib.rs` owns CLI parsing, child spawning, the service-run timer, and scan-and-compare helpers.
- The IPC transport continues to own connection creation and JSON line framing.
- The indexer crate remains responsible for scanning and `RefreshSummary`; no new dependency is required.

The existing no-interval `service-run` and `serve_ipc` behavior must remain unchanged from a client's perspective.

## Testing Strategy

### Unit and Parser Tests

- Service startup accepts no auto-refresh option and retains the current default behavior.
- Valid intervals at `30` and `86400` parse successfully.
- Invalid, repeated, missing, and out-of-range interval values return usage errors.
- `ServiceState` round-trips with an interval and reads legacy state JSON without the field.
- Text and JSON service status expose the interval only when configured.

### Daemon Behavior Tests

- A single automatic tick with no changes leaves the index file byte-for-byte unchanged.
- A single automatic tick after an added, updated, or removed file saves the changed index and produces the expected `RefreshSummary` counts.
- The automatic path excludes an index file inside the configured root and preserves stored-root safety.
- A failed automatic scan leaves the prior saved index readable and does not stop the service loop.
- A client refresh cannot overlap the timer path; the test uses an injectable internal interval or refresh hook rather than waiting 30 real seconds.
- Missed-tick handling is tested with an internal short duration and verifies that a completed slow refresh does not trigger a catch-up loop.

### Regression Tests

- Existing service start, status, stop, IPC, refresh, reindex, and index-status tests remain unchanged and pass.
- Existing state files without `auto_refresh_seconds` continue to report a valid service status.

## Required Verification

Before the implementation commit:

```bash
cargo fmt --check
cargo test -p ai-file-search-daemon
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Follow-Up

Native file watching is the next separate design candidate once polling behavior, scan cost, service responsiveness, and error observability have production measurements. It must not be folded into this interval-based MVP.
