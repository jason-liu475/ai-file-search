mod bounded_io;
pub mod service;

#[cfg(unix)]
mod unix_endpoint;

#[cfg(test)]
mod managed_lifecycle_tests;
#[cfg(test)]
mod refresh_tests;

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_file_search_indexer::{
    FileIndexStore, FileIndexWriter, IndexWriterGuard, IndexedFile, RefreshSummary, ScanOptions,
    Scanner,
};
use ai_file_search_protocol::{Request, Response};
use serde_json::json;
use service::{
    DEFAULT_ENDPOINT, SERVICE_STATE_ENV, ServiceCoordination, ServiceInstanceGuard, ServiceState,
    ServiceStatus, default_state_path, read_state, remove_state_if_matches, render_status_json,
    render_status_text, write_state,
};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::time::{Instant, sleep, timeout, timeout_at};

const USAGE: &str = "usage: ai-file-search-daemon <stdio <index-file>|handle <index-file> <json-line>|ipc <index-file> <endpoint>|ipc-request <endpoint> [json-line]|service start <index-file> [--endpoint <name>] [--auto-refresh-seconds <seconds>]|service status [--json]|service stop>\n";
const MIN_AUTO_REFRESH_SECONDS: u64 = 30;
const MAX_AUTO_REFRESH_SECONDS: u64 = 86_400;

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
    let status = inspect_service(state_path).await.unwrap_or_else(|error| {
        ServiceStatus::Error(format!("service state/ownership check failed: {error}"))
    });
    service_status_result(&status, json_output)
}

fn service_status_result(status: &ServiceStatus, json_output: bool) -> CliResult {
    CliResult {
        exit_code: i32::from(!matches!(
            status,
            ServiceStatus::Running(_) | ServiceStatus::Stopped
        )),
        stdout: if json_output {
            render_status_json(status)
        } else {
            render_status_text(status)
        },
        stderr: match status {
            ServiceStatus::Error(reason) => format!("{reason}\n"),
            _ => String::new(),
        },
    }
}

async fn inspect_service(state_path: &Path) -> io::Result<ServiceStatus> {
    let state = read_state(state_path)?;
    let starting = ServiceCoordination::startup_active(state_path)?;
    let active = ServiceCoordination::instance_active(state_path)?;
    let Some(state) = state else {
        return Ok(if starting {
            ServiceStatus::Starting
        } else if active {
            ServiceStatus::Unresponsive(None)
        } else {
            ServiceStatus::Stopped
        });
    };
    match ping_service(&state.endpoint).await {
        Ok(identity) if active && identity_matches(&identity, &state) => {
            Ok(ServiceStatus::Running(state))
        }
        Err(error) if !active && !starting && endpoint_is_unavailable(&error) => {
            Ok(ServiceStatus::Stale(state))
        }
        _ => Ok(ServiceStatus::Unresponsive(Some(state))),
    }
}

async fn service_stop(state_path: &Path) -> CliResult {
    if let Err(error) = read_state(state_path) {
        return lifecycle_error("service state read failed", error);
    }
    let coordination = match ServiceCoordination::acquire(state_path) {
        Ok(guard) => guard,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            return service_status_result(&ServiceStatus::Starting, false);
        }
        Err(error) => return lifecycle_error("service coordination failed", error),
    };
    let state_path = coordination.state_path();
    let state = match read_state(state_path) {
        Ok(state) => state,
        Err(error) => return lifecycle_error("service state read failed", error),
    };
    let active = match ServiceCoordination::instance_active(state_path) {
        Ok(active) => active,
        Err(error) => return lifecycle_error("service ownership check failed", error),
    };
    let Some(state) = state else {
        return service_status_result(
            &if active {
                ServiceStatus::Unresponsive(None)
            } else {
                ServiceStatus::Stopped
            },
            false,
        );
    };
    match ping_service(&state.endpoint).await {
        Ok(identity) if active && identity_matches(&identity, &state) => {}
        Err(error) if !active && endpoint_is_unavailable(&error) => {
            return remove_service_state(state_path, &state);
        }
        _ => return service_status_result(&ServiceStatus::Unresponsive(Some(state)), false),
    }
    let request = json!({"id":1,"method":"shutdown","params":{"service":service_identity(&state)}})
        .to_string();
    match send_managed_request(&state.endpoint, &request)
        .await
        .and_then(|line| response_result(&line))
    {
        Ok(result) if result["status"] == "shutting_down" => {
            wait_for_service_shutdown(&state, state_path).await
        }
        _ => service_status_result(&ServiceStatus::Unresponsive(Some(state)), false),
    }
}

async fn wait_for_service_shutdown(state: &ServiceState, state_path: &Path) -> CliResult {
    let deadline = Instant::now() + bounded_io::IO_TIMEOUT;
    loop {
        match ServiceCoordination::instance_active(state_path) {
            Ok(false) => {
                if IndexWriterGuard::acquire(&state.index_path).is_ok() {
                    match timeout_at(deadline, ping_service(&state.endpoint)).await {
                        Ok(Err(error)) if endpoint_is_unavailable(&error) => {
                            return remove_service_state(state_path, state);
                        }
                        _ => {
                            return service_status_result(
                                &ServiceStatus::Unresponsive(Some(state.clone())),
                                false,
                            );
                        }
                    }
                }
            }
            Ok(true) => {}
            Err(error) => return lifecycle_error("service ownership check failed", error),
        }
        if Instant::now() >= deadline {
            break;
        }
        sleep(Duration::from_millis(25)).await;
    }
    service_status_result(&ServiceStatus::Unresponsive(Some(state.clone())), false)
}

fn endpoint_is_unavailable(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) || cfg!(windows) && error.raw_os_error() == Some(2)
}

fn remove_service_state(state_path: &Path, state: &ServiceState) -> CliResult {
    match remove_state_if_matches(state_path, state) {
        Ok(true) => service_status_result(&ServiceStatus::Stopped, false),
        Ok(false) => lifecycle_error(
            "service state changed during stop",
            "refusing to remove another state",
        ),
        Err(error) => lifecycle_error("service state remove failed", error),
    }
}

fn lifecycle_error(context: &str, error: impl std::fmt::Display) -> CliResult {
    CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("{context}: {error}\n"),
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
    if let Err(error) = read_state(state_path) {
        return lifecycle_error("service state read failed", error);
    }
    let coordination = match ServiceCoordination::acquire(state_path) {
        Ok(guard) => guard,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
            return service_status_result(&ServiceStatus::Starting, false);
        }
        Err(error) => return lifecycle_error("service coordination failed", error),
    };
    let state_path = coordination.state_path();
    let endpoint = match resolve_managed_endpoint(&parsed.endpoint, state_path) {
        Ok(endpoint) => endpoint,
        Err(error) => return lifecycle_error("service endpoint resolve failed", error),
    };
    let state = match read_state(state_path) {
        Ok(state) => state,
        Err(error) => return lifecycle_error("service state read failed", error),
    };
    let active = match ServiceCoordination::instance_active(state_path) {
        Ok(active) => active,
        Err(error) => return lifecycle_error("service ownership check failed", error),
    };
    if let Some(state) = state {
        match ping_service(&state.endpoint).await {
            Ok(identity) if active && identity_matches(&identity, &state) => {
                if state.index_path != index_path
                    || !same_endpoint(&state.endpoint, &endpoint)
                    || state.auto_refresh_seconds != parsed.auto_refresh_seconds
                {
                    return lifecycle_error(
                        "service configuration mismatch",
                        "stop the running service before changing index, endpoint, or interval",
                    );
                }
                #[cfg(windows)]
                if let Err(error) = verify_reused_managed_endpoint(&state.endpoint).await {
                    return lifecycle_error(
                        "service endpoint security upgrade required; stop/start the service",
                        error,
                    );
                }
                return service_running_result(&state);
            }
            Err(error) if !active && endpoint_is_unavailable(&error) => {}
            _ => return service_status_result(&ServiceStatus::Unresponsive(Some(state)), false),
        }
    } else if active {
        return service_status_result(&ServiceStatus::Unresponsive(None), false);
    }
    let index_path = {
        let guard = match acquire_service_writer(&index_path) {
            Ok(guard) => guard,
            Err(result) => return result,
        };
        if let Err(result) =
            validate_index_root_metadata(guard.index_path(), parsed.auto_refresh_seconds.is_some())
        {
            return result;
        }
        guard.index_path().to_path_buf()
    };

    let mut child = match spawn_service_child(
        &index_path,
        &endpoint,
        parsed.auto_refresh_seconds,
        state_path,
    ) {
        Ok(child) => child,
        Err(result) => return result,
    };

    wait_for_started_service(
        &endpoint,
        &index_path,
        parsed.auto_refresh_seconds,
        state_path,
        &mut child,
    )
    .await
}

fn acquire_service_writer(index_path: &Path) -> Result<IndexWriterGuard, CliResult> {
    IndexWriterGuard::acquire(index_path).map_err(|error| CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("index writer acquire failed: {error}\n"),
    })
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

fn validate_index_root_metadata(index_path: &Path, auto_refresh: bool) -> Result<(), CliResult> {
    let store = FileIndexStore::open(index_path).map_err(|error| CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("index open failed: {error}\n"),
    })?;

    let root = store.root_path().ok_or_else(|| CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "index root metadata missing: run ai-file-search index <root> <index-file>\n"
            .to_owned(),
    })?;
    if !auto_refresh {
        return Ok(());
    }

    if store.scan_policy().is_none() {
        return Err(CliResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "index scan policy missing: rebuild with ai-file-search index <root> <index-file> [--exclude-name <name>] before enabling auto refresh\n".to_owned(),
        });
    }
    if !root.is_absolute() {
        return Err(CliResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "index root must be absolute for auto refresh: rebuild with ai-file-search index <root> <index-file>\n".to_owned(),
        });
    }
    let root = std::fs::canonicalize(root).map_err(|error| CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("index root resolve failed: {error}\n"),
    })?;
    if !root.is_dir() {
        return Err(CliResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: "index root is not a directory\n".to_owned(),
        });
    }
    Ok(())
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
    state_path: &Path,
) -> Result<Child, CliResult> {
    service_child_command(index_path, endpoint, auto_refresh_seconds, state_path)?
        .spawn()
        .map_err(|error| CliResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("service spawn failed: {error}\n"),
        })
}

fn service_child_command(
    index_path: &Path,
    endpoint: &str,
    auto_refresh_seconds: Option<u64>,
    state_path: &Path,
) -> Result<Command, CliResult> {
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

    let state_path = std::path::absolute(state_path).map_err(|error| CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: format!("service state path resolve failed: {error}\n"),
    })?;
    let mut command = Command::new(exe);
    command
        .arg("service-run")
        .arg(index_path)
        .arg(endpoint)
        .env(SERVICE_STATE_ENV, state_path)
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

    Ok(command)
}

async fn wait_for_started_service(
    endpoint: &str,
    index_path: &Path,
    auto_refresh_seconds: Option<u64>,
    state_path: &Path,
    child: &mut Child,
) -> CliResult {
    let deadline = Instant::now() + Duration::from_secs(2);
    for _ in 0..40 {
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => {
                return CliResult {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: format!("service exited before becoming healthy: {status}\n"),
                };
            }
            Err(error) => {
                reap_service_child(child);
                return CliResult {
                    exit_code: 1,
                    stdout: String::new(),
                    stderr: format!("service child status failed: {error}\n"),
                };
            }
        }
        let ready_state = match timeout_at(deadline, ping_service(endpoint)).await {
            Ok(Ok(identity)) => serde_json::from_value::<ServiceState>(identity).ok(),
            _ => None,
        };
        if let Some(state) = ready_state
            && state.pid == child.id()
            && state.index_path == index_path
            && same_endpoint(&state.endpoint, endpoint)
            && state.auto_refresh_seconds == auto_refresh_seconds
            && state.instance_id.as_ref().is_some_and(|id| !id.is_empty())
            && ServiceCoordination::instance_active(state_path).unwrap_or(false)
            && matches!(child.try_wait(), Ok(None))
        {
            if let Err(error) = write_state(state_path, &state) {
                reap_service_child(child);
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
        if Instant::now() >= deadline {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }

    reap_service_child(child);
    CliResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "service did not become healthy\n".to_owned(),
    }
}

fn reap_service_child(child: &mut Child) {
    if !matches!(child.try_wait(), Ok(Some(_))) {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn response_result(line: &str) -> io::Result<serde_json::Value> {
    let response: serde_json::Value = serde_json::from_str(line)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if response["id"] != 1 || response.get("error").is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid service response",
        ));
    }
    response
        .get("result")
        .filter(|result| result.is_object())
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing service result"))
}

async fn ping_service(endpoint: &str) -> io::Result<serde_json::Value> {
    let line = send_managed_request(endpoint, r#"{"id":1,"method":"ping","params":{}}"#).await?;
    let result = response_result(&line)?;
    if result["status"] != "ok" || !result["service"].is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unverified service identity",
        ));
    }
    Ok(result["service"].clone())
}

fn service_identity(state: &ServiceState) -> serde_json::Value {
    json!({"pid":state.pid,"index_path":state.index_path,"endpoint":state.endpoint,"auto_refresh_seconds":state.auto_refresh_seconds,"started_unix_seconds":state.started_unix_seconds,"instance_id":state.instance_id})
}

fn new_instance_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut hash = RandomState::new().build_hasher();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    hash.write_u128(timestamp);
    hash.write_u32(std::process::id());
    // A generation discriminator, not an authentication secret.
    format!("{}-{timestamp}-{:016x}", std::process::id(), hash.finish())
}

fn identity_matches(identity: &serde_json::Value, state: &ServiceState) -> bool {
    *identity == service_identity(state)
}

fn resolve_managed_endpoint(endpoint: &str, state_path: &Path) -> io::Result<String> {
    if endpoint.is_empty() || endpoint.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "empty or invalid endpoint",
        ));
    }
    #[cfg(windows)]
    {
        let _ = state_path;
        if endpoint == DEFAULT_ENDPOINT {
            Ok(format!(
                "{DEFAULT_ENDPOINT}-{}",
                ai_file_search_platform::current_user_sid()?
            ))
        } else {
            Ok(endpoint.to_owned())
        }
    }
    #[cfg(unix)]
    {
        unix_endpoint::resolve(endpoint, state_path)
    }
}

fn same_endpoint(left: &str, right: &str) -> bool {
    #[cfg(windows)]
    {
        pipe_name(left).eq_ignore_ascii_case(&pipe_name(right))
    }
    #[cfg(unix)]
    {
        left == right
    }
}

async fn send_managed_request(endpoint: &str, request: &str) -> io::Result<String> {
    timeout(bounded_io::IO_TIMEOUT, async {
        #[cfg(windows)]
        {
            let stream = open_managed_pipe(endpoint).await?;
            bounded_io::send_request(stream, request).await
        }
        #[cfg(unix)]
        {
            bounded_io::send_request(tokio::net::UnixStream::connect(endpoint).await?, request)
                .await
        }
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "service request timed out"))?
}

#[cfg(windows)]
async fn open_managed_pipe(
    endpoint: &str,
) -> io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    use tokio::net::windows::named_pipe::ClientOptions;
    loop {
        match ClientOptions::new().open(pipe_name(endpoint)) {
            Ok(stream) => return Ok(stream),
            Err(error) if error.raw_os_error() == Some(231) => {
                sleep(Duration::from_millis(10)).await;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(windows)]
async fn verify_reused_managed_endpoint(endpoint: &str) -> io::Result<()> {
    timeout(bounded_io::IO_TIMEOUT, async {
        let stream = open_managed_pipe(endpoint).await?;
        ai_file_search_platform::verify_private_pipe_client(&stream)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "pipe policy check timed out"))?
}

pub async fn service_run(
    index_path: &Path,
    endpoint: &str,
    auto_refresh_seconds: Option<u64>,
) -> i32 {
    let state_path = match std::path::absolute(default_state_path()) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("service state path resolve failed: {error}");
            return 1;
        }
    };
    let mut guard = match acquire_service_writer(index_path) {
        Ok(guard) => guard,
        Err(result) => {
            eprint!("{}", result.stderr);
            return result.exit_code;
        }
    };
    if auto_refresh_seconds.is_some()
        && let Err(result) = validate_index_root_metadata(guard.index_path(), true)
    {
        eprint!("{}", result.stderr);
        return result.exit_code;
    }
    let instance = match ServiceInstanceGuard::acquire(&state_path) {
        Ok(instance) => instance,
        Err(error) => {
            eprintln!("service instance acquire failed: {error}");
            return 1;
        }
    };
    let endpoint = match resolve_managed_endpoint(endpoint, &state_path) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            eprintln!("service endpoint resolve failed: {error}");
            return 1;
        }
    };
    let identity = ServiceState {
        endpoint,
        pid: std::process::id(),
        index_path: guard.index_path().to_path_buf(),
        started_unix_seconds: now_unix_seconds(),
        auto_refresh_seconds,
        instance_id: Some(new_instance_id()),
    };
    let result = serve_managed_ipc(
        &mut guard,
        &identity,
        &instance.artifact_paths(),
        &state_path,
    )
    .await;
    // Release the index before instance ownership: absence of instance ownership
    // is used by stop to confirm that this child's writer has finished.
    drop(guard);
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("service run failed: {error}");
            1
        }
    }
}

async fn handle_managed_connection<S>(
    guard: &mut IndexWriterGuard,
    mut stream: S,
    identity: &ServiceState,
    artifacts: &[PathBuf],
) -> io::Result<StreamStatus>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let line = bounded_io::read_request(&mut stream).await?;
    let request = Request::from_json_line(&line);
    let outcome = match request {
        Ok(request) if request.method == "ping" => HandlerOutcome {
            response: Response::success(
                request.id,
                json!({"status":"ok","service":service_identity(identity)}),
            ),
            shutdown_requested: false,
        },
        Ok(request)
            if request.method == "shutdown"
                && request
                    .params
                    .get("service")
                    .is_some_and(|target| !identity_matches(target, identity)) =>
        {
            HandlerOutcome {
                response: Response::error(request.id, "service identity mismatch"),
                shutdown_requested: false,
            }
        }
        _ => {
            let index_path = guard.index_path().to_path_buf();
            handle_json_request_with_guard(&index_path, &line, Some(guard), artifacts)
        }
    };
    let written = bounded_io::write_response(&mut stream, &outcome.response.to_json_line()).await;
    if outcome.shutdown_requested {
        return Ok(StreamStatus::ShutdownRequested);
    }
    written?;
    Ok(StreamStatus::ClientDisconnected)
}

#[cfg(windows)]
async fn serve_managed_ipc(
    guard: &mut IndexWriterGuard,
    identity: &ServiceState,
    artifacts: &[PathBuf],
    _state_path: &Path,
) -> io::Result<()> {
    let endpoint = pipe_name(&identity.endpoint);
    let mut pending = ai_file_search_platform::create_private_pipe(&endpoint, true)?;
    loop {
        if let Err(error) = pending.connect().await {
            if error.kind() == io::ErrorKind::Interrupted {
                tokio::task::yield_now().await;
                continue;
            }
            if matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionReset
                    | io::ErrorKind::ConnectionAborted
            ) || matches!(error.raw_os_error(), Some(232 | 233))
            {
                let _ = pending.disconnect();
                tokio::task::yield_now().await;
                continue;
            }
            return Err(error);
        }
        // Keep an unconnected instance alive while the serial owner serves or scans.
        // This also preserves the endpoint namespace continuously between requests.
        let next = ai_file_search_platform::create_private_pipe(&endpoint, false)?;
        let connected = std::mem::replace(&mut pending, next);
        if matches!(
            handle_managed_connection(guard, connected, identity, artifacts).await,
            Ok(StreamStatus::ShutdownRequested)
        ) {
            return Ok(());
        }
    }
}

#[cfg(unix)]
async fn serve_managed_ipc(
    guard: &mut IndexWriterGuard,
    identity: &ServiceState,
    artifacts: &[PathBuf],
    state_path: &Path,
) -> io::Result<()> {
    let (endpoint_guard, listener) =
        unix_endpoint::EndpointGuard::bind(Path::new(&identity.endpoint), state_path).await?;
    let mut runtime_artifacts = artifacts.to_vec();
    runtime_artifacts.extend(endpoint_guard.artifact_paths());
    loop {
        let (stream, _) = accept_with_retry(|| listener.accept()).await?;
        if matches!(
            handle_managed_connection(guard, stream, identity, &runtime_artifacts).await,
            Ok(StreamStatus::ShutdownRequested)
        ) {
            return Ok(());
        }
    }
}

#[cfg(any(unix, test))]
async fn accept_with_retry<T, F, Fut>(mut accept: F) -> io::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = io::Result<T>>,
{
    loop {
        match accept().await {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::ConnectionAborted
                ) =>
            {
                tokio::task::yield_now().await;
            }
            result => return result,
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
    handle_json_request_with_guard(index_path, line, None, &[])
}

fn handle_json_request_with_guard(
    index_path: &Path,
    line: &str,
    guard: Option<&mut IndexWriterGuard>,
    runtime_artifacts: &[PathBuf],
) -> HandlerOutcome {
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
            response: index_status(index_path, &request, runtime_artifacts),
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
            response: refresh(index_path, &request, guard, runtime_artifacts),
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

struct ScanComparison {
    root: PathBuf,
    files: Vec<IndexedFile>,
    summary: RefreshSummary,
}

fn scan_index(
    options: ScanOptions,
    root: &Path,
    index_path: &Path,
    runtime_artifacts: &[PathBuf],
) -> io::Result<Vec<IndexedFile>> {
    Scanner::new(options).scan_for_index_with_artifacts(root, index_path, runtime_artifacts)
}

fn scan_and_compare(
    store: &FileIndexStore,
    index_path: &Path,
    params: &serde_json::Value,
    options: Option<ScanOptions>,
    runtime_artifacts: &[PathBuf],
    scan: impl FnOnce(ScanOptions, &Path, &Path, &[PathBuf]) -> io::Result<Vec<IndexedFile>>,
) -> Result<ScanComparison, String> {
    let root = index_root(store, params)?;
    let options = store.resolve_scan_options(options)?;
    let files = scan(options, &root, index_path, runtime_artifacts)
        .map_err(|error| format!("scan failed: {error}"))?;
    let summary = RefreshSummary::compare_ordered(store.iter_files(), &files);
    Ok(ScanComparison {
        root,
        files,
        summary,
    })
}

fn publish_comparison<'guard>(
    store: &mut FileIndexWriter<'guard>,
    comparison: ScanComparison,
    save_only_if_changed: bool,
    save: impl FnOnce(&FileIndexWriter<'guard>) -> io::Result<()>,
) -> Result<(usize, RefreshSummary), String> {
    let ScanComparison {
        root,
        files,
        summary,
    } = comparison;
    let scanned_files = files.len();
    if !save_only_if_changed || summary.has_changes() {
        store.set_root_path(&root);
        store.replace_all(files);
        save(store).map_err(|error| format!("index save failed: {error}"))?;
    }
    Ok((scanned_files, summary))
}

fn summary_result(scanned_files: usize, summary: &RefreshSummary) -> serde_json::Value {
    json!({
        "scanned_files": scanned_files,
        "added": summary.added,
        "updated": summary.updated,
        "removed": summary.removed,
        "unchanged": summary.unchanged,
    })
}

fn index_status(index_path: &Path, request: &Request, runtime_artifacts: &[PathBuf]) -> Response {
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
    let comparison = match scan_and_compare(
        &store,
        index_path,
        &request.params,
        options,
        runtime_artifacts,
        scan_index,
    ) {
        Ok(comparison) => comparison,
        Err(message) => return Response::error(request.id, message),
    };
    let mut result = summary_result(comparison.files.len(), &comparison.summary);
    result["needs_refresh"] = json!(comparison.summary.has_changes());
    Response::success(request.id, result)
}

fn refresh(
    index_path: &Path,
    request: &Request,
    guard: Option<&mut IndexWriterGuard>,
    runtime_artifacts: &[PathBuf],
) -> Response {
    let options = match scan_options(&request.params) {
        Ok(options) => options,
        Err(message) => return Response::error(request.id, message),
    };

    let mut standalone_guard;
    let guard = if let Some(guard) = guard {
        guard
    } else {
        standalone_guard = match IndexWriterGuard::acquire(index_path) {
            Ok(guard) => guard,
            Err(error) => {
                return Response::error(
                    request.id,
                    format!("index writer acquire failed: {error}"),
                );
            }
        };
        &mut standalone_guard
    };
    let index_path = guard.index_path().to_path_buf();
    let mut store = match FileIndexWriter::open(guard) {
        Ok(store) => store,
        Err(error) => return Response::error(request.id, format!("index open failed: {error}")),
    };
    let comparison = match scan_and_compare(
        &store,
        &index_path,
        &request.params,
        options,
        runtime_artifacts,
        scan_index,
    ) {
        Ok(comparison) => comparison,
        Err(message) => return Response::error(request.id, message),
    };
    let (scanned_files, summary) =
        match publish_comparison(&mut store, comparison, false, FileIndexWriter::save) {
            Ok(result) => result,
            Err(message) => return Response::error(request.id, message),
        };

    Response::success(request.id, summary_result(scanned_files, &summary))
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

fn scan_options(params: &serde_json::Value) -> Result<Option<ScanOptions>, &'static str> {
    let Some(excluded_names) = params.get("exclude_names") else {
        return Ok(None);
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
        .map(Some)
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
    handle_json_stream_with_guard(index_path, stream, None, &[]).await
}

async fn handle_json_stream_with_guard<S>(
    index_path: &Path,
    stream: S,
    mut guard: Option<&mut IndexWriterGuard>,
    runtime_artifacts: &[PathBuf],
) -> io::Result<StreamStatus>
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

        let outcome = handle_json_request_with_guard(
            index_path,
            &line,
            guard.as_deref_mut(),
            runtime_artifacts,
        );
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

#[cfg(windows)]
async fn handle_one_json_request<S>(
    index_path: &Path,
    stream: S,
    guard: &mut IndexWriterGuard,
    runtime_artifacts: &[PathBuf],
) -> io::Result<StreamStatus>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    if stream.read_line(&mut line).await? == 0 {
        return Ok(StreamStatus::ClientDisconnected);
    }

    let outcome = handle_json_request_with_guard(index_path, &line, Some(guard), runtime_artifacts);
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
    if endpoint
        .get(..PIPE_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(PIPE_PREFIX))
    {
        endpoint.to_owned()
    } else {
        format!("{PIPE_PREFIX}{endpoint}")
    }
}

/// Serves JSON-RPC requests over the platform IPC transport.
///
/// # Errors
///
/// Returns an I/O error when the index is already owned, the endpoint cannot be
/// created, or a client stream cannot be handled.
pub async fn serve_ipc(index_path: &Path, endpoint: &str) -> io::Result<()> {
    let mut guard = IndexWriterGuard::acquire(index_path)?;
    serve_ipc_with_guard(&mut guard, endpoint, &[]).await
}

#[cfg(windows)]
async fn serve_ipc_with_guard(
    guard: &mut IndexWriterGuard,
    endpoint: &str,
    runtime_artifacts: &[PathBuf],
) -> io::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let index_path = guard.index_path().to_path_buf();
    let endpoint = pipe_name(endpoint);
    loop {
        let server = ServerOptions::new().create(&endpoint)?;
        server.connect().await?;
        let status = handle_one_json_request(&index_path, server, guard, runtime_artifacts).await?;
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

#[cfg(unix)]
async fn serve_ipc_with_guard(
    guard: &mut IndexWriterGuard,
    endpoint: &str,
    runtime_artifacts: &[PathBuf],
) -> io::Result<()> {
    use tokio::net::UnixListener;

    let index_path = guard.index_path().to_path_buf();
    let _ = std::fs::remove_file(endpoint);
    let listener = UnixListener::bind(endpoint)?;
    loop {
        let (stream, _) = listener.accept().await?;
        let status =
            handle_json_stream_with_guard(&index_path, stream, Some(guard), runtime_artifacts)
                .await?;
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

#[cfg(test)]
mod startup_tests {
    use super::*;
    use std::fs;
    use std::time::Instant;

    #[test]
    #[ignore = "startup subprocess helper"]
    fn startup_child_without_endpoint() {
        let index = PathBuf::from(std::env::var_os("AIFS_TEST_STARTUP_INDEX").unwrap());
        if let Ok(endpoint) = std::env::var("AIFS_TEST_STARTUP_ENDPOINT") {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            assert_eq!(runtime.block_on(service_run(&index, &endpoint, None)), 0);
            return;
        }
        let _guard = IndexWriterGuard::acquire(&index).unwrap();
        if let Some(ready) = std::env::var_os("AIFS_TEST_STARTUP_READY") {
            fs::write(PathBuf::from(ready), b"ready").unwrap();
            std::thread::sleep(Duration::from_mins(1));
        }
    }

    #[tokio::test]
    async fn startup_timeout_kills_and_reaps_only_owned_child() {
        let fixture = StartupFixture::new("timeout");
        let ready = fixture.path.join("ready");
        let mut child = fixture.child(Some(&ready));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !ready.exists() {
            assert!(Instant::now() < deadline);
            assert!(child.0.try_wait().unwrap().is_none());
            sleep(Duration::from_millis(10)).await;
        }
        assert!(IndexWriterGuard::acquire(&fixture.index).is_err());
        let result = wait_for_started_service(
            &fixture.endpoint(),
            &fixture.index,
            None,
            &fixture.path.join("state.json"),
            &mut child.0,
        )
        .await;
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.stderr, "service did not become healthy\n");
        assert!(
            child.0.try_wait().unwrap().is_some(),
            "owned startup child must be reaped"
        );
        assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());
    }

    #[tokio::test]
    async fn startup_child_lock_race_exits_quickly_and_is_reaped() {
        let fixture = StartupFixture::new("lock-race");
        let owner = IndexWriterGuard::acquire(&fixture.index).unwrap();
        let mut child = fixture.child(None);
        let started = Instant::now();
        let result = wait_for_started_service(
            &fixture.endpoint(),
            &fixture.index,
            None,
            &fixture.path.join("state.json"),
            &mut child.0,
        )
        .await;
        assert_eq!(result.exit_code, 1);
        assert!(
            result
                .stderr
                .starts_with("service exited before becoming healthy:")
        );
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(child.0.try_wait().unwrap().is_some());
        assert!(!fixture.path.join("state.json").exists());
        assert!(IndexWriterGuard::acquire(&fixture.index).is_err());
        drop(owner);
        assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn failed_atomic_state_publication_reaps_ready_owned_child_and_preserves_state() {
        use std::os::windows::fs::OpenOptionsExt;
        let fixture = StartupFixture::new("state-write-failure");
        let endpoint = fixture.endpoint();
        let state_path = fixture.path.join("state.json");
        let old = ServiceState {
            endpoint: "old-endpoint".into(),
            pid: 99,
            index_path: fixture.index.clone(),
            started_unix_seconds: 1,
            auto_refresh_seconds: None,
            instance_id: None,
        };
        write_state(&state_path, &old).unwrap();
        let before = fs::read(&state_path).unwrap();
        let _read_only = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&state_path)
            .unwrap();
        let mut child = TestChild(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "startup_tests::startup_child_without_endpoint",
                    "--ignored",
                ])
                .env("AIFS_TEST_STARTUP_INDEX", &fixture.index)
                .env("AIFS_TEST_STARTUP_ENDPOINT", &endpoint)
                .env(SERVICE_STATE_ENV, &state_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let index = std::path::absolute(&fixture.index).unwrap();
        // The child resolves its new index through the canonical parent.
        let published_index = fs::canonicalize(index.parent().unwrap())
            .unwrap()
            .join(index.file_name().unwrap());
        let result =
            wait_for_started_service(&endpoint, &published_index, None, &state_path, &mut child.0)
                .await;
        assert_eq!(result.exit_code, 1);
        assert!(
            result.stderr.starts_with("service state write failed:"),
            "{}",
            result.stderr
        );
        assert!(child.0.try_wait().unwrap().is_some());
        assert!(!ServiceCoordination::instance_active(&state_path).unwrap());
        assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());
        assert_eq!(fs::read(&state_path).unwrap(), before);
        assert!(endpoint_is_unavailable(
            &ping_service(&endpoint).await.unwrap_err()
        ));
        assert!(!fs::read_dir(&fixture.path).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    struct TestChild(Child);

    impl Drop for TestChild {
        fn drop(&mut self) {
            reap_service_child(&mut self.0);
        }
    }

    struct StartupFixture {
        path: PathBuf,
        index: PathBuf,
    }

    impl StartupFixture {
        fn new(name: &str) -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "aifs-startup-{name}-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            let index = path.join("index.txt");
            Self { path, index }
        }
        fn child(&self, ready: Option<&Path>) -> TestChild {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "startup_tests::startup_child_without_endpoint",
                    "--ignored",
                ])
                .env("AIFS_TEST_STARTUP_INDEX", &self.index)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(ready) = ready {
                command.env("AIFS_TEST_STARTUP_READY", ready);
            }
            TestChild(command.spawn().unwrap())
        }
        fn endpoint(&self) -> String {
            #[cfg(windows)]
            {
                self.path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            }
            #[cfg(unix)]
            {
                self.path
                    .join("missing.sock")
                    .to_string_lossy()
                    .into_owned()
            }
        }
    }

    impl Drop for StartupFixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }
}
