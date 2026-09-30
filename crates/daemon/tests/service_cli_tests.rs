use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use ai_file_search_daemon::service::{SERVICE_STATE_ENV, ServiceState, read_state, write_state};
use ai_file_search_daemon::{run_with_state_path, send_ipc_request};
use ai_file_search_indexer::{FileIndexStore, FileIndexWriter, IndexWriterGuard, ScanOptions};

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
    let fixture =
        TestDir::new("service_status_json_reports_stale_when_state_endpoint_is_unreachable");
    let state_path = fixture.path().join("service-state.json");
    write_state(
        &state_path,
        &ServiceState {
            endpoint: "aifs-unreachable-service-cli-test".to_owned(),
            pid: 99,
            index_path: fixture.path().join("index.txt"),
            started_unix_seconds: 1_782_281_286,
            auto_refresh_seconds: None,
        },
    )
    .expect("state should write");

    let result = run_with_state(&state_path, ["service", "status", "--json"]);

    assert_eq!(result.exit_code, 1);
    assert!(result.stdout.contains("\"status\":\"stale\""));
    assert!(
        result
            .stdout
            .contains("\"endpoint\":\"aifs-unreachable-service-cli-test\"")
    );
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

#[test]
fn service_start_requires_index_root_metadata() {
    let fixture = TestDir::new("service_start_requires_index_root_metadata");
    let state_path = fixture.path().join("service-state.json");
    let index_path = fixture.path().join("index.txt");
    let mut guard = IndexWriterGuard::acquire(&index_path).expect("writer should acquire");
    FileIndexWriter::new(&mut guard)
        .save()
        .expect("store should save without root metadata");
    drop(guard);

    let result = run_with_state(
        &state_path,
        [
            "service",
            "start",
            index_path.to_str().expect("index path should be UTF-8"),
        ],
    );

    assert_eq!(result.exit_code, 1);
    assert_eq!(result.stdout, "");
    assert_eq!(
        result.stderr,
        "index root metadata missing: run ai-file-search index <root> <index-file>\n"
    );
}

#[test]
fn service_start_accepts_auto_refresh_seconds() {
    let fixture = TestDir::new("service_start_accepts_auto_refresh_seconds");
    let state_path = fixture.path().join("service-state.json");
    let result = run_with_state(
        &state_path,
        [
            "service",
            "start",
            "missing-index.json",
            "--auto-refresh-seconds",
            "300",
        ],
    );

    assert_eq!(result.exit_code, 1);
    assert!(result.stderr.starts_with("index path resolve failed:"));
}

#[test]
fn service_start_accepts_auto_refresh_and_endpoint_in_either_order() {
    let fixture = TestDir::new("service_start_accepts_auto_refresh_and_endpoint_in_either_order");
    let state_path = fixture.path().join("service-state.json");

    for args in [
        [
            "service",
            "start",
            "missing-index.json",
            "--endpoint",
            "aifs-test",
            "--auto-refresh-seconds",
            "300",
        ],
        [
            "service",
            "start",
            "missing-index.json",
            "--auto-refresh-seconds",
            "300",
            "--endpoint",
            "aifs-test",
        ],
    ] {
        let result = run_with_state(&state_path, args);

        assert_eq!(result.exit_code, 1);
        assert!(result.stderr.starts_with("index path resolve failed:"));
    }
}

#[test]
fn service_start_rejects_invalid_auto_refresh_seconds() {
    let fixture = TestDir::new("service_start_rejects_invalid_auto_refresh_seconds");
    let state_path = fixture.path().join("service-state.json");

    for args in [
        [
            "service",
            "start",
            "missing-index.json",
            "--auto-refresh-seconds",
            "29",
            "",
            "",
        ],
        [
            "service",
            "start",
            "missing-index.json",
            "--auto-refresh-seconds",
            "86401",
            "",
            "",
        ],
        [
            "service",
            "start",
            "missing-index.json",
            "--auto-refresh-seconds",
            "nope",
            "",
            "",
        ],
        [
            "service",
            "start",
            "missing-index.json",
            "--auto-refresh-seconds",
            "",
            "",
            "",
        ],
        [
            "service",
            "start",
            "missing-index.json",
            "--auto-refresh-seconds",
            "300",
            "--auto-refresh-seconds",
            "301",
        ],
        [
            "service",
            "start",
            "missing-index.json",
            "--unknown",
            "value",
            "",
            "",
        ],
    ] {
        let args = args
            .into_iter()
            .filter(|arg| !arg.is_empty())
            .collect::<Vec<_>>();
        let result = run_with_state_path(args, &state_path);

        assert_eq!(result.exit_code, 2);
        assert_eq!(result.stdout, "");
        assert!(result.stderr.starts_with("usage: ai-file-search-daemon "));
    }
}

#[tokio::test]
async fn service_start_spawns_service_and_persists_auto_refresh_seconds() {
    let fixture = TestDir::new("service_start_spawns_service_and_persists_auto_refresh_seconds");
    let state_path = fixture.path().join("service-state.json");
    let root = fixture.path().join("root");
    let index_path = fixture.path().join("index.txt");
    let endpoint = service_start_endpoint(&fixture);
    fs::create_dir_all(&root).expect("root directory should be created");

    let mut writer_guard = IndexWriterGuard::acquire(&index_path).expect("writer should acquire");
    let mut store = FileIndexWriter::new(&mut writer_guard);
    store.set_root_path(&root);
    store.set_scan_policy(ScanOptions::default());
    store.save().expect("store should save with root metadata");
    drop(store);
    drop(writer_guard);

    let mut guard = ServiceStopGuard::new(&state_path);
    let start = Command::new(daemon_binary())
        .env(SERVICE_STATE_ENV, &state_path)
        .args([
            "service",
            "start",
            index_path.to_str().expect("index path should be UTF-8"),
            "--endpoint",
            &endpoint,
            "--auto-refresh-seconds",
            "300",
        ])
        .status()
        .expect("service start should run");

    assert!(start.success());

    let state = read_state(&state_path)
        .expect("state should be readable")
        .expect("service start should write state");
    assert_eq!(state.endpoint, endpoint);
    assert_eq!(
        state.index_path,
        fs::canonicalize(&index_path).expect("index should resolve")
    );
    assert_eq!(state.auto_refresh_seconds, Some(300));

    let response = send_ipc_request(&endpoint, r#"{"id":1,"method":"ping","params":{}}"#)
        .await
        .expect("spawned service should accept IPC requests");
    assert!(response.contains(r#""status":"ok""#));

    let stop = Command::new(daemon_binary())
        .env(SERVICE_STATE_ENV, &state_path)
        .args(["service", "stop"])
        .output()
        .expect("service stop should run");
    assert!(stop.status.success());
    assert_eq!(stop.stdout, b"stopped\n");
    assert!(stop.stderr.is_empty());
    assert!(!state_path.exists());
    assert_endpoint_stays_closed(&endpoint).await;
    guard.disarm();
}

#[tokio::test]
async fn hidden_service_run_accepts_auto_refresh_seconds_and_serves_shutdown() {
    let fixture =
        TestDir::new("hidden_service_run_accepts_auto_refresh_seconds_and_serves_shutdown");
    let index_path = fixture.path().join("index.txt");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).expect("root directory should be created");
    save_service_index(&index_path, &root, true);
    let endpoint = service_run_endpoint(&fixture);
    let mut child = ChildGuard::spawn(
        Command::new(daemon_binary())
            .arg("service-run")
            .arg(&index_path)
            .arg(&endpoint)
            .arg("--auto-refresh-seconds")
            .arg("300")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );

    let response = shutdown_when_ready(&endpoint).await;

    assert!(response.contains(r#""status":"shutting_down""#));
    assert!(wait_for_exit(&mut child.0).await.success());
}

#[test]
fn hidden_service_run_rejects_invalid_auto_refresh_arguments() {
    for args in [
        [
            "service-run",
            "index.json",
            "endpoint",
            "--auto-refresh-seconds",
            "29",
            "",
            "",
        ],
        [
            "service-run",
            "index.json",
            "endpoint",
            "--auto-refresh-seconds",
            "86401",
            "",
            "",
        ],
        [
            "service-run",
            "index.json",
            "endpoint",
            "--auto-refresh-seconds",
            "nope",
            "",
            "",
        ],
        [
            "service-run",
            "index.json",
            "endpoint",
            "--auto-refresh-seconds",
            "",
            "",
            "",
        ],
        [
            "service-run",
            "index.json",
            "endpoint",
            "--auto-refresh-seconds",
            "300",
            "--auto-refresh-seconds",
            "301",
        ],
        [
            "service-run",
            "index.json",
            "endpoint",
            "--unknown",
            "value",
            "",
            "",
        ],
    ] {
        let output = Command::new(daemon_binary())
            .args(args.into_iter().filter(|arg| !arg.is_empty()))
            .output()
            .expect("hidden command should run");

        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&output.stderr).starts_with("usage: ai-file-search-daemon ")
        );
    }
}

#[tokio::test]
async fn service_scan_policy_configured_start_rejects_legacy_unknown_before_spawn() {
    let fixture = TestDir::new("configured-start-unknown-policy");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).unwrap();
    assert_configured_start_rejected(&fixture, &root, false, "index scan policy missing:").await;
}

#[tokio::test]
async fn service_scan_policy_configured_start_rejects_relative_root_before_spawn() {
    let fixture = TestDir::new("configured-start-relative-root");
    assert_configured_start_rejected(
        &fixture,
        Path::new("relative-root"),
        true,
        "index root must be absolute",
    )
    .await;
}

#[tokio::test]
async fn service_scan_policy_configured_start_rejects_unresolvable_root_before_spawn() {
    let fixture = TestDir::new("configured-start-unresolvable-root");
    let root = fixture.path().join("missing-root");
    assert_configured_start_rejected(&fixture, &root, true, "index root resolve failed:").await;
}

#[tokio::test]
async fn service_scan_policy_configured_start_rejects_file_root_before_spawn() {
    let fixture = TestDir::new("configured-start-file-root");
    let root = fixture.path().join("root.txt");
    fs::write(&root, "not a directory").unwrap();
    assert_configured_start_rejected(&fixture, &root, true, "index root is not a directory").await;
}

async fn assert_configured_start_rejected(
    fixture: &TestDir,
    root: &Path,
    known_policy: bool,
    error: &str,
) {
    let state_path = fixture.path().join("service-state.json");
    let index_path = fixture.path().join("index.txt");
    save_service_index(&index_path, root, known_policy);
    let before = fs::read(&index_path).unwrap();
    let endpoint = service_start_endpoint(fixture);
    let _guard = ServiceStopGuard::new(&state_path);
    let stdout_path = fixture.path().join("start.stdout");
    let stderr_path = fixture.path().join("start.stderr");
    let status = Command::new(daemon_binary())
        .env(SERVICE_STATE_ENV, &state_path)
        .args(["service", "start"])
        .arg(&index_path)
        .args(["--endpoint", &endpoint, "--auto-refresh-seconds", "300"])
        .stdout(Stdio::from(fs::File::create(&stdout_path).unwrap()))
        .stderr(Stdio::from(fs::File::create(&stderr_path).unwrap()))
        .status()
        .expect("configured start should run");

    assert_eq!(status.code(), Some(1));
    assert!(fs::read(&stdout_path).unwrap().is_empty());
    let stderr = fs::read_to_string(&stderr_path).unwrap();
    assert!(
        stderr.starts_with(error),
        "unexpected startup error: {stderr}"
    );
    if !known_policy {
        assert!(stderr.contains("ai-file-search index"));
        assert!(stderr.contains("--exclude-name"));
    }
    assert!(!state_path.exists(), "rejected start must not write state");
    assert_eq!(fs::read(&index_path).unwrap(), before);
    assert_endpoint_stays_closed(&endpoint).await;
}

#[tokio::test]
async fn service_scan_policy_no_auto_start_keeps_legacy_root_metadata_behavior() {
    for root_kind in ["relative", "unresolvable"] {
        let fixture = TestDir::new(&format!("no-auto-start-{root_kind}"));
        let root = if root_kind == "relative" {
            PathBuf::from("legacy-relative-root")
        } else {
            fixture.path().join("missing-root")
        };
        let state_path = fixture.path().join("service-state.json");
        let index_path = fixture.path().join("index.txt");
        save_service_index(&index_path, &root, false);
        let before = fs::read(&index_path).unwrap();
        let endpoint = service_start_endpoint(&fixture);
        let mut guard = ServiceStopGuard::new(&state_path);
        let status = Command::new(daemon_binary())
            .env(SERVICE_STATE_ENV, &state_path)
            .args(["service", "start"])
            .arg(&index_path)
            .args(["--endpoint", &endpoint])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(
            send_ipc_request(&endpoint, r#"{"id":1,"method":"ping","params":{}}"#)
                .await
                .unwrap()
                .contains(r#""status":"ok""#)
        );
        assert_eq!(
            read_state(&state_path)
                .unwrap()
                .unwrap()
                .auto_refresh_seconds,
            None
        );
        assert!(
            FileIndexStore::open(&index_path)
                .unwrap()
                .scan_policy()
                .is_none()
        );
        assert_eq!(fs::read(&index_path).unwrap(), before);
        let stop = Command::new(daemon_binary())
            .env(SERVICE_STATE_ENV, &state_path)
            .args(["service", "stop"])
            .output()
            .unwrap();
        assert!(stop.status.success());
        assert!(!state_path.exists());
        assert_endpoint_stays_closed(&endpoint).await;
        guard.disarm();
    }
}

#[tokio::test]
async fn service_scan_policy_hidden_configured_child_rejects_unknown_policy_before_bind() {
    let fixture = TestDir::new("hidden-unknown-policy");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).unwrap();
    assert_hidden_configured_child_rejected(&fixture, &root, false, "index scan policy missing:")
        .await;
}

#[tokio::test]
async fn service_scan_policy_hidden_configured_child_rejects_invalid_roots_before_bind() {
    for root_kind in ["relative", "unresolvable", "file"] {
        let fixture = TestDir::new(&format!("hidden-invalid-root-{root_kind}"));
        let (root, error) = match root_kind {
            "relative" => (
                PathBuf::from("relative-root"),
                "index root must be absolute",
            ),
            "unresolvable" => (
                fixture.path().join("missing-root"),
                "index root resolve failed:",
            ),
            _ => {
                let root = fixture.path().join("root.txt");
                fs::write(&root, "not a directory").unwrap();
                (root, "index root is not a directory")
            }
        };
        assert_hidden_configured_child_rejected(&fixture, &root, true, error).await;
    }
}

async fn assert_hidden_configured_child_rejected(
    fixture: &TestDir,
    root: &Path,
    known_policy: bool,
    error: &str,
) {
    let index_path = fixture.path().join("index.txt");
    let state_path = fixture.path().join("service-state.json");
    save_service_index(&index_path, root, known_policy);
    let before = fs::read(&index_path).unwrap();
    let endpoint = service_run_endpoint(fixture);
    let mut child = ChildGuard::spawn(
        Command::new(daemon_binary())
            .env(SERVICE_STATE_ENV, &state_path)
            .arg("service-run")
            .arg(&index_path)
            .arg(&endpoint)
            .args(["--auto-refresh-seconds", "300"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );

    let status = wait_for_exit(&mut child.0).await;
    assert_eq!(status.code(), Some(1));
    let mut stderr = String::new();
    child
        .0
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(
        stderr.starts_with(error),
        "unexpected child error: {stderr}"
    );
    assert!(!state_path.exists());
    assert_eq!(fs::read(&index_path).unwrap(), before);
    assert_endpoint_stays_closed(&endpoint).await;
}

#[tokio::test]
async fn service_scan_policy_hidden_no_auto_keeps_legacy_behavior() {
    let fixture = TestDir::new("hidden-no-auto-legacy");
    let index_path = fixture.path().join("index.txt");
    save_service_index(&index_path, Path::new("legacy-relative-root"), false);
    let endpoint = service_run_endpoint(&fixture);
    let mut child = ChildGuard::spawn(
        Command::new(daemon_binary())
            .arg("service-run")
            .arg(&index_path)
            .arg(&endpoint)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    assert!(
        shutdown_when_ready(&endpoint)
            .await
            .contains(r#""status":"shutting_down""#)
    );
    assert!(wait_for_exit(&mut child.0).await.success());
    assert_endpoint_stays_closed(&endpoint).await;
}

fn save_service_index(index_path: &Path, root: &Path, known_policy: bool) {
    let mut guard = IndexWriterGuard::acquire(index_path).unwrap();
    let mut store = FileIndexWriter::new(&mut guard);
    store.set_root_path(root);
    if known_policy {
        store.set_scan_policy(ScanOptions::default());
    }
    store.save().unwrap();
}

fn daemon_binary() -> &'static str {
    env!("CARGO_BIN_EXE_ai-file-search-daemon")
}

#[cfg(windows)]
fn service_run_endpoint(fixture: &TestDir) -> String {
    service_start_endpoint(fixture)
}

#[cfg(unix)]
fn service_run_endpoint(fixture: &TestDir) -> String {
    fixture
        .path()
        .join("service.sock")
        .to_string_lossy()
        .into_owned()
}

#[cfg(windows)]
fn service_start_endpoint(_fixture: &TestDir) -> String {
    format!(
        "aifs-service-start-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos()
    )
}

#[cfg(unix)]
fn service_start_endpoint(fixture: &TestDir) -> String {
    fixture
        .path()
        .join("service-start.sock")
        .to_string_lossy()
        .into_owned()
}

async fn shutdown_when_ready(endpoint: &str) -> String {
    for _ in 0..50 {
        if let Ok(response) =
            send_ipc_request(endpoint, r#"{"id":1,"method":"shutdown","params":{}}"#).await
        {
            return response;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("hidden service-run did not accept IPC requests");
}

async fn wait_for_exit(child: &mut Child) -> ExitStatus {
    for _ in 0..50 {
        if let Some(status) = child.try_wait().expect("child status should be readable") {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    panic!("hidden service-run did not exit after shutdown");
}

async fn assert_endpoint_stays_closed(endpoint: &str) {
    for _ in 0..20 {
        if let Ok(response) =
            send_ipc_request(endpoint, r#"{"id":1,"method":"ping","params":{}}"#).await
        {
            panic!("service accepted a request after stop: {response}");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct ChildGuard(Child);

impl ChildGuard {
    fn spawn(command: &mut Command) -> Self {
        Self(command.spawn().expect("hidden command should start"))
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct ServiceStopGuard {
    state_path: PathBuf,
    armed: bool,
}

impl ServiceStopGuard {
    fn new(state_path: &Path) -> Self {
        Self {
            state_path: state_path.to_path_buf(),
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for ServiceStopGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = Command::new(daemon_binary())
                .env(SERVICE_STATE_ENV, &self.state_path)
                .args(["service", "stop"])
                .output();
        }
    }
}

fn run_with_state<const N: usize>(
    state_path: &Path,
    args: [&str; N],
) -> ai_file_search_daemon::CliResult {
    run_with_state_path(args, state_path)
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ai-file-search-service-cli-{name}-{}",
            std::process::id()
        ));

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
