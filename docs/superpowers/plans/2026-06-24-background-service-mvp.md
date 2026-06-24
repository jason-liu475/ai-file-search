# Background Service MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add application-level background service management to `ai-file-search-daemon` with `service start`, `service status`, and `service stop`.

**Architecture:** Keep service lifecycle management inside the daemon crate and reuse the existing JSON-RPC IPC transport. Add a small service-state module for advisory state-file IO, add `ping` and `shutdown` to the daemon request handler, then expose a testable CLI runner that can spawn a hidden `service-run` child process.

**Tech Stack:** Rust, Tokio, Serde/serde_json, existing daemon/protocol/indexer crates, platform IPC already implemented by `tokio::net::windows::named_pipe` and `tokio::net::UnixListener`.

---

## File Structure

- Modify: `crates/daemon/Cargo.toml`
  - Add `serde = { version = "1", features = ["derive"] }`.
  - Add Tokio features needed for timers and process spawning: `process`, `time`.
- Create: `crates/daemon/src/service.rs`
  - Owns `ServiceState`, service state path resolution, read/write/remove helpers, and status rendering.
- Modify: `crates/daemon/src/lib.rs`
  - Expose `pub mod service`.
  - Add `HandlerOutcome` and shutdown-aware JSON-RPC handling while preserving `handle_json_line`.
  - Add `ping` and `shutdown`.
  - Change stream handling to return a shutdown-aware status.
- Modify: `crates/daemon/src/main.rs`
  - Add a testable `run(args)` entry point.
  - Parse `service start/status/stop` and hidden `service-run`.
  - Spawn background child and manage service state.
- Modify: `crates/daemon/tests/handler_tests.rs`
  - Add `ping` and `shutdown` handler tests.
- Modify: `crates/daemon/tests/transport_tests.rs`
  - Update stream status assertions and add shutdown stream coverage.
- Create: `crates/daemon/tests/service_state_tests.rs`
  - Test state-file round trips and status rendering.
- Create: `crates/daemon/tests/service_cli_tests.rs`
  - Test parser-level service status/stop behavior through `run(args)`.
- Modify: `README.md`
  - Document service commands and clarify remaining service limitations.

---

### Task 1: Service State Module

**Files:**
- Modify: `crates/daemon/Cargo.toml`
- Create: `crates/daemon/src/service.rs`
- Modify: `crates/daemon/src/lib.rs`
- Test: `crates/daemon/tests/service_state_tests.rs`

- [ ] **Step 1: Write failing service state tests**

Create `crates/daemon/tests/service_state_tests.rs`:

```rust
use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_daemon::service::{
    read_state, remove_state, render_status_json, render_status_text, write_state, ServiceState,
    ServiceStatus,
};

#[test]
fn service_state_round_trips_as_json() {
    let fixture = TestDir::new("service_state_round_trips_as_json");
    let state_path = fixture.path().join("service-state.json");
    let index_path = fixture.path().join("index.txt");
    let state = ServiceState {
        endpoint: "aifs-test".to_owned(),
        pid: 42,
        index_path: index_path.clone(),
        started_unix_seconds: 1_782_281_286,
    };

    write_state(&state_path, &state).expect("state should write");

    let loaded = read_state(&state_path).expect("state should read");
    assert_eq!(loaded, Some(state));
}

#[test]
fn missing_state_file_reads_as_none() {
    let fixture = TestDir::new("missing_state_file_reads_as_none");
    let loaded = read_state(&fixture.path().join("missing.json")).expect("missing state is ok");

    assert_eq!(loaded, None);
}

#[test]
fn remove_state_ignores_missing_files() {
    let fixture = TestDir::new("remove_state_ignores_missing_files");
    remove_state(&fixture.path().join("missing.json")).expect("missing removal is ok");
}

#[test]
fn service_status_renders_stopped_json() {
    assert_eq!(render_status_json(&ServiceStatus::Stopped), "{\"status\":\"stopped\"}\n");
}

#[test]
fn service_status_renders_running_text() {
    let state = ServiceState {
        endpoint: "aifs-test".to_owned(),
        pid: 42,
        index_path: PathBuf::from("C:/tmp/index.txt"),
        started_unix_seconds: 1_782_281_286,
    };

    assert_eq!(
        render_status_text(&ServiceStatus::Running(state)),
        "running endpoint=aifs-test pid=42 index=C:/tmp/index.txt\n"
    );
}

#[test]
fn malformed_state_file_returns_invalid_data_error() {
    let fixture = TestDir::new("malformed_state_file_returns_invalid_data_error");
    let state_path = fixture.path().join("service-state.json");
    fs::write(&state_path, "{not json").expect("fixture should write");

    let error = read_state(&state_path).expect_err("malformed state should fail");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("ai-file-search-service-state-{name}-{}", std::process::id()));

        if path.exists() {
            fs::remove_dir_all(&path).expect("old fixture should be removable");
        }
        fs::create_dir_all(&path).expect("fixture directory should be created");

        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).expect("fixture directory should be removed");
        }
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p ai-file-search-daemon service_state -- --nocapture
```

Expected: FAIL because `ai_file_search_daemon::service` does not exist.

- [ ] **Step 3: Implement service state module**

Modify `crates/daemon/Cargo.toml`:

```toml
[dependencies]
ai-file-search-core = { path = "../core" }
ai-file-search-indexer = { path = "../indexer" }
ai-file-search-protocol = { path = "../protocol" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["io-util", "macros", "net", "process", "rt-multi-thread", "time"] }
```

Modify `crates/daemon/src/lib.rs` near the top:

```rust
pub mod service;
```

Create `crates/daemon/src/service.rs`:

```rust
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

pub const DEFAULT_ENDPOINT: &str = "aifs-service";
pub const SERVICE_STATE_ENV: &str = "AIFS_SERVICE_STATE";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceState {
    pub endpoint: String,
    pub pid: u32,
    pub index_path: PathBuf,
    pub started_unix_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceStatus {
    Running(ServiceState),
    Stale(ServiceState),
    Stopped,
}

pub fn default_state_path() -> PathBuf {
    if let Some(path) = std::env::var_os(SERVICE_STATE_ENV) {
        return PathBuf::from(path);
    }

    #[cfg(windows)]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local_app_data)
                .join("ai-file-search")
                .join("service-state.json");
        }
    }

    #[cfg(not(windows))]
    {
        if let Some(xdg_state_home) = std::env::var_os("XDG_STATE_HOME") {
            return PathBuf::from(xdg_state_home)
                .join("ai-file-search")
                .join("service-state.json");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("ai-file-search")
                .join("service-state.json");
        }
    }

    std::env::temp_dir()
        .join("ai-file-search")
        .join("service-state.json")
}

pub fn read_state(path: &Path) -> io::Result<Option<ServiceState>> {
    match fs::read_to_string(path) {
        Ok(contents) => serde_json::from_str(&contents)
            .map(Some)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn write_state(path: &Path, state: &ServiceState) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let contents = serde_json::to_string_pretty(state)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    fs::write(path, format!("{contents}\n"))
}

pub fn remove_state(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[must_use]
pub fn render_status_text(status: &ServiceStatus) -> String {
    match status {
        ServiceStatus::Running(state) => format!(
            "running endpoint={} pid={} index={}\n",
            state.endpoint,
            state.pid,
            state.index_path.display()
        ),
        ServiceStatus::Stale(state) => format!(
            "stale endpoint={} pid={} index={}\n",
            state.endpoint,
            state.pid,
            state.index_path.display()
        ),
        ServiceStatus::Stopped => "stopped\n".to_owned(),
    }
}

#[must_use]
pub fn render_status_json(status: &ServiceStatus) -> String {
    let value = match status {
        ServiceStatus::Running(state) => json!({
            "status": "running",
            "endpoint": state.endpoint,
            "pid": state.pid,
            "index_path": state.index_path,
            "started_unix_seconds": state.started_unix_seconds,
        }),
        ServiceStatus::Stale(state) => json!({
            "status": "stale",
            "endpoint": state.endpoint,
            "pid": state.pid,
            "index_path": state.index_path,
            "started_unix_seconds": state.started_unix_seconds,
        }),
        ServiceStatus::Stopped => json!({ "status": "stopped" }),
    };

    format!("{value}\n")
}
```

- [ ] **Step 4: Run target tests to verify pass**

Run:

```bash
cargo test -p ai-file-search-daemon service_state -- --nocapture
```

Expected: PASS for all service state tests.

- [ ] **Step 5: Commit and push Task 1**

Run:

```bash
cargo fmt --check
cargo test -p ai-file-search-daemon service_state -- --nocapture
git add Cargo.lock crates/daemon/Cargo.toml crates/daemon/src/lib.rs crates/daemon/src/service.rs crates/daemon/tests/service_state_tests.rs
git commit -m "feat: add daemon service state"
git push origin main
```

---

### Task 2: Ping and Shutdown JSON-RPC Lifecycle

**Files:**
- Modify: `crates/daemon/src/lib.rs`
- Modify: `crates/daemon/tests/handler_tests.rs`
- Modify: `crates/daemon/tests/transport_tests.rs`

- [ ] **Step 1: Write failing handler tests**

Append to `crates/daemon/tests/handler_tests.rs`:

```rust
#[test]
fn handler_returns_ping_status() {
    let fixture = TestDir::new("handler_returns_ping_status");
    let response = handle_json_line(
        &fixture.path().join("index.txt"),
        r#"{"id":4,"method":"ping","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":4,\"result\":{\"status\":\"ok\"}}\n"
    );
}

#[test]
fn handler_returns_shutdown_status() {
    let fixture = TestDir::new("handler_returns_shutdown_status");
    let response = handle_json_line(
        &fixture.path().join("index.txt"),
        r#"{"id":5,"method":"shutdown","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":5,\"result\":{\"status\":\"shutting_down\"}}\n"
    );
}
```

- [ ] **Step 2: Write failing shutdown stream test**

Modify the import in `crates/daemon/tests/transport_tests.rs`:

```rust
use ai_file_search_daemon::{handle_json_stream, StreamStatus};
```

Update the existing handler task expectation inside `stream_handler_serves_multiple_json_rpc_lines`:

```rust
let handler = tokio::spawn(async move {
    let status = handle_json_stream(&handler_index_path, server)
        .await
        .expect("stream handler should finish cleanly");
    assert_eq!(status, StreamStatus::ClientDisconnected);
});
```

Append this test:

```rust
#[tokio::test]
async fn stream_handler_stops_after_shutdown_request() {
    let fixture = TestDir::new("stream_handler_stops_after_shutdown_request");
    let index_path = fixture.path().join("index.txt");
    save_index(
        &index_path,
        vec![indexed_file("Documents/report.pdf", 6, 1_700_000_000)],
    );

    let (client, server) = tokio::io::duplex(4096);
    let handler_index_path = index_path.clone();
    let handler = tokio::spawn(async move {
        handle_json_stream(&handler_index_path, server)
            .await
            .expect("stream handler should finish cleanly")
    });

    let mut client = BufReader::new(client);
    client
        .get_mut()
        .write_all(b"{\"id\":9,\"method\":\"shutdown\",\"params\":{}}\n")
        .await
        .expect("shutdown request should write");

    let mut response = String::new();
    client
        .read_line(&mut response)
        .await
        .expect("shutdown response should read");

    assert_eq!(
        response,
        "{\"id\":9,\"result\":{\"status\":\"shutting_down\"}}\n"
    );
    assert_eq!(
        handler.await.expect("handler task should join"),
        StreamStatus::ShutdownRequested
    );
}
```

- [ ] **Step 3: Run tests to verify failure**

Run:

```bash
cargo test -p ai-file-search-daemon handler_returns_ping_status handler_returns_shutdown_status stream_handler_stops_after_shutdown_request -- --nocapture
```

Expected: FAIL because `ping`, `shutdown`, and `StreamStatus` do not exist yet.

- [ ] **Step 4: Implement shutdown-aware handler**

Modify `crates/daemon/src/lib.rs`:

```rust
pub mod service;

use std::io;
use std::path::Path;

use ai_file_search_indexer::FileIndexStore;
use ai_file_search_protocol::{Request, Response};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StreamStatus {
    ClientDisconnected,
    ShutdownRequested,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandlerOutcome {
    pub response: Response,
    pub shutdown_requested: bool,
}

#[must_use]
pub fn handle_json_line(index_path: &Path, line: &str) -> Response {
    handle_json_request(index_path, line).response
}

#[must_use]
pub fn handle_json_request(index_path: &Path, line: &str) -> HandlerOutcome {
    let request = match Request::from_json_line(line) {
        Ok(request) => request,
        Err(error) => {
            return HandlerOutcome {
                response: Response::error(0, format!("invalid request: {error}")),
                shutdown_requested: false,
            };
        }
    };

    match request.method.as_str() {
        "ping" => HandlerOutcome {
            response: Response::success(request.id, json!({ "status": "ok" })),
            shutdown_requested: false,
        },
        "shutdown" => HandlerOutcome {
            response: Response::success(request.id, json!({ "status": "shutting_down" })),
            shutdown_requested: true,
        },
        "stats" => HandlerOutcome {
            response: stats(index_path, request.id),
            shutdown_requested: false,
        },
        "search" => HandlerOutcome {
            response: search(index_path, &request),
            shutdown_requested: false,
        },
        method => HandlerOutcome {
            response: Response::error(request.id, format!("unknown method: {method}")),
            shutdown_requested: false,
        },
    }
}
```

Update `handle_json_stream`:

```rust
pub async fn handle_json_stream<S>(index_path: &Path, stream: S) -> io::Result<StreamStatus>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = BufReader::new(stream);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = stream.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Ok(StreamStatus::ClientDisconnected);
        }

        let outcome = handle_json_request(index_path, &line);
        stream
            .get_mut()
            .write_all(outcome.response.to_json_line().as_bytes())
            .await?;
        stream.get_mut().flush().await?;

        if outcome.shutdown_requested {
            return Ok(StreamStatus::ShutdownRequested);
        }
    }
}
```

Update both `serve_ipc` loops so they stop on shutdown:

```rust
let status = handle_json_stream(index_path, server).await?;
if status == StreamStatus::ShutdownRequested {
    return Ok(());
}
```

For Unix, use the same pattern after `listener.accept()`.

- [ ] **Step 5: Run target tests to verify pass**

Run:

```bash
cargo test -p ai-file-search-daemon handler_returns_ping_status handler_returns_shutdown_status stream_handler -- --nocapture
```

Expected: PASS.

- [ ] **Step 6: Commit and push Task 2**

Run:

```bash
cargo fmt --check
cargo test -p ai-file-search-daemon handler_returns_ping_status handler_returns_shutdown_status stream_handler -- --nocapture
git add crates/daemon/src/lib.rs crates/daemon/tests/handler_tests.rs crates/daemon/tests/transport_tests.rs
git commit -m "feat: add daemon ping and shutdown"
git push origin main
```

---

### Task 3: Service CLI Commands

**Files:**
- Modify: `crates/daemon/src/main.rs`
- Test: `crates/daemon/tests/service_cli_tests.rs`

- [ ] **Step 1: Write failing service CLI tests**

Create `crates/daemon/tests/service_cli_tests.rs`:

```rust
use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_daemon::service::{write_state, ServiceState, SERVICE_STATE_ENV};

#[test]
fn service_status_json_reports_stopped_without_state_file() {
    let fixture = TestDir::new("service_status_json_reports_stopped_without_state_file");
    let state_path = fixture.path().join("service-state.json");
    let result = run_with_state(&state_path, ["service", "status", "--json"]);

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "{\"status\":\"stopped\"}\n");
    assert_eq!(result.stderr, "");
}

#[test]
fn service_stop_succeeds_without_state_file() {
    let fixture = TestDir::new("service_stop_succeeds_without_state_file");
    let state_path = fixture.path().join("service-state.json");
    let result = run_with_state(&state_path, ["service", "stop"]);

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "stopped\n");
    assert_eq!(result.stderr, "");
}

#[test]
fn service_status_json_reports_stale_when_state_endpoint_is_unreachable() {
    let fixture = TestDir::new("service_status_json_reports_stale_when_state_endpoint_is_unreachable");
    let state_path = fixture.path().join("service-state.json");
    write_state(
        &state_path,
        &ServiceState {
            endpoint: "aifs-unreachable-service-cli-test".to_owned(),
            pid: 99,
            index_path: fixture.path().join("index.txt"),
            started_unix_seconds: 1_782_281_286,
        },
    )
    .expect("state should write");

    let result = run_with_state(&state_path, ["service", "status", "--json"]);

    assert_eq!(result.exit_code, 1);
    assert!(result.stdout.contains("\"status\":\"stale\""));
    assert!(result.stdout.contains("\"endpoint\":\"aifs-unreachable-service-cli-test\""));
    assert_eq!(result.stderr, "");
}

#[test]
fn service_start_requires_index_file() {
    let fixture = TestDir::new("service_start_requires_index_file");
    let state_path = fixture.path().join("service-state.json");
    let result = run_with_state(&state_path, ["service", "start"]);

    assert_eq!(result.exit_code, 2);
    assert_eq!(result.stdout, "");
    assert!(result.stderr.starts_with("usage: ai-file-search-daemon "));
}

fn run_with_state<const N: usize>(
    state_path: &Path,
    args: [&str; N],
) -> ai_file_search_daemon::CliResult {
    let old = std::env::var_os(SERVICE_STATE_ENV);
    std::env::set_var(SERVICE_STATE_ENV, state_path);
    let result = ai_file_search_daemon::run(args);
    match old {
        Some(value) => std::env::set_var(SERVICE_STATE_ENV, value),
        None => std::env::remove_var(SERVICE_STATE_ENV),
    }
    result
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("ai-file-search-service-cli-{name}-{}", std::process::id()));

        if path.exists() {
            fs::remove_dir_all(&path).expect("old fixture should be removable");
        }
        fs::create_dir_all(&path).expect("fixture directory should be created");

        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).expect("fixture directory should be removed");
        }
    }
}
```

- [ ] **Step 2: Run tests to verify failure**

Run:

```bash
cargo test -p ai-file-search-daemon service_ -- --nocapture
```

Expected: FAIL because `CliResult`, `run`, and service command parsing do not exist.

- [ ] **Step 3: Move CLI behavior into library**

Move daemon CLI behavior from `crates/daemon/src/main.rs` into `crates/daemon/src/lib.rs` so integration tests can call it.

Add to `crates/daemon/src/lib.rs`:

```rust
use std::ffi::OsString;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use service::{
    default_state_path, read_state, remove_state, render_status_json, render_status_text,
    write_state, ServiceState, ServiceStatus, DEFAULT_ENDPOINT,
};
use tokio::time::sleep;

pub struct CliResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

const USAGE: &str = "usage: ai-file-search-daemon <stdio <index-file>|handle <index-file> <json-line>|ipc <index-file> <endpoint>|ipc-request <endpoint> [json-line]|service start <index-file> [--endpoint <name>]|service status [--json]|service stop>\n";

#[must_use]
pub fn run<I, S>(args: I) -> CliResult
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let args = args
        .into_iter()
        .map(Into::into)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("runtime init failed: {error}\n"),
            };
        }
    };

    runtime.block_on(run_async(args))
}

async fn run_async(args: Vec<String>) -> CliResult {
    match args.first().map(String::as_str) {
        Some("service") => service_command(&args[1..]).await,
        Some("handle") if args.len() == 3 => {
            let response = handle_json_line(Path::new(&args[1]), &args[2]);
            CliResult {
                exit_code: 0,
                stdout: response.to_json_line(),
                stderr: String::new(),
            }
        }
        _ => usage_error(),
    }
}

fn usage_error() -> CliResult {
    CliResult {
        exit_code: 2,
        stdout: String::new(),
        stderr: USAGE.to_owned(),
    }
}
```

Keep the existing interactive `stdio`, `ipc`, and `ipc-request` code in `main.rs` initially, then delegate service and handle parser behavior to the library runner. This keeps the first service CLI tests focused and avoids rewriting stdin/stdout plumbing in one step.

- [ ] **Step 4: Implement status and stop**

Add to `crates/daemon/src/lib.rs`:

```rust
async fn service_command(args: &[String]) -> CliResult {
    match args.first().map(String::as_str) {
        Some("status") if args.len() == 1 => service_status(false).await,
        Some("status") if args.len() == 2 && args[1] == "--json" => service_status(true).await,
        Some("stop") if args.len() == 1 => service_stop().await,
        Some("start") => service_start(&args[1..]).await,
        _ => usage_error(),
    }
}

async fn service_status(json_output: bool) -> CliResult {
    let state_path = default_state_path();
    let status = match read_state(&state_path) {
        Ok(Some(state)) => {
            if ping_endpoint(&state.endpoint).await {
                ServiceStatus::Running(state)
            } else {
                ServiceStatus::Stale(state)
            }
        }
        Ok(None) => ServiceStatus::Stopped,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => ServiceStatus::Stopped,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("service state read failed: {error}\n"),
            };
        }
    };

    let exit_code = if matches!(status, ServiceStatus::Stale(_)) { 1 } else { 0 };
    let stdout = if json_output {
        render_status_json(&status)
    } else {
        render_status_text(&status)
    };

    CliResult {
        exit_code,
        stdout,
        stderr: String::new(),
    }
}

async fn service_stop() -> CliResult {
    let state_path = default_state_path();
    let Some(state) = (match read_state(&state_path) {
        Ok(state) => state,
        Err(error) if error.kind() == io::ErrorKind::InvalidData => None,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("service state read failed: {error}\n"),
            };
        }
    }) else {
        return CliResult {
            exit_code: 0,
            stdout: "stopped\n".to_owned(),
            stderr: String::new(),
        };
    };

    let _ = send_ipc_request(
        &state.endpoint,
        r#"{"id":1,"method":"shutdown","params":{}}"#,
    )
    .await;

    for _ in 0..20 {
        if !ping_endpoint(&state.endpoint).await {
            if let Err(error) = remove_state(&state_path) {
                return CliResult {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: format!("service state remove failed: {error}\n"),
                };
            }
            return CliResult {
                exit_code: 0,
                stdout: "stopped\n".to_owned(),
                stderr: String::new(),
            };
        }
        sleep(Duration::from_millis(50)).await;
    }

    CliResult {
        exit_code: 1,
        stdout: render_status_text(&ServiceStatus::Stale(state)),
        stderr: String::new(),
    }
}

async fn ping_endpoint(endpoint: &str) -> bool {
    match send_ipc_request(endpoint, r#"{"id":1,"method":"ping","params":{}}"#).await {
        Ok(response) => response.contains(r#""status":"ok""#),
        Err(_) => false,
    }
}
```

- [ ] **Step 5: Implement start and service-run**

Add to `crates/daemon/src/lib.rs`:

```rust
async fn service_start(args: &[String]) -> CliResult {
    let Some(index_path) = args.first() else {
        return usage_error();
    };

    let endpoint = match args {
        [_index] => DEFAULT_ENDPOINT.to_owned(),
        [_index, flag, value] if flag == "--endpoint" => value.clone(),
        _ => return usage_error(),
    };

    let index_path = match std::fs::canonicalize(index_path) {
        Ok(path) => path,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("index path resolve failed: {error}\n"),
            };
        }
    };

    let state_path = default_state_path();
    if let Ok(Some(state)) = read_state(&state_path) {
        if ping_endpoint(&state.endpoint).await {
            return CliResult {
                exit_code: 0,
                stdout: format!(
                    "running endpoint={} pid={} index={}\n",
                    state.endpoint,
                    state.pid,
                    state.index_path.display()
                ),
                stderr: String::new(),
            };
        }
    }

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("current exe resolve failed: {error}\n"),
            };
        }
    };

    let mut command = Command::new(exe);
    command
        .arg("service-run")
        .arg(&index_path)
        .arg(&endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("service spawn failed: {error}\n"),
            };
        }
    };

    for _ in 0..40 {
        if ping_endpoint(&endpoint).await {
            let state = ServiceState {
                endpoint: endpoint.clone(),
                pid: child.id(),
                index_path: index_path.clone(),
                started_unix_seconds: now_unix_seconds(),
            };
            if let Err(error) = write_state(&state_path, &state) {
                let _ = send_ipc_request(&endpoint, r#"{"id":1,"method":"shutdown","params":{}}"#).await;
                return CliResult {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: format!("service state write failed: {error}\n"),
                };
            }
            return CliResult {
                exit_code: 0,
                stdout: format!(
                    "started endpoint={} pid={} index={}\n",
                    state.endpoint,
                    state.pid,
                    state.index_path.display()
                ),
                stderr: String::new(),
            };
        }
        sleep(Duration::from_millis(50)).await;
    }

    CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "service did not become healthy\n".to_owned(),
    }
}

pub async fn service_run(index_path: &Path, endpoint: &str) -> i32 {
    match serve_ipc(index_path, endpoint).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("service run failed: {error}");
            1
        }
    }
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
```

Modify `crates/daemon/src/main.rs` so service commands use the library runner and `service-run` stays async:

```rust
use std::io::{self, BufRead, Read, Write};
use std::path::Path;

use ai_file_search_daemon::{handle_json_line, run, service_run};

#[tokio::main]
async fn main() {
    let exit_code = async_main().await;
    std::process::exit(exit_code);
}

async fn async_main() -> i32 {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("stdio") if args.len() == 2 => serve_stdio(&args[1]),
        Some("ipc") if args.len() == 3 => serve_ipc(&args[1], &args[2]).await,
        Some("ipc-request") if args.len() == 2 => ipc_request_stdin(&args[1]).await,
        Some("ipc-request") if args.len() == 3 => ipc_request(&args[1], &args[2]).await,
        Some("service-run") if args.len() == 3 => service_run(Path::new(&args[1]), &args[2]).await,
        Some("handle" | "service") => {
            let result = run(args);
            print!("{}", result.stdout);
            eprint!("{}", result.stderr);
            result.exit_code
        }
        _ => {
            let result = run(args);
            print!("{}", result.stdout);
            eprint!("{}", result.stderr);
            result.exit_code
        }
    }
}
```

Keep the existing `serve_stdio`, `serve_ipc`, `ipc_request`, and `ipc_request_stdin` helper functions below this entry point.

- [ ] **Step 6: Run service CLI tests**

Run:

```bash
cargo test -p ai-file-search-daemon service_ -- --nocapture
```

Expected: PASS.

- [ ] **Step 7: Commit and push Task 3**

Run:

```bash
cargo fmt --check
cargo test -p ai-file-search-daemon service_ -- --nocapture
git add crates/daemon/src/lib.rs crates/daemon/src/main.rs crates/daemon/tests/service_cli_tests.rs
git commit -m "feat: add daemon service commands"
git push origin main
```

---

### Task 4: Documentation, Smoke Test, and Full Verification

**Files:**
- Modify: `README.md`
- Modify: `docs/superpowers/plans/2026-06-24-background-service-mvp.md`

- [ ] **Step 1: Update README**

Add service commands to the CLI Commands block:

```text
ai-file-search-daemon service start <index-file> [--endpoint <name>]
ai-file-search-daemon service status [--json]
ai-file-search-daemon service stop
```

Add a quick usage example after the IPC daemon example:

```bash
cargo run -p ai-file-search-daemon -- service start ./tmp-index.txt
cargo run -p ai-file-search-daemon -- service status --json
echo '{"id":1,"method":"stats","params":{}}' | cargo run -p ai-file-search-daemon -- ipc-request aifs-service
cargo run -p ai-file-search-daemon -- service stop
```

Update current behavior bullets:

```text
- `ai-file-search-daemon service start/status/stop` manages a user-level background daemon over the platform IPC transport.
```

Update MVP limitations:

```text
- OS service installation, start-on-login, authentication, and multi-user access controls are not implemented yet.
```

- [ ] **Step 2: Run full automated verification**

Run:

```bash
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: all commands exit `0`.

- [ ] **Step 3: Run service smoke test**

Run on Windows PowerShell:

```powershell
$fixture = 'C:\tmp\ai-file-search-service-fixture'
$index = 'C:\tmp\ai-file-search-service-index.txt'
$state = 'C:\tmp\ai-file-search-service-state.json'
Remove-Item -LiteralPath $fixture,$index,$state -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path "$fixture\Documents" | Out-Null
Set-Content -LiteralPath "$fixture\Documents\report.pdf" -Value 'report' -NoNewline
cargo run -p ai-file-search-cli -- index $fixture $index
$env:AIFS_SERVICE_STATE = $state
cargo run -p ai-file-search-daemon -- service start $index --endpoint aifs-service-smoke
cargo run -p ai-file-search-daemon -- service status --json
'{"id":1,"method":"stats","params":{}}' | cargo run -p ai-file-search-daemon -- ipc-request aifs-service-smoke
cargo run -p ai-file-search-daemon -- service stop
cargo run -p ai-file-search-daemon -- service status --json
Remove-Item Env:\AIFS_SERVICE_STATE
```

Expected:

```text
indexed 1 files
started endpoint=aifs-service-smoke ...
{"status":"running",...}
{"id":1,"result":{"files":1,"total_bytes":6}}
stopped
{"status":"stopped"}
```

If the smoke test leaves a service process running, stop only the matching endpoint process:

```powershell
Get-CimInstance Win32_Process |
  Where-Object { $_.CommandLine -like '*service-run*' -and $_.CommandLine -like '*aifs-service-smoke*' } |
  ForEach-Object { Stop-Process -Id $_.ProcessId -Force }
```

- [ ] **Step 4: Mark this plan complete**

Update each completed checkbox in this file from `[ ]` to `[x]` as implementation progresses.

- [ ] **Step 5: Commit and push Task 4**

Run:

```bash
git add README.md docs/superpowers/plans/2026-06-24-background-service-mvp.md
git commit -m "docs: document daemon service commands"
git push origin main
```

- [ ] **Step 6: Final clean status check**

Run:

```bash
git status --short --branch
git log --oneline -5
```

Expected:

```text
## main...origin/main
```

The latest commits should include:

```text
docs: document daemon service commands
feat: add daemon service commands
feat: add daemon ping and shutdown
feat: add daemon service state
docs: add background service MVP design
```
