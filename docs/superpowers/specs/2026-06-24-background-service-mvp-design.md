# Background Service MVP Design

Reviewed: 2026-09-30. The managed-process MVP is implemented. The ownership, bounded-health-I/O, private-endpoint, atomic-state, and error-state corrections below are revised contracts, not claims that the current code already enforces them. They are prerequisites for unattended automatic refresh.

## Goal

Make the daemon usable as a lightweight background process that can be started, checked, and stopped through cross-platform commands without requiring administrator privileges or OS service installation.

This MVP offers a local entry point for AI tools without HTTP or administrator setup. Full scans still block the serial daemon and use O(N) metadata memory. Local IPC alone does not enforce a caller permission boundary.

## Scope

Build application-level service management inside `ai-file-search-daemon`:

- `service start <index-file> [--endpoint <name>]`
- `service status [--json]`
- `service stop`
- JSON-RPC `ping`
- JSON-RPC `shutdown`
- A local state file that records the managed daemon endpoint, process id, index path, and start timestamp

The existing commands stay supported:

- `stdio <index-file>`
- `ipc <index-file> <endpoint>`
- `ipc-request <endpoint> [json-line]`
- `handle <index-file> <json-line>`

## Non-Goals

This MVP intentionally does not implement:

- Windows Service, systemd, or launchd installation
- Start-on-login or start-on-boot
- Tray UI
- File watching
- Multi-user permission isolation
- Authentication or authorization
- Content indexing

These can be layered on later once the local daemon lifecycle is reliable.

## Recommended Approach

Use a managed background child process instead of OS service frameworks.

`service start` launches the same executable in a hidden/background mode:

```text
ai-file-search-daemon service-run <index-file> <endpoint> [--auto-refresh-seconds <seconds>]
```

`service-run` serves the existing platform IPC transport and writes no interactive output except fatal errors. This keeps the service runtime path close to the already-tested `ipc` command while allowing `service start` to own state-file creation.

This approach is preferred because it:

- Works across Windows, macOS, and Linux without elevated permissions
- Avoids OS-specific service-install code in the first service MVP
- Keeps the transport as Named Pipe on Windows and Unix Domain Socket on Unix
- Gives AI clients a stable local endpoint without HTTP
- Can later become the implementation behind OS service wrappers

## CLI Behavior

### `service start <index-file> [--endpoint <name>]`

Behavior:

1. Resolve `index-file` to an absolute path.
2. Resolve the exact state path, including `AIFS_SERVICE_STATE`, and the endpoint into a private per-user namespace. The prototype default remains `aifs-service`; the hardened Unix default must be an absolute path under a private runtime directory, and Windows needs first-instance protection and current-user access restrictions.
3. Hold a short-lived startup coordination guard; inspect child-lifetime ownership separately. This lets the child acquire its lifetime guard before the parent publishes state and releases startup coordination, with no unowned handoff window.
4. If state exists and `ping` succeeds, verify that the requested index/endpoint matches before reporting an already-running instance. Report that configuration is retained rather than pretending a new interval took effect.
5. A failed or timed-out `ping` is unknown/busy, not proof of stale state. If startup or child-lifetime ownership is held, do not replace state, unlink the endpoint, or spawn a duplicate. Malformed state is an explicit error.
6. Spawn `service-run` as a background child, retaining its `Child` handle. The child acquires managed-instance and index-writer ownership before binding. Pass absolute index/state paths and optional configuration explicitly.
7. Poll readiness with bounded I/O. On readiness failure, stop and reap only the child spawned by this attempt; do not use an advisory saved PID as kill authority.
8. Atomically publish state with endpoint, pid, index path, start timestamp, and optional interval. If this fails, stop/reap the owned child and clean only artifacts this attempt owns.
9. Print a concise success message.

Exit codes:

- `0` when the service is running after the command
- `1` when the child cannot be spawned or does not become healthy
- `2` for usage errors

### `service status [--json]`

Behavior:

1. Load the state file.
2. If no state file exists and neither startup nor child-lifetime ownership is held, report `stopped`. If startup is active, report `starting`; a live owner without readable state is `unresponsive`, not stopped.
3. If state exists and `ping` succeeds, report `running`.
4. If state exists but `ping` fails, inspect managed ownership: a live owner is `unresponsive`, not `stale`. Report stale only when ownership is absent and endpoint cleanup is safe. Malformed or unreadable state is an error, not stopped.
5. With `--json`, print a machine-readable object.

Human output examples:

```text
running endpoint=aifs-service pid=12345 index=C:\path\index.txt
stale endpoint=aifs-service pid=12345 index=C:\path\index.txt
stopped
```

JSON output examples:

```json
{"status":"running","endpoint":"aifs-service","pid":12345,"index_path":"C:\\path\\index.txt","started_unix_seconds":1782281286}
{"status":"stale","endpoint":"aifs-service","pid":12345,"index_path":"C:\\path\\index.txt","started_unix_seconds":1782281286}
{"status":"stopped"}
```

Exit codes:

- `0` for `running` and `stopped`
- `1` for `starting`, `unresponsive`, `stale`, or unreadable/malformed state
- `2` for usage errors

The revised machine-readable status names are exactly `running`, `stopped`, `starting`, `unresponsive`, `stale`, and `error`. Error output carries a concise reason; do not invent a separate `busy` status or require metadata fields when no readable state exists. Existing healthy/stopped JSON stays unchanged.

### `service stop`

Behavior:

1. Load the state file.
2. If no state file exists and ownership is absent, report already stopped and return success. If an owner exists without usable state, report unresponsive with exit code `1` rather than claiming it stopped.
3. Send JSON-RPC `shutdown` to the stored endpoint.
4. After shutdown acknowledgement, wait for owned lifetime/index guards and the endpoint to be released; process tests also confirm child exit. One failed `ping` is not enough to prove exit.
5. Remove only matching owned state/endpoint artifacts after confirmed release. A regular file at the endpoint is never removed as stale socket cleanup.
6. If shutdown cannot be delivered or the owner is busy, retain state and report the condition. Do not kill an unrelated PID or spawn a replacement to recover from a slow scan.

Exit codes:

- `0` when stopped or already stopped
- `1` when shutdown fails and the endpoint still appears reachable
- `2` for usage errors

## JSON-RPC Additions

### `ping`

Request:

```json
{"id":1,"method":"ping","params":{}}
```

Response:

```json
{"id":1,"result":{"status":"ok"}}
```

### `shutdown`

Request:

```json
{"id":2,"method":"shutdown","params":{}}
```

Response:

```json
{"id":2,"result":{"status":"shutting_down"}}
```

`shutdown` is only needed for service-managed daemon instances. It should be available through the daemon handler, but normal `stdio` and `ipc` users are expected to keep using process control if they started the process manually.

## State File

Use a user-local state file. The exact directory is resolved by Rust standard environment APIs to avoid introducing a heavy dependency.

Proposed locations:

- Windows: `%LOCALAPPDATA%\ai-file-search\service-state.json`
- Unix: `$XDG_STATE_HOME/ai-file-search/service-state.json` when set, otherwise `$HOME/.local/state/ai-file-search/service-state.json`
- Fallback for tests or unusual environments: `std::env::temp_dir()/ai-file-search/service-state.json`

State schema:

```json
{
  "endpoint": "aifs-service",
  "pid": 12345,
  "index_path": "C:\\path\\index.txt",
  "started_unix_seconds": 1782281286
}
```

The state file and PID are advisory. A successful `ping` proves an endpoint responded, not ownership of the intended index; a failed `ping` does not prove process death. Reconcile bounded health checks with managed-instance/index guards and matching identity before startup, cleanup, or stop decisions.

Write state atomically, do not rewrite it per automatic scan, and pass the resolved absolute state path to the child for artifact self-exclusion. Configuration-only state is not evidence that scheduled refresh has run.

## Internal Components

### Service State Module

Create `crates/daemon/src/service.rs`.

Responsibilities:

- Represent `ServiceState`
- Resolve the default state path
- Read/write/remove state
- Render status responses for CLI output
- Keep file IO separate from IPC transport and JSON-RPC handling

### Daemon Runtime

Extend `crates/daemon/src/lib.rs`.

Responsibilities:

- Add `ping`
- Add shutdown-aware stream serving
- Preserve existing `stats` and `search` behavior
- Keep transport code reusable by `ipc` and `service-run`

### CLI Entry

Extend `crates/daemon/src/main.rs`.

Responsibilities:

- Parse `service start/status/stop`
- Parse hidden `service-run`
- Spawn background child for `service start`
- Call state and IPC helpers
- Keep usage messages concise

## Error Handling

- Invalid CLI arguments return exit code `2`.
- Missing state file is not an error for `status` or `stop`.
- Unreachable endpoint with held ownership is unresponsive; do not assume stale from a timeout.
- Failure to write state or become ready must stop and reap the child created by this attempt, then report the error.
- Malformed state is an explicit readable error and must not trigger blind replacement or silently report stopped.
- Existing `ipc` behavior remains unchanged for manually started daemon processes.

## Testing Strategy

### Unit Tests

Add tests for:

- State file round trip
- Missing state file reports stopped
- Malformed state file reports an explicit error, not stopped
- Concurrent start and a busy live owner do not create a duplicate child
- Readiness/state-write failure reaps the owned child and preserves unrelated artifacts
- Endpoint cleanup rejects a regular file or foreign live socket
- JSON status rendering
- `ping` JSON-RPC response
- `shutdown` JSON-RPC response

### Functional Tests

Add daemon CLI tests for parser-level behavior where practical:

- `service status --json` returns stopped when using an isolated test state path
- `service stop` succeeds when no state file exists
- Usage errors return non-zero status and usage text

The implementation should allow an environment variable such as `AIFS_SERVICE_STATE` in tests so commands do not touch the user's real service state.

### Smoke Tests

Manual or scripted local smoke:

1. Create a fixture and index.
2. Run `ai-file-search-daemon service start <index-file>`.
3. Run `ai-file-search-daemon service status --json`.
4. Send an IPC `stats` request to the default endpoint.
5. Run `ai-file-search-daemon service stop`.
6. Confirm `service status --json` reports stopped or stale-free state.

### Required Verification

Before commit:

```bash
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Security Notes

This MVP exposes local-only IPC, not HTTP. The service endpoint is intended for the current user's local tools. It does not claim strong access control yet.

Security-sensitive follow-ups:

- Restrict Named Pipe and Unix Socket permissions before production or unattended use; use private per-user directories/namespaces and first-instance protection.
- Never unlink a pre-existing endpoint without verified ownership, socket-type inspection, and safe stale detection.
- Add an optional per-user token or peer-credential check.
- Define a separate safe read-only API profile for AI clients.

For scheduled operation, follow the [reviewed automatic-refresh prerequisites](2026-07-10-service-auto-refresh-design.md): persisted scan scope, all-writer locking, exclusive temporary creation, failure-safe replacement, and one bounded request per managed connection. Strong AI-client authorization remains a separate production gate.

## Open Source Fit

The MVP keeps OS-specific complexity small and auditable. Contributors can run and test the service lifecycle without admin setup, which lowers onboarding cost and makes cross-platform CI easier later.
