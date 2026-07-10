pub mod service;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_file_search_indexer::{FileIndexStore, IndexedFile, RefreshSummary, ScanOptions, Scanner};
use ai_file_search_protocol::{Request, Response};
use serde_json::json;
use service::{
    DEFAULT_ENDPOINT, ServiceState, ServiceStatus, default_state_path, read_state, remove_state,
    render_status_json, render_status_text, write_state,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::sleep;

const USAGE: &str = "usage: ai-file-search-daemon <stdio <index-file>|handle <index-file> <json-line>|ipc <index-file> <endpoint>|ipc-request <endpoint> [json-line]|service start <index-file> [--endpoint <name>] [--auto-refresh-seconds <seconds>]|service status [--json]|service stop>\n";
const MIN_AUTO_REFRESH_SECONDS: u64 = 30;
const MAX_AUTO_REFRESH_SECONDS: u64 = 86_400;
const SERVICE_STOP_ATTEMPTS: usize = 20;

#[must_use]
pub fn parse_auto_refresh_seconds(value: &str) -> Option<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|seconds| (MIN_AUTO_REFRESH_SECONDS..=MAX_AUTO_REFRESH_SECONDS).contains(seconds))
}

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

    for _ in 0..SERVICE_STOP_ATTEMPTS {
        match send_ipc_request(
            &state.endpoint,
            r#"{"id":1,"method":"shutdown","params":{}}"#,
        )
        .await
        {
            Ok(response) if response.contains(r#""status":"shutting_down""#) => {
                return wait_for_service_shutdown(&state, state_path).await;
            }
            Err(error) if endpoint_is_unavailable(&error) => {
                return remove_service_state(state_path);
            }
            _ => sleep(Duration::from_millis(50)).await,
        }
    }

    CliResult {
        exit_code: 1,
        stdout: render_status_text(&ServiceStatus::Stale(state)),
        stderr: String::new(),
    }
}

async fn wait_for_service_shutdown(state: &ServiceState, state_path: &Path) -> CliResult {
    for _ in 0..SERVICE_STOP_ATTEMPTS {
        match send_ipc_request(&state.endpoint, r#"{"id":1,"method":"ping","params":{}}"#).await {
            Err(error) if endpoint_is_unavailable(&error) => {
                return remove_service_state(state_path);
            }
            _ => sleep(Duration::from_millis(50)).await,
        }
    }

    CliResult {
        exit_code: 1,
        stdout: render_status_text(&ServiceStatus::Stale(state.clone())),
        stderr: String::new(),
    }
}

fn endpoint_is_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) || cfg!(windows) && error.raw_os_error() == Some(2)
}

fn remove_service_state(state_path: &Path) -> CliResult {
    if let Err(error) = remove_state(state_path) {
        return CliResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("service state remove failed: {error}\n"),
        };
    }

    CliResult {
        exit_code: 0,
        stdout: "stopped\n".to_owned(),
        stderr: String::new(),
    }
}

async fn service_start(args: &[String], state_path: &Path) -> CliResult {
    let parsed = match parse_service_start_args(args) {
        Ok(parsed) => parsed,
        Err(result) => return result,
    };
    let index_path = match resolve_index_path(parsed.index_path) {
        Ok(path) => path,
        Err(result) => return result,
    };
    if let Err(result) = validate_index_root_metadata(&index_path) {
        return result;
    }

    if let Some(state) = running_state(state_path).await {
        return service_running_result(&state);
    }

    let child =
        match spawn_service_child(&index_path, &parsed.endpoint, parsed.auto_refresh_seconds) {
            Ok(child) => child,
            Err(result) => return result,
        };

    wait_for_started_service(
        &parsed.endpoint,
        &index_path,
        parsed.auto_refresh_seconds,
        state_path,
        child.id(),
    )
    .await
}

struct ServiceStartArgs<'a> {
    index_path: &'a str,
    endpoint: String,
    auto_refresh_seconds: Option<u64>,
}

fn parse_service_start_args(args: &[String]) -> Result<ServiceStartArgs<'_>, CliResult> {
    let Some(index_path) = args.first() else {
        return Err(usage_error());
    };

    let mut endpoint = None;
    let mut auto_refresh_seconds = None;
    let mut arguments = args[1..].iter();
    while let Some(flag) = arguments.next() {
        let Some(value) = arguments.next() else {
            return Err(usage_error());
        };
        match flag.as_str() {
            "--endpoint" if endpoint.is_none() => endpoint = Some(value.clone()),
            "--auto-refresh-seconds" if auto_refresh_seconds.is_none() => {
                let Some(seconds) = parse_auto_refresh_seconds(value) else {
                    return Err(usage_error());
                };
                auto_refresh_seconds = Some(seconds);
            }
            _ => return Err(usage_error()),
        }
    }

    Ok(ServiceStartArgs {
        index_path,
        endpoint: endpoint.unwrap_or_else(|| DEFAULT_ENDPOINT.to_owned()),
        auto_refresh_seconds,
    })
}

fn resolve_index_path(index_path: &str) -> Result<PathBuf, CliResult> {
    std::fs::canonicalize(index_path).map_err(|error| CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("index path resolve failed: {error}\n"),
    })
}

fn validate_index_root_metadata(index_path: &Path) -> Result<(), CliResult> {
    let store = FileIndexStore::open(index_path).map_err(|error| CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("index open failed: {error}\n"),
    })?;

    if store.root_path().is_some() {
        Ok(())
    } else {
        Err(CliResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "index root metadata missing: run ai-file-search index <root> <index-file>\n"
                .to_owned(),
        })
    }
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

fn spawn_service_child(
    index_path: &Path,
    endpoint: &str,
    auto_refresh_seconds: Option<u64>,
) -> Result<Child, CliResult> {
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
    if let Some(seconds) = auto_refresh_seconds {
        command
            .arg("--auto-refresh-seconds")
            .arg(seconds.to_string());
    }

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
    auto_refresh_seconds: Option<u64>,
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
                auto_refresh_seconds,
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

pub async fn service_run(
    index_path: &Path,
    endpoint: &str,
    auto_refresh_seconds: Option<u64>,
) -> i32 {
    let _ = auto_refresh_seconds;
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
        "methods" => HandlerOutcome {
            response: method_catalog(request.id),
            shutdown_requested: false,
        },
        "ping" => HandlerOutcome {
            response: Response::success(request.id, json!({ "status": "ok" })),
            shutdown_requested: false,
        },
        "index_status" => HandlerOutcome {
            response: index_status(index_path, &request),
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
        "refresh" | "reindex" => HandlerOutcome {
            response: refresh(index_path, &request),
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

fn method_catalog(id: u64) -> Response {
    Response::success(
        id,
        json!({
            "protocol": "ai-file-search-json-rpc",
            "version": 1,
            "methods": [
                {
                    "name": "methods",
                    "params": {},
                },
                {
                    "name": "ping",
                    "params": {},
                },
                {
                    "name": "index_status",
                    "params": {
                        "root": "optional string with stored root metadata (if supplied, must match); otherwise required",
                        "exclude_names": "optional string array",
                    },
                },
                {
                    "name": "refresh",
                    "params": {
                        "root": "optional string; must match stored root",
                        "exclude_names": "optional string array",
                    },
                },
                {
                    "name": "reindex",
                    "params": {
                        "root": "optional string; must match stored root",
                        "exclude_names": "optional string array",
                    },
                },
                {
                    "name": "search",
                    "params": {
                        "query": "string",
                        "limit": "optional u64 default 20",
                    },
                },
                {
                    "name": "shutdown",
                    "params": {},
                },
                {
                    "name": "stats",
                    "params": {},
                },
            ],
        }),
    )
}

fn index_status(index_path: &Path, request: &Request) -> Response {
    let options = match scan_options(&request.params) {
        Ok(options) => options,
        Err(message) => return Response::error(request.id, message),
    };

    let store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => return Response::error(request.id, format!("index open failed: {error}")),
    };
    if matches!(
        request.params.get("root"),
        Some(root) if !root.is_string()
    ) {
        return Response::error(request.id, "root must be a string");
    }
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

fn refresh(index_path: &Path, request: &Request) -> Response {
    let options = match scan_options(&request.params) {
        Ok(options) => options,
        Err(message) => return Response::error(request.id, message),
    };

    let mut store = match FileIndexStore::open(index_path) {
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

    let old_files = store.all_files();
    let summary = RefreshSummary::compare(&old_files, &files);
    store.set_root_path(&root);
    store.replace_all(files);
    if let Err(error) = store.save() {
        return Response::error(request.id, format!("index save failed: {error}"));
    }

    Response::success(
        request.id,
        json!({
            "scanned_files": scanned_files,
            "added": summary.added,
            "updated": summary.updated,
            "removed": summary.removed,
            "unchanged": summary.unchanged,
        }),
    )
}

fn index_root(store: &FileIndexStore, params: &serde_json::Value) -> Result<PathBuf, &'static str> {
    let requested_root = params
        .get("root")
        .and_then(|root| root.as_str())
        .map(PathBuf::from);

    match (store.root_path(), requested_root) {
        (Some(stored_root), Some(requested_root)) => {
            if same_root_path(stored_root, &requested_root) {
                Ok(stored_root.to_path_buf())
            } else {
                Err("root does not match stored index root")
            }
        }
        (Some(stored_root), None) => Ok(stored_root.to_path_buf()),
        (None, Some(requested_root)) => Ok(requested_root),
        (None, None) => Err("missing string param: root"),
    }
}

fn same_root_path(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn scan_options(params: &serde_json::Value) -> Result<ScanOptions, &'static str> {
    let Some(excluded_names) = params.get("exclude_names") else {
        return Ok(ScanOptions::default());
    };
    let Some(excluded_names) = excluded_names.as_array() else {
        return Err("exclude_names must be an array of strings");
    };

    excluded_names
        .iter()
        .try_fold(ScanOptions::default(), |options, name| {
            name.as_str()
                .map(|name| options.exclude_name(name.to_owned()))
                .ok_or("exclude_names must be an array of strings")
        })
}

fn scan_files_for_index(
    root: &Path,
    index_path: &Path,
    options: ScanOptions,
) -> io::Result<Vec<IndexedFile>> {
    let scanner = Scanner::new(options);
    let mut files = scanner.scan(root)?;

    if let Some(index_relative_path) = relative_index_path(root, index_path) {
        files.retain(|file| file.relative_path.as_normalized() != index_relative_path);
    }

    Ok(files)
}

fn relative_index_path(root: &Path, index_path: &Path) -> Option<String> {
    let relative_path = match (
        std::fs::canonicalize(root),
        std::fs::canonicalize(index_path),
    ) {
        (Ok(root), Ok(index_path)) => index_path.strip_prefix(root).ok()?.to_path_buf(),
        _ => index_path.strip_prefix(root).ok()?.to_path_buf(),
    };

    Some(
        relative_path
            .components()
            .collect::<PathBuf>()
            .to_string_lossy()
            .replace('\\', "/"),
    )
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

async fn handle_one_json_request<S>(index_path: &Path, stream: S) -> io::Result<StreamStatus>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    if stream.read_line(&mut line).await? == 0 {
        return Ok(StreamStatus::ClientDisconnected);
    }

    let outcome = handle_json_request(index_path, &line);
    stream
        .get_mut()
        .write_all(outcome.response.to_json_line().as_bytes())
        .await?;
    stream.get_mut().flush().await?;

    Ok(if outcome.shutdown_requested {
        StreamStatus::ShutdownRequested
    } else {
        StreamStatus::ClientDisconnected
    })
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
    stream.get_mut().shutdown().await?;

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
        let status = handle_one_json_request(index_path, server).await?;
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
