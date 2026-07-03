pub mod service;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_file_search_indexer::FileIndexStore;
use ai_file_search_protocol::{Request, Response};
use serde_json::json;
use service::{
    DEFAULT_ENDPOINT, ServiceState, ServiceStatus, default_state_path, read_state, remove_state,
    render_status_json, render_status_text, write_state,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::sleep;

const USAGE: &str = "usage: ai-file-search-daemon <stdio <index-file>|handle <index-file> <json-line>|ipc <index-file> <endpoint>|ipc-request <endpoint> [json-line]|service start <index-file> [--endpoint <name>]|service status [--json]|service stop>\n";

pub struct CliResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

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
pub fn run<I, S>(args: I) -> CliResult
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    run_with_state_path(args, default_state_path())
}

#[must_use]
pub fn run_with_state_path<I, S>(args: I, state_path: impl Into<PathBuf>) -> CliResult
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let args = args
        .into_iter()
        .map(Into::into)
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let state_path = state_path.into();

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

    runtime.block_on(run_async_with_state_path(args, state_path))
}

pub async fn run_async(args: Vec<String>) -> CliResult {
    run_async_with_state_path(args, default_state_path()).await
}

pub async fn run_async_with_state_path(args: Vec<String>, state_path: PathBuf) -> CliResult {
    match args.first().map(String::as_str) {
        Some("service") => service_command(&args[1..], &state_path).await,
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

async fn service_command(args: &[String], state_path: &Path) -> CliResult {
    match args.first().map(String::as_str) {
        Some("status") if args.len() == 1 => service_status(false, state_path).await,
        Some("status") if args.len() == 2 && args[1] == "--json" => {
            service_status(true, state_path).await
        }
        Some("stop") if args.len() == 1 => service_stop(state_path).await,
        Some("start") => service_start(&args[1..], state_path).await,
        _ => usage_error(),
    }
}

async fn service_status(json_output: bool, state_path: &Path) -> CliResult {
    let status = match read_state(state_path) {
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

    let exit_code = i32::from(matches!(&status, ServiceStatus::Stale(_)));
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

async fn service_stop(state_path: &Path) -> CliResult {
    let Some(state) = (match read_state(state_path) {
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
            if let Err(error) = remove_state(state_path) {
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

async fn service_start(args: &[String], state_path: &Path) -> CliResult {
    let (index_path, endpoint) = match parse_service_start_args(args) {
        Ok(parsed) => parsed,
        Err(result) => return result,
    };
    let index_path = match resolve_index_path(index_path) {
        Ok(path) => path,
        Err(result) => return result,
    };

    if let Some(state) = running_state(state_path).await {
        return service_running_result(&state);
    }

    let child = match spawn_service_child(&index_path, &endpoint) {
        Ok(child) => child,
        Err(result) => return result,
    };

    wait_for_started_service(&endpoint, &index_path, state_path, child.id()).await
}

fn parse_service_start_args(args: &[String]) -> Result<(&str, String), CliResult> {
    let Some(index_path) = args.first() else {
        return Err(usage_error());
    };

    match args {
        [_index] => Ok((index_path, DEFAULT_ENDPOINT.to_owned())),
        [_index, flag, value] if flag == "--endpoint" => Ok((index_path, value.clone())),
        _ => Err(usage_error()),
    }
}

fn resolve_index_path(index_path: &str) -> Result<PathBuf, CliResult> {
    std::fs::canonicalize(index_path).map_err(|error| CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("index path resolve failed: {error}\n"),
    })
}

async fn running_state(state_path: &Path) -> Option<ServiceState> {
    let state = read_state(state_path).ok().flatten()?;
    if ping_endpoint(&state.endpoint).await {
        Some(state)
    } else {
        None
    }
}

fn service_running_result(state: &ServiceState) -> CliResult {
    CliResult {
        exit_code: 0,
        stdout: format!(
            "running endpoint={} pid={} index={}\n",
            state.endpoint,
            state.pid,
            state.index_path.display()
        ),
        stderr: String::new(),
    }
}

fn spawn_service_child(index_path: &Path, endpoint: &str) -> Result<Child, CliResult> {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => {
            return Err(CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("current exe resolve failed: {error}\n"),
            });
        }
    };

    let mut command = Command::new(exe);
    command
        .arg("service-run")
        .arg(index_path)
        .arg(endpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }

    command.spawn().map_err(|error| CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("service spawn failed: {error}\n"),
    })
}

async fn wait_for_started_service(
    endpoint: &str,
    index_path: &Path,
    state_path: &Path,
    pid: u32,
) -> CliResult {
    for _ in 0..40 {
        if ping_endpoint(endpoint).await {
            let state = ServiceState {
                endpoint: endpoint.to_owned(),
                pid,
                index_path: index_path.to_path_buf(),
                started_unix_seconds: now_unix_seconds(),
            };
            if let Err(error) = write_state(state_path, &state) {
                let _ =
                    send_ipc_request(endpoint, r#"{"id":1,"method":"shutdown","params":{}}"#).await;
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

async fn ping_endpoint(endpoint: &str) -> bool {
    match send_ipc_request(endpoint, r#"{"id":1,"method":"ping","params":{}}"#).await {
        Ok(response) => response.contains(r#""status":"ok""#),
        Err(_) => false,
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

fn stats(index_path: &Path, id: u64) -> Response {
    let store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => return Response::error(id, format!("index open failed: {error}")),
    };

    Response::success(
        id,
        json!({
            "files": store.file_count(),
            "total_bytes": store.total_size_bytes(),
        }),
    )
}

fn search(index_path: &Path, request: &Request) -> Response {
    let Some(query) = request.params.get("query").and_then(|query| query.as_str()) else {
        return Response::error(request.id, "missing string param: query");
    };
    let limit = request
        .params
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .and_then(|limit| usize::try_from(limit).ok())
        .unwrap_or(20);

    let store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => return Response::error(request.id, format!("index open failed: {error}")),
    };
    let files = store
        .search_by_name(query)
        .into_iter()
        .take(limit)
        .map(|file| {
            json!({
                "path": file.relative_path.as_normalized(),
                "size_bytes": file.size_bytes,
                "modified_unix_seconds": file.modified_unix_seconds,
            })
        })
        .collect::<Vec<_>>();

    Response::success(request.id, json!({ "files": files }))
}

/// Handles newline-delimited JSON-RPC requests on a bidirectional async stream.
///
/// # Errors
///
/// Returns an I/O error when the stream cannot be read from or written to.
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

/// Sends one newline-delimited JSON-RPC request on a bidirectional async stream.
///
/// # Errors
///
/// Returns an I/O error when the stream cannot be written to or read from.
pub async fn send_json_request<S>(stream: S, request: &str) -> io::Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = stream;
    stream.write_all(request.as_bytes()).await?;
    if !request.ends_with('\n') {
        stream.write_all(b"\n").await?;
    }
    stream.flush().await?;

    let mut stream = BufReader::new(stream);
    let mut response = String::new();
    stream.read_line(&mut response).await?;

    Ok(response)
}

#[cfg(windows)]
fn pipe_name(endpoint: &str) -> String {
    const PIPE_PREFIX: &str = r"\\.\pipe\";
    if endpoint.starts_with(PIPE_PREFIX) {
        endpoint.to_owned()
    } else {
        format!("{PIPE_PREFIX}{endpoint}")
    }
}

/// Serves JSON-RPC requests over the platform IPC transport.
///
/// # Errors
///
/// Returns an I/O error when the endpoint cannot be created or a client stream
/// cannot be handled.
#[cfg(windows)]
pub async fn serve_ipc(index_path: &Path, endpoint: &str) -> io::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let endpoint = pipe_name(endpoint);
    loop {
        let server = ServerOptions::new().create(&endpoint)?;
        server.connect().await?;
        let status = handle_json_stream(index_path, server).await?;
        if status == StreamStatus::ShutdownRequested {
            return Ok(());
        }
    }
}

/// Sends one JSON-RPC request over the platform IPC transport.
///
/// # Errors
///
/// Returns an I/O error when the endpoint cannot be opened or used.
#[cfg(windows)]
pub async fn send_ipc_request(endpoint: &str, request: &str) -> io::Result<String> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let client = ClientOptions::new().open(pipe_name(endpoint))?;
    send_json_request(client, request).await
}

/// Serves JSON-RPC requests over the platform IPC transport.
///
/// # Errors
///
/// Returns an I/O error when the endpoint cannot be created or a client stream
/// cannot be handled.
#[cfg(unix)]
pub async fn serve_ipc(index_path: &Path, endpoint: &str) -> io::Result<()> {
    use tokio::net::UnixListener;

    let _ = std::fs::remove_file(endpoint);
    let listener = UnixListener::bind(endpoint)?;
    loop {
        let (stream, _) = listener.accept().await?;
        let status = handle_json_stream(index_path, stream).await?;
        if status == StreamStatus::ShutdownRequested {
            return Ok(());
        }
    }
}

/// Sends one JSON-RPC request over the platform IPC transport.
///
/// # Errors
///
/// Returns an I/O error when the endpoint cannot be opened or used.
#[cfg(unix)]
pub async fn send_ipc_request(endpoint: &str, request: &str) -> io::Result<String> {
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(endpoint).await?;
    send_json_request(stream, request).await
}
