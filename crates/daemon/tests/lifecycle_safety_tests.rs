#![cfg(any(windows, unix))]

use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_file_search_daemon::send_ipc_request;
use ai_file_search_daemon::service::{
    SERVICE_STATE_ENV, ServiceCoordination, ServiceInstanceGuard, ServiceState, read_state,
    write_state,
};
use ai_file_search_indexer::{FileIndexWriter, IndexWriterGuard, ScanOptions};
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::time::{sleep, timeout};

const CLI_BOUND: Duration = Duration::from_secs(8);
const RPC_BOUND: Duration = Duration::from_secs(2);
const PING: &str = r#"{"id":1,"method":"ping","params":{}}"#;
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn malformed_state_status_is_an_explicit_error_and_preserves_bytes() {
    let fixture = Fixture::new();
    let malformed = b"{not valid service state}\n";
    fs::write(&fixture.state, malformed).unwrap();
    let mut child = fixture.cli(&["service", "status", "--json"], "status");
    let output = child.output().await;
    assert_eq!(fs::read(&fixture.state).unwrap(), malformed);
    assert_malformed_error(&output);
    assert_eq!(output_json(&output)["status"], "error");
    assert!(
        output_json(&output)["reason"]
            .as_str()
            .unwrap()
            .contains("service state")
    );
}

#[tokio::test]
async fn malformed_state_start_is_an_explicit_error_and_preserves_bytes() {
    let fixture = Fixture::new();
    let malformed = b"{not valid service state}\n";
    fs::write(&fixture.state, malformed).unwrap();
    let mut cleanup = DetachedGuard::new(&fixture, vec![fixture.endpoint.clone()]);
    let mut child = fixture.start(&fixture.index, &fixture.endpoint, None, "start");
    let output = child.output().await;
    assert_eq!(fs::read(&fixture.state).unwrap(), malformed);
    assert_malformed_error(&output);
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_endpoint_closed(&fixture.endpoint).await;
    assert!(!ServiceCoordination::instance_active(&fixture.state).unwrap());
    assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());
    cleanup.disarm();
}

#[tokio::test]
async fn malformed_state_stop_is_an_explicit_error_and_preserves_bytes() {
    let fixture = Fixture::new();
    let malformed = b"{not valid service state}\n";
    fs::write(&fixture.state, malformed).unwrap();
    let mut child = fixture.cli(&["service", "stop"], "stop");
    let output = child.output().await;
    assert_eq!(fs::read(&fixture.state).unwrap(), malformed);
    assert_malformed_error(&output);
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[tokio::test]
async fn held_startup_without_state_reports_starting_and_does_not_spawn() {
    let fixture = Fixture::new();
    let mut cleanup = DetachedGuard::new(&fixture, vec![fixture.endpoint.clone()]);
    let coordination = ServiceCoordination::acquire(&fixture.state).unwrap();
    let mut status = fixture.cli(&["service", "status", "--json"], "status");
    let output = status.output().await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output_json(&output)["status"], "starting");
    let mut start = fixture.start(&fixture.index, &fixture.endpoint, None, "start");
    assert_text_status(&start.output().await, "starting", 1);
    assert!(!fixture.state.exists());
    assert!(!ServiceCoordination::instance_active(&fixture.state).unwrap());
    assert_endpoint_closed(&fixture.endpoint).await;
    assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());
    drop(coordination);
    cleanup.disarm();
}

#[tokio::test]
async fn held_instance_without_state_is_unresponsive_and_start_stop_do_not_spawn() {
    let fixture = Fixture::new();
    let mut cleanup = DetachedGuard::new(&fixture, vec![fixture.endpoint.clone()]);
    let instance = ServiceInstanceGuard::acquire(&fixture.state).unwrap();
    let mut status = fixture.cli(&["service", "status", "--json"], "status");
    let output = status.output().await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output_json(&output)["status"], "unresponsive");
    let mut stop = fixture.cli(&["service", "stop"], "stop");
    assert_text_status(&stop.output().await, "unresponsive", 1);
    let mut start = fixture.start(&fixture.index, &fixture.endpoint, None, "start");
    assert_text_status(&start.output().await, "unresponsive", 1);
    assert!(!fixture.state.exists());
    assert!(ServiceCoordination::instance_active(&fixture.state).unwrap());
    assert_endpoint_closed(&fixture.endpoint).await;
    assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());
    drop(instance);
    cleanup.disarm();
}

#[tokio::test]
async fn matching_configuration_returns_running_without_respawning() {
    for interval in [None, Some(300)] {
        let fixture = Fixture::new();
        let mut service = fixture.hidden(interval);
        let state = fixture.publish_owned_state(&mut service, interval).await;
        let before = fs::read(&fixture.state).unwrap();
        let mut start = fixture.start(&fixture.index, &fixture.endpoint, interval, "start");
        let output = start.output().await;
        assert_text_status(&output, "running", 0);
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains(&format!("pid={}", service.child.id()))
        );
        assert_eq!(read_state(&fixture.state).unwrap(), Some(state));
        assert_eq!(fs::read(&fixture.state).unwrap(), before);
        fixture.assert_owned_running(&mut service).await;
        fixture.stop_owned(&mut service).await;
    }
}

#[tokio::test]
async fn a_different_index_is_rejected_instead_of_false_already_running() {
    let fixture = Fixture::new();
    let other = fixture.path.join("other-index.txt");
    save_index(&other, &fixture.root);
    let mut service = fixture.hidden(None);
    fixture.publish_owned_state(&mut service, None).await;
    let before = fs::read(&fixture.state).unwrap();
    let mut start = fixture.start(&other, &fixture.endpoint, None, "different-index");
    assert_config_mismatch(&start.output().await);
    assert_eq!(fs::read(&fixture.state).unwrap(), before);
    assert!(IndexWriterGuard::acquire(&other).is_ok());
    fixture.assert_owned_running(&mut service).await;
    fixture.stop_owned(&mut service).await;
}

#[tokio::test]
async fn a_different_endpoint_is_rejected_instead_of_false_already_running() {
    let fixture = Fixture::new();
    let other = fixture.other_endpoint();
    let mut service = fixture.hidden(None);
    fixture.publish_owned_state(&mut service, None).await;
    let before = fs::read(&fixture.state).unwrap();
    let mut start = fixture.start(&fixture.index, &other, None, "different-endpoint");
    assert_config_mismatch(&start.output().await);
    assert_eq!(fs::read(&fixture.state).unwrap(), before);
    assert_endpoint_closed(&other).await;
    fixture.assert_owned_running(&mut service).await;
    fixture.stop_owned(&mut service).await;
}

#[tokio::test]
async fn a_different_interval_is_rejected_including_removing_auto_refresh() {
    let fixture = Fixture::new();
    let mut service = fixture.hidden(Some(300));
    fixture.publish_owned_state(&mut service, Some(300)).await;
    let before = fs::read(&fixture.state).unwrap();
    for (number, interval) in [Some(301), None].into_iter().enumerate() {
        let mut start = fixture.start(
            &fixture.index,
            &fixture.endpoint,
            interval,
            &format!("different-interval-{number}"),
        );
        assert_config_mismatch(&start.output().await);
        assert_eq!(fs::read(&fixture.state).unwrap(), before);
        fixture.assert_owned_running(&mut service).await;
    }
    fixture.stop_owned(&mut service).await;
}

#[tokio::test]
async fn same_configuration_concurrent_starts_share_one_child() {
    let fixture = Fixture::new();
    let mut cleanup = DetachedGuard::new(&fixture, vec![fixture.endpoint.clone()]);
    let mut first = fixture.start(&fixture.index, &fixture.endpoint, None, "first-start");
    let mut second = fixture.start(&fixture.index, &fixture.endpoint, None, "second-start");
    let (first_output, second_output) = tokio::join!(first.output(), second.output());
    let outputs = [&first_output, &second_output];
    assert!(
        outputs.iter().any(|output| output.status.success()),
        "{outputs:?}"
    );
    let state = read_state(&fixture.state)
        .unwrap()
        .expect("one child must publish state");
    assert_eq!(state.endpoint, fixture.endpoint);
    assert_eq!(state.index_path, fs::canonicalize(&fixture.index).unwrap());
    let mut started_count = 0;
    for output in outputs {
        if output.status.success() {
            let status = if String::from_utf8_lossy(&output.stdout).starts_with("started ") {
                started_count += 1;
                "started"
            } else {
                "running"
            };
            assert_text_status(output, status, 0);
            assert!(
                String::from_utf8_lossy(&output.stdout).contains(&format!("pid={}", state.pid)),
                "{output:?}"
            );
        } else {
            assert_text_status(output, "starting", 1);
        }
    }
    assert_eq!(started_count, 1, "{outputs:?}");
    assert_eq!(
        rpc(&fixture.endpoint, PING).await["result"]["service"]["pid"],
        state.pid
    );
    assert!(ServiceCoordination::instance_active(&fixture.state).unwrap());
    assert!(IndexWriterGuard::acquire(&fixture.index).is_err());
    let before = fs::read(&fixture.state).unwrap();
    let mut again = fixture.start(&fixture.index, &fixture.endpoint, None, "matching-start");
    let output = again.output().await;
    assert_text_status(&output, "running", 0);
    assert!(String::from_utf8_lossy(&output.stdout).contains(&format!("pid={}", state.pid)));
    assert_eq!(fs::read(&fixture.state).unwrap(), before);
    let mut stop = fixture.cli(&["service", "stop"], "stop");
    assert_text_status(&stop.output().await, "stopped", 0);
    assert!(!fixture.state.exists());
    assert!(!ServiceCoordination::instance_active(&fixture.state).unwrap());
    assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());
    assert_endpoint_closed(&fixture.endpoint).await;
    cleanup.disarm();
}

#[tokio::test]
async fn same_state_concurrent_starts_publish_exactly_one_managed_owner() {
    let fixture = Fixture::new();
    let other_index = fixture.path.join("other-index.txt");
    save_index(&other_index, &fixture.root);
    let other_endpoint = fixture.other_endpoint();
    let mut cleanup = DetachedGuard::new(
        &fixture,
        vec![fixture.endpoint.clone(), other_endpoint.clone()],
    );
    // Distinct indexes and endpoints prevent the index writer or bind from masking
    // missing same-state coordination. Both CLI children start before either wait.
    let mut first = fixture.start(&fixture.index, &fixture.endpoint, None, "first-start");
    let mut second = fixture.start(&other_index, &other_endpoint, None, "second-start");
    let (first_output, second_output) = tokio::join!(first.output(), second.output());
    let outputs = [&first_output, &second_output];
    assert_eq!(
        outputs
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1,
        "{outputs:?}"
    );
    let rejected = outputs
        .iter()
        .find(|output| !output.status.success())
        .unwrap();
    assert_eq!(rejected.status.code(), Some(1), "{rejected:?}");
    assert!(
        String::from_utf8_lossy(&rejected.stdout).starts_with("starting")
            || String::from_utf8_lossy(&rejected.stderr).contains("configuration mismatch"),
        "{rejected:?}"
    );
    let state = read_state(&fixture.state)
        .unwrap()
        .expect("winner must publish state");
    let (winner_index, winner_endpoint, loser_index, loser_endpoint) =
        if first_output.status.success() {
            (
                &fixture.index,
                &fixture.endpoint,
                &other_index,
                &other_endpoint,
            )
        } else {
            (
                &other_index,
                &other_endpoint,
                &fixture.index,
                &fixture.endpoint,
            )
        };
    assert_eq!(state.endpoint, *winner_endpoint);
    assert_eq!(state.index_path, fs::canonicalize(winner_index).unwrap());
    assert_eq!(
        rpc(winner_endpoint, PING).await["result"]["service"]["pid"],
        state.pid
    );
    assert!(ServiceCoordination::instance_active(&fixture.state).unwrap());
    assert!(IndexWriterGuard::acquire(winner_index).is_err());
    assert!(IndexWriterGuard::acquire(loser_index).is_ok());
    assert_endpoint_closed(loser_endpoint).await;
    let before = fs::read(&fixture.state).unwrap();
    let mut again = fixture.start(winner_index, winner_endpoint, None, "matching-start");
    assert_text_status(&again.output().await, "running", 0);
    assert_eq!(fs::read(&fixture.state).unwrap(), before);
    let mut stop = fixture.cli(&["service", "stop"], "stop");
    assert_text_status(&stop.output().await, "stopped", 0);
    assert!(!fixture.state.exists());
    assert!(!ServiceCoordination::instance_active(&fixture.state).unwrap());
    assert!(IndexWriterGuard::acquire(winner_index).is_ok());
    assert_endpoint_closed(winner_endpoint).await;
    cleanup.disarm();
}

#[tokio::test]
async fn copied_foreign_state_without_ownership_cannot_report_running_stop_or_delete() {
    let fixture = Fixture::new();
    let mut service = fixture.hidden(None);
    fixture.publish_owned_state(&mut service, None).await;
    let before = fs::read(&fixture.state).unwrap();
    let foreign = fixture.path.join("foreign-state.json");
    fs::write(&foreign, &before).unwrap();
    assert!(!ServiceCoordination::instance_active(&foreign).unwrap());
    let mut status = fixture.cli_at(&foreign, &["service", "status", "--json"], "foreign-status");
    let output = status.output().await;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(output_json(&output)["status"], "unresponsive");
    let mut stop = fixture.cli_at(&foreign, &["service", "stop"], "foreign-stop");
    assert_text_status(&stop.output().await, "unresponsive", 1);
    let mut command = fixture.command(&foreign);
    command
        .args(["service", "start"])
        .arg(&fixture.index)
        .args(["--endpoint", &fixture.endpoint]);
    let mut start = fixture.spawn(&mut command, "foreign-start");
    assert_text_status(&start.output().await, "unresponsive", 1);
    assert_eq!(fs::read(&foreign).unwrap(), before);
    assert_eq!(fs::read(&fixture.state).unwrap(), before);
    assert!(!ServiceCoordination::instance_active(&foreign).unwrap());
    fixture.assert_owned_running(&mut service).await;
    fixture.stop_owned(&mut service).await;
    assert_eq!(fs::read(&foreign).unwrap(), before);
}

#[tokio::test]
async fn wrong_identity_in_owned_state_cannot_stop_the_actual_child_or_delete_state() {
    let fixture = Fixture::new();
    let mut service = fixture.hidden(None);
    let actual = fixture.publish_owned_state(&mut service, None).await;
    for field in ["pid", "instance_id", "started_unix_seconds"] {
        let mut wrong = actual.clone();
        match field {
            "pid" => wrong.pid += 1,
            "instance_id" => wrong.instance_id = Some("different-generation".to_owned()),
            _ => wrong.started_unix_seconds += 1,
        }
        write_state(&fixture.state, &wrong).unwrap();
        let before = fs::read(&fixture.state).unwrap();
        let mut status = fixture.cli(
            &["service", "status", "--json"],
            &format!("wrong-{field}-status"),
        );
        let output = status.output().await;
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert_eq!(output_json(&output)["status"], "unresponsive");
        let mut stop = fixture.cli(&["service", "stop"], &format!("wrong-{field}-stop"));
        assert_text_status(&stop.output().await, "unresponsive", 1);
        let mut start = fixture.start(
            &fixture.index,
            &fixture.endpoint,
            None,
            &format!("wrong-{field}-start"),
        );
        assert_text_status(&start.output().await, "unresponsive", 1);
        assert_eq!(fs::read(&fixture.state).unwrap(), before);
        assert!(service.child.try_wait().unwrap().is_none());
        let identity = rpc(&fixture.endpoint, PING).await["result"]["service"].clone();
        assert_eq!(identity["pid"], service.child.id());
        assert_eq!(identity["instance_id"], json!(actual.instance_id));
        assert_eq!(
            identity["started_unix_seconds"],
            actual.started_unix_seconds
        );
    }
    write_state(&fixture.state, &actual).unwrap();
    fixture.stop_owned(&mut service).await;
}

#[tokio::test]
async fn busy_health_check_is_bounded_and_never_discards_a_live_owned_state() {
    let fixture = Fixture::new();
    let mut service = fixture.hidden(None);
    fixture.publish_owned_state(&mut service, None).await;
    let before = fs::read(&fixture.state).unwrap();
    let mut busy = connect(&fixture.endpoint).await;
    busy.write_all(b"{").await.unwrap();
    let started = tokio::time::Instant::now();
    let mut status = fixture.cli(&["service", "status", "--json"], "busy-status");
    let output = status.output().await;
    assert!(started.elapsed() < CLI_BOUND);
    let status = output_json(&output)["status"].as_str().unwrap().to_owned();
    match status.as_str() {
        "running" => assert_eq!(output.status.code(), Some(0)),
        "unresponsive" => assert_eq!(output.status.code(), Some(1)),
        _ => panic!("busy live owner was misclassified: {output:?}"),
    }
    assert_eq!(fs::read(&fixture.state).unwrap(), before);
    assert!(ServiceCoordination::instance_active(&fixture.state).unwrap());
    assert!(service.child.try_wait().unwrap().is_none());
    drop(busy);
    fixture.assert_owned_running(&mut service).await;
    fixture.stop_owned(&mut service).await;
}

fn assert_malformed_error(output: &Output) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("service state") && stderr.contains("failed"),
        "{output:?}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("stopped"),
        "{output:?}"
    );
}

fn assert_config_mismatch(output: &Output) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("configuration mismatch"),
        "{output:?}"
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout).starts_with("running"),
        "{output:?}"
    );
}

fn assert_text_status(output: &Output, status: &str, code: i32) {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .next(),
        Some(status),
        "{output:?}"
    );
    assert!(output.stderr.is_empty(), "{output:?}");
}

fn output_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("invalid CLI JSON: {error}; {output:?}"))
}

fn retry_connect(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) || cfg!(windows) && matches!(error.raw_os_error(), Some(2 | 231))
}

async fn rpc(endpoint: &str, request: &str) -> Value {
    let response = timeout(RPC_BOUND, async {
        loop {
            match send_ipc_request(endpoint, request).await {
                Ok(response) => return response,
                Err(error) if retry_connect(&error) => sleep(Duration::from_millis(20)).await,
                Err(error) => panic!("IPC failed: {error}"),
            }
        }
    })
    .await
    .expect("IPC request exceeded its outer timeout");
    assert!(response.ends_with('\n'), "{response:?}");
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["id"], 1, "{response}");
    response
}

#[cfg(windows)]
type Client = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(unix)]
type Client = tokio::net::UnixStream;

async fn connect(endpoint: &str) -> Client {
    timeout(RPC_BOUND, async {
        loop {
            #[cfg(windows)]
            let connected = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(format!(r"\\.\pipe\{endpoint}"));
            #[cfg(unix)]
            let connected = tokio::net::UnixStream::connect(endpoint).await;
            match connected {
                Ok(client) => return client,
                Err(error) if retry_connect(&error) => sleep(Duration::from_millis(20)).await,
                Err(error) => panic!("IPC connect failed: {error}"),
            }
        }
    })
    .await
    .expect("IPC endpoint remained unavailable or pipe busy")
}

async fn assert_endpoint_closed(endpoint: &str) {
    for _ in 0..4 {
        match timeout(Duration::from_millis(500), send_ipc_request(endpoint, PING)).await {
            Ok(Err(error))
                if error.kind() == io::ErrorKind::NotFound
                    || error.kind() == io::ErrorKind::ConnectionRefused
                    || cfg!(windows) && error.raw_os_error() == Some(2) => {}
            other => panic!("unexpected listener at rejected/stopped endpoint: {other:?}"),
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn save_index(index: &Path, root: &Path) {
    let mut guard = IndexWriterGuard::acquire(index).unwrap();
    let mut writer = FileIndexWriter::new(&mut guard);
    writer.set_root_path(fs::canonicalize(root).unwrap());
    writer.set_scan_policy(ScanOptions::default());
    writer.save().unwrap();
}

struct ChildGuard {
    child: Child,
    stdout: PathBuf,
    stderr: PathBuf,
}

impl ChildGuard {
    async fn output(&mut self) -> Output {
        let status = timeout(CLI_BOUND, async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("CLI subprocess exceeded its bounded wait");
        Output {
            status,
            stdout: fs::read(&self.stdout).unwrap(),
            stderr: fs::read(&self.stderr).unwrap(),
        }
    }
}

struct DetachedGuard {
    state: PathBuf,
    endpoints: Vec<String>,
    armed: bool,
}

impl DetachedGuard {
    fn new(fixture: &Fixture, endpoints: Vec<String>) -> Self {
        Self {
            state: fixture.state.clone(),
            endpoints,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for DetachedGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let endpoints = self.endpoints.clone();
        let state = self.state.clone();
        // service start deliberately detaches its grandchild. Close only the
        // fixture's known endpoints, never kill the advisory state PID.
        let cleanup = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async {
                for endpoint in endpoints {
                    let _ = timeout(RPC_BOUND, async {
                        loop {
                            match send_ipc_request(&endpoint, PING).await {
                                Ok(line) => {
                                    let response: Value =
                                        serde_json::from_str(&line).unwrap_or(Value::Null);
                                    let identity = response["result"]["service"].clone();
                                    let params = if identity.is_null() {
                                        json!({})
                                    } else {
                                        json!({"service": identity})
                                    };
                                    let request =
                                        json!({"id":1,"method":"shutdown","params":params})
                                            .to_string();
                                    let _ = send_ipc_request(&endpoint, &request).await;
                                    break;
                                }
                                Err(error) if retry_connect(&error) => {
                                    if !ServiceCoordination::instance_active(&state).unwrap_or(true)
                                        && !ServiceCoordination::startup_active(&state)
                                            .unwrap_or(true)
                                    {
                                        break;
                                    }
                                    sleep(Duration::from_millis(20)).await;
                                }
                                Err(_) => break,
                            }
                        }
                    })
                    .await;
                }
                let _ = timeout(Duration::from_secs(6), async {
                    while ServiceCoordination::instance_active(&state).unwrap_or(true) {
                        sleep(Duration::from_millis(20)).await;
                    }
                })
                .await;
            });
        });
        let _ = cleanup.join();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Fixture {
    path: PathBuf,
    state: PathBuf,
    root: PathBuf,
    index: PathBuf,
    endpoint: String,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!("aifs-ls-{}-{nonce}-{sequence}", std::process::id());
        #[cfg(windows)]
        let path = std::env::temp_dir().join(&name);
        #[cfg(unix)]
        let path = PathBuf::from("/tmp").join(format!(
            "aifs-ls-{}-{nonce:x}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        #[cfg(unix)]
        let path = fs::canonicalize(path).unwrap();
        let root = path.join("root");
        fs::create_dir(&root).unwrap();
        let index = path.join("index.txt");
        save_index(&index, &root);
        let endpoint = if cfg!(windows) {
            name
        } else {
            path.join("service.sock").to_string_lossy().into_owned()
        };
        Self {
            state: path.join("state.json"),
            path,
            root,
            index,
            endpoint,
        }
    }

    fn cli(&self, args: &[&str], label: &str) -> ChildGuard {
        self.cli_at(&self.state, args, label)
    }

    fn cli_at(&self, state: &Path, args: &[&str], label: &str) -> ChildGuard {
        let mut command = self.command(state);
        command.args(args);
        self.spawn(&mut command, label)
    }

    fn command(&self, state: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-file-search-daemon"));
        command
            .current_dir(&self.path)
            .env(SERVICE_STATE_ENV, state)
            .stdin(Stdio::null());
        command
    }

    fn spawn(&self, command: &mut Command, label: &str) -> ChildGuard {
        let stdout = self.path.join(format!("{label}.stdout"));
        let stderr = self.path.join(format!("{label}.stderr"));
        let child = command
            .stdout(Stdio::from(fs::File::create(&stdout).unwrap()))
            .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
            .spawn()
            .expect("CLI subprocess should spawn");
        ChildGuard {
            child,
            stdout,
            stderr,
        }
    }

    fn start(
        &self,
        index: &Path,
        endpoint: &str,
        interval: Option<u64>,
        label: &str,
    ) -> ChildGuard {
        let mut command = self.command(&self.state);
        command
            .args(["service", "start"])
            .arg(index)
            .args(["--endpoint", endpoint]);
        if let Some(interval) = interval {
            command.args(["--auto-refresh-seconds", &interval.to_string()]);
        }
        self.spawn(&mut command, label)
    }

    fn hidden(&self, interval: Option<u64>) -> ChildGuard {
        let mut command = self.command(&self.state);
        command
            .arg("service-run")
            .arg(&self.index)
            .arg(&self.endpoint);
        if let Some(interval) = interval {
            command.args(["--auto-refresh-seconds", &interval.to_string()]);
        }
        self.spawn(&mut command, "managed-child")
    }

    fn other_endpoint(&self) -> String {
        if cfg!(windows) {
            format!("{}-other", self.endpoint)
        } else {
            self.path.join("other.sock").to_string_lossy().into_owned()
        }
    }

    async fn publish_owned_state(
        &self,
        child: &mut ChildGuard,
        interval: Option<u64>,
    ) -> ServiceState {
        let response = rpc(&self.endpoint, PING).await;
        assert_eq!(response["result"]["status"], "ok");
        let identity = &response["result"]["service"];
        assert_eq!(identity["pid"], child.child.id());
        assert_eq!(
            identity["index_path"],
            json!(fs::canonicalize(&self.index).unwrap())
        );
        assert_eq!(identity["endpoint"], self.endpoint);
        assert_eq!(identity["auto_refresh_seconds"], json!(interval));
        assert!(child.child.try_wait().unwrap().is_none());
        assert!(ServiceCoordination::instance_active(&self.state).unwrap());
        let state = ServiceState {
            endpoint: self.endpoint.clone(),
            pid: child.child.id(),
            index_path: fs::canonicalize(&self.index).unwrap(),
            started_unix_seconds: identity["started_unix_seconds"]
                .as_u64()
                .expect("managed identity must include the actual child start time"),
            auto_refresh_seconds: interval,
            instance_id: Some(
                identity["instance_id"]
                    .as_str()
                    .filter(|generation| !generation.is_empty())
                    .expect("managed identity must include a nonempty child generation")
                    .to_owned(),
            ),
        };
        write_state(&self.state, &state).unwrap();
        state
    }

    async fn assert_owned_running(&self, child: &mut ChildGuard) {
        assert!(child.child.try_wait().unwrap().is_none());
        assert_eq!(
            rpc(&self.endpoint, PING).await["result"]["service"]["pid"],
            child.child.id()
        );
        let mut status = self.cli(&["service", "status", "--json"], "healthy-status");
        let output = status.output().await;
        assert!(output.status.success(), "{output:?}");
        let status = output_json(&output);
        assert_eq!(status["status"], "running");
        assert_eq!(status["pid"], child.child.id());
    }

    async fn stop_owned(&self, child: &mut ChildGuard) {
        let mut stop = self.cli(&["service", "stop"], "owned-stop");
        assert_text_status(&stop.output().await, "stopped", 0);
        let output = child.output().await;
        assert!(output.status.success(), "{output:?}");
        assert!(!self.state.exists());
        assert!(!ServiceCoordination::instance_active(&self.state).unwrap());
        assert!(IndexWriterGuard::acquire(&self.index).is_ok());
        assert_endpoint_closed(&self.endpoint).await;
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
