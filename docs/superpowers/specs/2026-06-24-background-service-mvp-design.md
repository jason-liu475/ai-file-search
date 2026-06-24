# Background Service MVP Design

## Goal

Make the daemon usable as a lightweight background process that can be started, checked, and stopped through cross-platform commands without requiring administrator privileges or OS service installation.

This MVP turns the existing IPC daemon into a practical always-on local entry point for AI tools while keeping memory use, security exposure, and platform coupling low.

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
ai-file-search-daemon service-run <index-file> <endpoint>
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
2. Choose endpoint:
   - Use `--endpoint <name>` when provided.
   - Otherwise use the default `aifs-service`.
3. Load the state file if it exists.
4. If state exists and the endpoint answers `ping`, return success with a message that the service is already running.
5. If state exists but does not answer `ping`, treat it as stale and replace it.
6. Spawn `service-run <index-file> <endpoint>` as a detached/background child.
7. Poll the endpoint briefly until `ping` succeeds.
8. Write a state file with endpoint, pid, index path, and start timestamp.
9. Print a concise success message.

Exit codes:

- `0` when the service is running after the command
- `1` when the child cannot be spawned or does not become healthy
- `2` for usage errors

### `service status [--json]`

Behavior:

1. Load the state file.
2. If no state file exists, report `stopped`.
3. If state exists and `ping` succeeds, report `running`.
4. If state exists but `ping` fails, report `stale`.
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
- `1` for `stale`
- `2` for usage errors

### `service stop`

Behavior:

1. Load the state file.
2. If no state file exists, report that the service is already stopped and return success.
3. Send JSON-RPC `shutdown` to the stored endpoint.
4. Poll until `ping` fails or a short timeout expires.
5. Remove the state file once the endpoint is no longer reachable.
6. If shutdown cannot be delivered, report `stale` and leave removal to a later cleanup path.

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

The state file is advisory. The source of truth is the IPC `ping` result.

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
- Unreachable endpoint with a state file is `stale`.
- Failure to write state after successful startup should stop the child through `shutdown` when possible, then return an error.
- Malformed state file is treated as `stale` and can be replaced by `service start`.
- Existing `ipc` behavior remains unchanged for manually started daemon processes.

## Testing Strategy

### Unit Tests

Add tests for:

- State file round trip
- Missing state file reports stopped
- Malformed state file reports stale or readable error state
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

- Restrict Named Pipe and Unix Socket permissions where supported.
- Add an optional per-user token or peer-credential check.
- Define a separate safe read-only API profile for AI clients.

## Open Source Fit

The MVP keeps OS-specific complexity small and auditable. Contributors can run and test the service lifecycle without admin setup, which lowers onboarding cost and makes cross-platform CI easier later.
