use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ai_file_search_daemon::service::{SERVICE_STATE_ENV, ServiceState, write_state};
use ai_file_search_daemon::{
    handle_json_line, handle_json_stream, send_ipc_request, send_json_request,
};
use ai_file_search_indexer::{FileIndexStore, FileIndexWriter, IndexWriterGuard, ScanOptions};

const PING: &str = r#"{"id":1,"method":"ping","params":{}}"#;
const REFRESH: &str = r#"{"id":2,"method":"refresh","params":{}}"#;
const REINDEX: &str = r#"{"id":3,"method":"reindex","params":{}}"#;
const STATS: &str = r#"{"id":4,"method":"stats","params":{}}"#;
const SEARCH: &str = r#"{"id":5,"method":"search","params":{"query":"added"}}"#;

#[tokio::test]
async fn managed_service_owns_index_and_reuses_guard_for_refresh() {
    let fixture = Fixture::new("managed");
    let endpoint = fixture.endpoint("owner");
    let mut owner = fixture.spawn_server("service-run", &endpoint);
    wait_ready(&endpoint).await;

    assert_writer_busy(&fixture.index);
    let before = fs::read(&fixture.index).unwrap();
    fs::write(fixture.root.join("added.txt"), "added").unwrap();
    assert_standalone_writes_busy(&fixture, &before).await;
    assert_readers_work(&fixture, &endpoint).await;

    let same_state = fixture.path.join("same-state.json");
    write_state(
        &same_state,
        &ServiceState {
            endpoint: endpoint.clone(),
            pid: owner.0.id(),
            index_path: fixture.index.clone(),
            started_unix_seconds: 1,
            auto_refresh_seconds: None,
        },
    )
    .unwrap();
    let already_running = fixture
        .command()
        .env(SERVICE_STATE_ENV, &same_state)
        .args(["service", "start"])
        .arg(&fixture.index)
        .output()
        .unwrap();
    assert!(already_running.status.success());
    assert!(String::from_utf8_lossy(&already_running.stdout).starts_with("running endpoint="));

    assert_other_services_busy(&fixture).await;
    assert!(request(&endpoint, PING).await.contains("\"status\":\"ok\""));

    let reserved = fixture.root.join(".index.txt.aifs-tmp-abandoned");
    fs::write(&reserved, "reserved").unwrap();
    fs::write(fixture.root.join("keep.tmp"), "ordinary temp file").unwrap();
    assert!(
        request(&endpoint, REFRESH)
            .await
            .contains("\"scanned_files\":2")
    );
    assert!(
        request(&endpoint, REINDEX)
            .await
            .contains("\"unchanged\":2")
    );
    let store = FileIndexStore::open(&fixture.index).unwrap();
    assert_eq!(store.file_count(), 2);
    assert_eq!(store.search_by_name("keep.tmp").len(), 1);
    assert!(store.search_by_name("index.txt").is_empty());
    assert_eq!(fs::read(&reserved).unwrap(), b"reserved");

    let state_path = fixture.path.join("owner-state.json");
    write_state(
        &state_path,
        &ServiceState {
            endpoint: endpoint.clone(),
            pid: owner.0.id(),
            index_path: fixture.index.clone(),
            started_unix_seconds: 1,
            auto_refresh_seconds: None,
        },
    )
    .unwrap();
    let stop = fixture
        .command()
        .env(SERVICE_STATE_ENV, &state_path)
        .args(["service", "stop"])
        .output()
        .unwrap();
    assert!(
        stop.status.success(),
        "{}",
        String::from_utf8_lossy(&stop.stderr)
    );
    assert_eq!(stop.stdout, b"stopped\n");
    assert!(wait_exit(&mut owner.0).await.success());
    assert!(!state_path.exists());
    assert_closed(&endpoint).await;
    assert_writer_free(&fixture.index);
}

#[tokio::test]
async fn public_ipc_owns_index_and_forced_owned_child_kill_releases_lock() {
    let fixture = Fixture::new("manual-ipc");
    let endpoint = fixture.endpoint("ipc");
    let mut owner = fixture.spawn_server("ipc", &endpoint);
    wait_ready(&endpoint).await;
    assert_writer_busy(&fixture.index);
    fs::write(fixture.root.join("added.txt"), "added").unwrap();
    assert!(request(&endpoint, REFRESH).await.contains("\"added\":1"));
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    assert_closed(&endpoint).await;
    assert_writer_free(&fixture.index);
}

async fn assert_other_services_busy(fixture: &Fixture) {
    let endpoint = fixture.endpoint("second");
    let mut second = fixture.spawn_server("service-run", &endpoint);
    let started = Instant::now();
    assert!(!wait_exit(&mut second.0).await.success());
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(second.stderr().contains("index is busy"));
    assert_closed(&endpoint).await;

    let state = fixture.path.join("second-state.json");
    let started = Instant::now();
    let output = fixture
        .command()
        .env(SERVICE_STATE_ENV, &state)
        .args(["service", "start"])
        .arg(&fixture.index)
        .args(["--endpoint", &endpoint])
        .output()
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("index is busy"));
    assert!(!state.exists());
    assert_closed(&endpoint).await;
}

#[tokio::test]
async fn failed_start_state_write_reaps_owned_service_child() {
    let fixture = Fixture::new("failed-state-write");
    let endpoint = fixture.endpoint("failed-start");
    let state_path = fixture.path.join("state-is-directory");
    fs::create_dir(&state_path).unwrap();
    let _cleanup = EndpointCleanup(endpoint.clone());
    let output = fixture
        .command()
        .env(SERVICE_STATE_ENV, &state_path)
        .args(["service", "start"])
        .arg(&fixture.index)
        .args(["--endpoint", &endpoint])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("service state write failed"));
    assert_closed(&endpoint).await;
    assert_writer_free(&fixture.index);
    assert!(
        state_path.is_dir(),
        "foreign state directory must be preserved"
    );
}

#[tokio::test]
async fn standalone_stream_and_stdio_release_lock_between_writes() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let fixture = Fixture::new("per-write");
    fs::write(fixture.root.join("added.txt"), "added").unwrap();
    let (client, server) = tokio::io::duplex(4096);
    let index = fixture.index.clone();
    let handler = tokio::spawn(async move { handle_json_stream(&index, server).await.unwrap() });
    let mut client = BufReader::new(client);
    for (line, expected) in [(REFRESH, "\"added\":1"), (REINDEX, "\"unchanged\":1")] {
        client
            .get_mut()
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        client.read_line(&mut response).await.unwrap();
        assert!(
            response.contains(expected),
            "unexpected response: {response}"
        );
        assert_writer_free(&fixture.index);
    }
    drop(client);
    handler.await.unwrap();

    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_ai-file-search-daemon"))
        .arg("stdio")
        .arg(&fixture.index)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    for line in [REFRESH, REINDEX, STATS] {
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        stdout.read_line(&mut response).await.unwrap();
        assert!(
            response.contains("\"result\""),
            "unexpected response: {response}"
        );
        assert_writer_free(&fixture.index);
    }
    drop(stdin);
    assert!(child.wait().await.unwrap().success());
}

#[tokio::test]
async fn caller_process_lock_blocks_standalone_writes_before_snapshot_open() {
    let fixture = Fixture::new("caller-lock");
    let ready = fixture.path.join("lock-ready");
    let mut owner = OwnedChild::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "caller_lock_process", "--ignored", "--nocapture"])
            .env("AIFS_TEST_LOCK_INDEX", &fixture.index)
            .env("AIFS_TEST_LOCK_READY", &ready)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while !ready.exists() {
        assert!(
            Instant::now() < deadline,
            "lock helper did not become ready"
        );
        assert!(owner.0.try_wait().unwrap().is_none());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fs::write(
        &fixture.index,
        "invalid snapshot deliberately unreadable by parser\n",
    )
    .unwrap();
    let before = fs::read(&fixture.index).unwrap();
    assert_standalone_writes_busy(&fixture, &before).await;
    let configured_endpoint = fixture.endpoint("configured-child");
    let mut configured_child = OwnedChild::spawn(
        fixture
            .command()
            .arg("service-run")
            .arg(&fixture.index)
            .arg(&configured_endpoint)
            .args(["--auto-refresh-seconds", "300"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped()),
    );
    assert!(!wait_exit(&mut configured_child.0).await.success());
    assert!(configured_child.stderr().contains("index is busy"));
    assert_closed(&configured_endpoint).await;
    let state = fixture.path.join("service-state.json");
    let endpoint = fixture.endpoint("blocked");
    let output = fixture
        .command()
        .env(SERVICE_STATE_ENV, &state)
        .args(["service", "start"])
        .arg(&fixture.index)
        .args(["--endpoint", &endpoint, "--auto-refresh-seconds", "300"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("index is busy"));
    assert!(!state.exists());
    assert_closed(&endpoint).await;
    owner.0.stdin.take().unwrap().write_all(b"release").unwrap();
    assert!(wait_exit(&mut owner.0).await.success());
    assert_writer_free(&fixture.index);
}

#[test]
#[ignore = "subprocess lock holder"]
fn caller_lock_process() {
    let index = PathBuf::from(std::env::var_os("AIFS_TEST_LOCK_INDEX").unwrap());
    let lock = IndexWriterGuard::acquire(&index).unwrap();
    fs::write(
        PathBuf::from(std::env::var_os("AIFS_TEST_LOCK_READY").unwrap()),
        b"ready",
    )
    .unwrap();
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    drop(lock);
}

async fn assert_standalone_writes_busy(fixture: &Fixture, before: &[u8]) {
    for request in [REFRESH, REINDEX] {
        let response = handle_json_line(&fixture.index, request).to_json_line();
        assert!(
            response.contains("index is busy"),
            "unexpected response: {response}"
        );
        assert_eq!(fs::read(&fixture.index).unwrap(), before);

        let output = fixture
            .command()
            .arg("handle")
            .arg(&fixture.index)
            .arg(request)
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("index is busy"));
        assert_eq!(fs::read(&fixture.index).unwrap(), before);
    }
    let mut stdio = fixture
        .command()
        .arg("stdio")
        .arg(&fixture.index)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(stdio.stdin.take().unwrap(), "{REFRESH}\n{REINDEX}\n{STATS}").unwrap();
    let output = stdio.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .matches("index is busy")
            .count(),
        2
    );
    assert_eq!(fs::read(&fixture.index).unwrap(), before);

    let (client, server) = tokio::io::duplex(4096);
    let (response, status) = tokio::join!(
        send_json_request(client, REFRESH),
        handle_json_stream(&fixture.index, server)
    );
    assert!(response.unwrap().contains("index is busy"));
    status.unwrap();
    assert_eq!(fs::read(&fixture.index).unwrap(), before);
}

async fn assert_readers_work(fixture: &Fixture, endpoint: &str) {
    for method in [STATS, SEARCH] {
        assert!(
            handle_json_line(&fixture.index, method)
                .to_json_line()
                .contains("\"result\"")
        );
        assert!(request(endpoint, method).await.contains("\"result\""));
    }
}

fn assert_writer_busy(index: &Path) {
    match IndexWriterGuard::acquire(index) {
        Ok(_) => panic!("service must own the adjacent writer lock"),
        Err(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            assert!(error.to_string().contains("index is busy"));
        }
    }
}

fn assert_writer_free(index: &Path) {
    let lock = IndexWriterGuard::acquire(index).expect("writer lock must release after owner exit");
    assert!(lock.lock_path().exists());
    assert!(
        index.with_file_name("index.txt.lock").exists(),
        "persistent lock must not be unlinked"
    );
}

async fn request(endpoint: &str, line: &str) -> String {
    for _ in 0..100 {
        if let Ok(response) =
            tokio::time::timeout(Duration::from_secs(2), send_ipc_request(endpoint, line))
                .await
                .unwrap()
        {
            return response;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("endpoint did not accept request: {line}");
}

async fn wait_ready(endpoint: &str) {
    assert!(request(endpoint, PING).await.contains("\"status\":\"ok\""));
}

async fn assert_closed(endpoint: &str) {
    for _ in 0..5 {
        assert!(send_ipc_request(endpoint, PING).await.is_err());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_exit(child: &mut Child) -> ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "owned child did not exit promptly"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

struct OwnedChild(Child);

impl OwnedChild {
    fn spawn(command: &mut Command) -> Self {
        Self(command.spawn().unwrap())
    }
    fn stderr(&mut self) -> String {
        let mut stderr = String::new();
        self.0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        stderr
    }
}

struct EndpointCleanup(String);

impl Drop for EndpointCleanup {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_ai-file-search-daemon"))
            .args([
                "ipc-request",
                &self.0,
                r#"{"id":1,"method":"shutdown","params":{}}"#,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    path: PathBuf,
    root: PathBuf,
    index: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("aifs-writer-{name}-{}-{nonce}", std::process::id()));
        let root = path.join("root");
        fs::create_dir_all(&root).unwrap();
        let index = root.join("index.txt");
        let mut guard = IndexWriterGuard::acquire(&index).unwrap();
        let mut store = FileIndexWriter::new(&mut guard);
        store.set_root_path(&root);
        store.set_scan_policy(ScanOptions::default());
        store.save().unwrap();
        Self { path, root, index }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-file-search-daemon"));
        command.current_dir(&self.path);
        command
    }
    fn spawn_server(&self, mode: &str, endpoint: &str) -> OwnedChild {
        OwnedChild::spawn(
            self.command()
                .arg(mode)
                .arg(&self.index)
                .arg(endpoint)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped()),
        )
    }
    fn endpoint(&self, name: &str) -> String {
        #[cfg(windows)]
        {
            format!(
                "{}-{name}",
                self.path.file_name().unwrap().to_string_lossy()
            )
        }
        #[cfg(unix)]
        {
            self.path
                .join(format!("{name}.sock"))
                .to_string_lossy()
                .into_owned()
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
