#![cfg(unix)]

use std::fs::{self, DirBuilder};
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_file_search_daemon::send_ipc_request;
use ai_file_search_daemon::service::{
    DEFAULT_ENDPOINT, SERVICE_STATE_ENV, ServiceState, write_state,
};
use ai_file_search_indexer::{FileIndexWriter, IndexWriterGuard, ScanOptions};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::{sleep, timeout};

const CHILD_BOUND: Duration = Duration::from_secs(10);
const RPC_BOUND: Duration = Duration::from_secs(2);
const PING: &str = r#"{"id":1,"method":"ping","params":{}}"#;
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn default_runtime_endpoint_is_private_and_owned_by_the_current_user() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(&fixture.state, &fixture.index, None, Runtime::Xdg);
    let state = child.ready().await;
    let endpoint = Path::new(&state.endpoint);
    assert_eq!(
        endpoint.parent().unwrap(),
        fixture.runtime.join("ai-file-search")
    );
    assert_ne!(endpoint, fixture.state.with_file_name("service.sock"));
    assert_private_endpoint(&fixture, &state);
    child.shutdown(&state).await;
    assert_socket_removed_and_marker_persistent(endpoint);
}

#[tokio::test]
async fn default_without_xdg_uses_a_private_uid_directory_in_child_local_temp() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(&fixture.state, &fixture.index, None, Runtime::Fallback);
    let state = child.ready().await;
    let endpoint = Path::new(&state.endpoint);
    assert_eq!(
        endpoint.parent().unwrap(),
        fixture.temp.join(format!("aifs-{}", fixture.uid))
    );
    assert_private_endpoint(&fixture, &state);
    child.shutdown(&state).await;
    assert_socket_removed_and_marker_persistent(endpoint);
}

#[tokio::test]
async fn default_endpoints_distinguish_state_owners_and_remain_stable_on_restart() {
    let fixture = Fixture::new();
    let other_state = fixture.path.join("other.json");
    let other_index = fixture.create_index("other.txt");
    let mut first = fixture.spawn(&fixture.state, &fixture.index, None, Runtime::Xdg);
    let first_state = first.ready().await;
    let mut second = fixture.spawn(&other_state, &other_index, None, Runtime::Xdg);
    let second_state = second.ready().await;
    assert_ne!(first_state.endpoint, second_state.endpoint);
    assert_private_endpoint(&fixture, &first_state);
    assert_private_endpoint(&fixture, &second_state);
    first.shutdown(&first_state).await;
    let mut restarted = fixture.spawn(&fixture.state, &fixture.index, None, Runtime::Xdg);
    let restarted_state = restarted.ready().await;
    assert_eq!(restarted_state.endpoint, first_state.endpoint);
    assert_ne!(restarted_state.instance_id, first_state.instance_id);
    restarted.shutdown(&restarted_state).await;
    second.shutdown(&second_state).await;
}

#[tokio::test]
async fn killed_service_run_recovers_only_its_exact_stale_socket_and_persistent_marker() {
    let fixture = Fixture::new();
    let requested = fixture.path.join("own.sock");
    let mut first = fixture.spawn(
        &fixture.state,
        &fixture.index,
        Some(&requested),
        Runtime::Xdg,
    );
    let old = first.ready().await;
    assert_private_endpoint(&fixture, &old);
    let endpoint = Path::new(&old.endpoint);
    let socket_before = fingerprint(endpoint);
    let marker = marker_path(endpoint);
    let marker_before = fingerprint(&marker);
    let marker_bytes = fs::read(&marker).unwrap();
    let index_before = fs::read(&fixture.index).unwrap();
    first.crash();
    assert_eq!(fingerprint(endpoint), socket_before);
    assert_eq!(fingerprint(&marker), marker_before);
    assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
    assert!(tokio::net::UnixStream::connect(endpoint).await.is_err());
    assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());

    let mut restarted = fixture.spawn(&fixture.state, &fixture.index, Some(endpoint), Runtime::Xdg);
    let new = restarted.ready().await;
    assert_eq!(new.endpoint, old.endpoint);
    assert_eq!(new.index_path, old.index_path);
    assert_ne!(new.instance_id, old.instance_id);
    assert_eq!(fs::read(&fixture.index).unwrap(), index_before);
    assert_eq!(fingerprint(&marker), marker_before);
    assert_private_endpoint(&fixture, &new);
    restarted.shutdown(&new).await;
    assert_socket_removed_and_marker_persistent(endpoint);
    assert_eq!(fingerprint(&marker), marker_before);
}

#[tokio::test]
async fn different_state_owners_cannot_take_over_a_live_or_stale_managed_endpoint() {
    let fixture = Fixture::new();
    let requested = fixture.path.join("own.sock");
    let other_state = fixture.path.join("other.json");
    let other_index = fixture.create_index("other.txt");
    let mut owner = fixture.spawn(
        &fixture.state,
        &fixture.index,
        Some(&requested),
        Runtime::Xdg,
    );
    let state = owner.ready().await;
    let endpoint = Path::new(&state.endpoint);
    let socket_before = fingerprint(endpoint);
    let marker = marker_path(endpoint);
    let marker_bytes = fs::read(&marker).unwrap();
    let state_bytes = fs::read(&fixture.state).unwrap();
    let other_index_bytes = fs::read(&other_index).unwrap();

    for stale in [false, true] {
        if stale {
            owner.crash();
        }
        let mut contender = fixture.spawn(&other_state, &other_index, Some(endpoint), Runtime::Xdg);
        contender.rejected().await;
        assert_eq!(fingerprint(endpoint), socket_before);
        assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
        assert_eq!(fs::read(&fixture.state).unwrap(), state_bytes);
        assert_eq!(fs::read(&other_index).unwrap(), other_index_bytes);
        assert!(!other_state.exists());
        if !stale {
            assert_eq!(
                rpc(&state.endpoint, PING).await["result"]["service"],
                json!(state)
            );
            assert!(owner.child.try_wait().unwrap().is_none());
        }
    }
    let mut recovered = fixture.spawn(&fixture.state, &fixture.index, Some(endpoint), Runtime::Xdg);
    let recovered_state = recovered.ready().await;
    recovered.shutdown(&recovered_state).await;
}

#[tokio::test]
async fn a_live_foreign_unix_socket_is_not_unlinked_or_taken_over() {
    check_foreign_socket(true).await;
}

#[tokio::test]
async fn a_stale_foreign_unix_socket_without_an_owned_marker_is_not_unlinked() {
    check_foreign_socket(false).await;
}

async fn check_foreign_socket(live: bool) {
    let fixture = Fixture::new();
    let endpoint = fixture.path.join("foreign.sock");
    let listener = UnixListener::bind(&endpoint).unwrap();
    let before = fingerprint(&endpoint);
    let listener = if live {
        Some(listener)
    } else {
        drop(listener);
        None
    };
    let mut child = fixture.spawn(
        &fixture.state,
        &fixture.index,
        Some(&endpoint),
        Runtime::Xdg,
    );
    child.rejected().await;
    assert_eq!(fingerprint(&endpoint), before);
    assert!(
        fs::symlink_metadata(&endpoint)
            .unwrap()
            .file_type()
            .is_socket()
    );
    assert!(!fixture.state.exists());
    if live {
        assert!(tokio::net::UnixStream::connect(&endpoint).await.is_ok());
    }
    drop(listener);
}

#[tokio::test]
async fn an_ordinary_file_at_the_endpoint_is_not_removed_or_modified() {
    let fixture = Fixture::new();
    let endpoint = fixture.path.join("foreign.sock");
    fs::write(&endpoint, b"user-owned ordinary file").unwrap();
    let before = fingerprint(&endpoint);
    let bytes = fs::read(&endpoint).unwrap();
    let mut child = fixture.spawn(
        &fixture.state,
        &fixture.index,
        Some(&endpoint),
        Runtime::Xdg,
    );
    child.rejected().await;
    assert_eq!(fingerprint(&endpoint), before);
    assert_eq!(fs::read(&endpoint).unwrap(), bytes);
    assert!(!fixture.state.exists());
}

#[tokio::test]
async fn an_endpoint_symlink_and_its_target_are_not_removed_or_modified() {
    let fixture = Fixture::new();
    let target = fixture.path.join("target.txt");
    let endpoint = fixture.path.join("foreign.sock");
    fs::write(&target, b"symlink target").unwrap();
    symlink(&target, &endpoint).unwrap();
    let link_before = fingerprint(&endpoint);
    let target_before = fingerprint(&target);
    let mut child = fixture.spawn(
        &fixture.state,
        &fixture.index,
        Some(&endpoint),
        Runtime::Xdg,
    );
    child.rejected().await;
    assert_eq!(fingerprint(&endpoint), link_before);
    assert_eq!(fs::read_link(&endpoint).unwrap(), target);
    assert_eq!(fingerprint(&target), target_before);
    assert_eq!(fs::read(&target).unwrap(), b"symlink target");
    assert!(!fixture.state.exists());
}

#[tokio::test]
async fn a_custom_endpoint_with_a_nonprivate_parent_is_rejected_without_chmod() {
    let fixture = Fixture::new();
    let parent = fixture.path.join("public");
    private_dir(&parent);
    let endpoint = parent.join("own.sock");
    for mode in [0o755, 0o750, 0o777] {
        fs::set_permissions(&parent, fs::Permissions::from_mode(mode)).unwrap();
        let before = fingerprint(&parent);
        let mut child = fixture.spawn(
            &fixture.state,
            &fixture.index,
            Some(&endpoint),
            Runtime::Xdg,
        );
        child.rejected().await;
        assert_eq!(fingerprint(&parent), before);
        assert!(!endpoint.exists());
        assert!(!fixture.state.exists());
    }
}

#[tokio::test]
async fn a_foreign_persistent_marker_cannot_authorize_stale_socket_recovery() {
    let fixture = Fixture::new();
    let endpoint = fixture.path.join("own.sock");
    let mut owner = fixture.spawn(
        &fixture.state,
        &fixture.index,
        Some(&endpoint),
        Runtime::Xdg,
    );
    let state = owner.ready().await;
    owner.crash();
    let endpoint = Path::new(&state.endpoint);
    let socket_before = fingerprint(endpoint);

    let other_state = fixture.path.join("other.json");
    let other_index = fixture.create_index("other.txt");
    let other_endpoint = fixture.path.join("other.sock");
    let mut other = fixture.spawn(
        &other_state,
        &other_index,
        Some(&other_endpoint),
        Runtime::Xdg,
    );
    let other_identity = other.ready().await;
    other.crash();
    let foreign_marker = fs::read(marker_path(Path::new(&other_identity.endpoint))).unwrap();
    let marker = marker_path(endpoint);
    assert_ne!(fs::read(&marker).unwrap(), foreign_marker);
    fs::write(&marker, &foreign_marker).unwrap();
    let marker_before = fingerprint(&marker);
    let mut rejected = fixture.spawn(&fixture.state, &fixture.index, Some(endpoint), Runtime::Xdg);
    rejected.rejected().await;
    assert_eq!(fingerprint(endpoint), socket_before);
    assert_eq!(fingerprint(&marker), marker_before);
    assert_eq!(fs::read(&marker).unwrap(), foreign_marker);
}

#[tokio::test]
async fn a_stale_marker_cannot_authorize_recovery_of_a_replaced_socket_inode() {
    let fixture = Fixture::new();
    let requested = fixture.path.join("own.sock");
    let mut owner = fixture.spawn(
        &fixture.state,
        &fixture.index,
        Some(&requested),
        Runtime::Xdg,
    );
    let state = owner.ready().await;
    owner.crash();
    let endpoint = Path::new(&state.endpoint);
    let old = fingerprint(endpoint);
    // Keep the old inode allocated so immediate inode reuse cannot mask replacement.
    fs::rename(endpoint, fixture.path.join("retired.sock")).unwrap();
    let foreign = UnixListener::bind(endpoint).unwrap();
    let replacement = fingerprint(endpoint);
    assert_ne!((old.0, old.1), (replacement.0, replacement.1));
    drop(foreign);
    let marker = marker_path(endpoint);
    let marker_bytes = fs::read(&marker).unwrap();
    let mut rejected = fixture.spawn(&fixture.state, &fixture.index, Some(endpoint), Runtime::Xdg);
    rejected.rejected().await;
    assert_eq!(fingerprint(endpoint), replacement);
    assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
}

#[tokio::test]
async fn graceful_cleanup_does_not_unlink_a_replacement_socket() {
    let fixture = Fixture::new();
    let requested = fixture.path.join("own.sock");
    let mut owner = fixture.spawn(
        &fixture.state,
        &fixture.index,
        Some(&requested),
        Runtime::Xdg,
    );
    let state = owner.ready().await;
    let endpoint = Path::new(&state.endpoint);
    let old = fingerprint(endpoint);
    let connection = tokio::net::UnixStream::connect(endpoint).await.unwrap();
    fs::rename(endpoint, fixture.path.join("retired.sock")).unwrap();
    let foreign = UnixListener::bind(endpoint).unwrap();
    let replacement = fingerprint(endpoint);
    assert_ne!((old.0, old.1), (replacement.0, replacement.1));
    let marker = marker_path(endpoint);
    let marker_bytes = fs::read(&marker).unwrap();

    let mut connection = BufReader::new(connection);
    let request = format!("{}\n", shutdown_request(&state));
    let mut response = String::new();
    timeout(RPC_BOUND, async {
        connection
            .get_mut()
            .write_all(request.as_bytes())
            .await
            .unwrap();
        connection.read_line(&mut response).await.unwrap();
    })
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&response).unwrap()["result"]["status"],
        "shutting_down"
    );
    assert!(owner.wait().await.success());
    assert_eq!(fingerprint(endpoint), replacement);
    assert_eq!(fs::read(&marker).unwrap(), marker_bytes);
    assert!(tokio::net::UnixStream::connect(endpoint).await.is_ok());
    drop(foreign);
}

fn fingerprint(path: &Path) -> (u64, u64, u32, u32) {
    let metadata = fs::symlink_metadata(path).unwrap();
    (
        metadata.dev(),
        metadata.ino(),
        metadata.mode() & 0o7777,
        metadata.uid(),
    )
}

fn marker_path(endpoint: &Path) -> PathBuf {
    endpoint.with_file_name(format!(
        ".{}.aifs-endpoint.lock",
        endpoint.file_name().unwrap().to_str().unwrap()
    ))
}

fn assert_private_endpoint(fixture: &Fixture, state: &ServiceState) {
    let endpoint = Path::new(&state.endpoint);
    assert!(
        state.endpoint.len() < 104,
        "socket path is too long: {}",
        state.endpoint
    );
    let parent = endpoint.parent().unwrap();
    assert_eq!(fs::canonicalize(parent).unwrap(), parent);
    assert_eq!(
        (fingerprint(parent).2, fingerprint(parent).3),
        (0o700, fixture.uid)
    );
    assert!(
        fs::symlink_metadata(endpoint)
            .unwrap()
            .file_type()
            .is_socket()
    );
    assert_eq!(
        (fingerprint(endpoint).2, fingerprint(endpoint).3),
        (0o600, fixture.uid)
    );
    let marker = marker_path(endpoint);
    assert!(fs::symlink_metadata(&marker).unwrap().file_type().is_file());
    assert_eq!(
        (fingerprint(&marker).2, fingerprint(&marker).3),
        (0o600, fixture.uid)
    );
    let record: Value = serde_json::from_slice(&fs::read(marker).unwrap()).unwrap();
    let socket = fs::symlink_metadata(endpoint).unwrap();
    assert_eq!(record["socket"]["device"], socket.dev());
    assert_eq!(record["socket"]["inode"], socket.ino());
    assert_eq!(record["socket"]["uid"], fixture.uid);
}

fn assert_socket_removed_and_marker_persistent(endpoint: &Path) {
    assert_eq!(
        fs::symlink_metadata(endpoint).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(marker_path(endpoint).is_file());
}

fn shutdown_request(state: &ServiceState) -> String {
    json!({"id":1,"method":"shutdown","params":{"service":state}}).to_string()
}

async fn rpc(endpoint: &str, request: &str) -> Value {
    let line = timeout(RPC_BOUND, send_ipc_request(endpoint, request))
        .await
        .unwrap()
        .unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 1, "{response}");
    assert!(response.get("error").is_none(), "{response}");
    response
}

#[derive(Clone, Copy)]
enum Runtime {
    Xdg,
    Fallback,
}

struct Fixture {
    path: PathBuf,
    runtime: PathBuf,
    temp: PathBuf,
    state: PathBuf,
    index: PathBuf,
    uid: u32,
}

fn private_dir(path: &Path) {
    DirBuilder::new().mode(0o700).create(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = PathBuf::from("/tmp").join(format!(
            "aifs-es-{}-{nonce:x}-{sequence:x}",
            std::process::id()
        ));
        private_dir(&path);
        let path = fs::canonicalize(path).unwrap();
        let runtime = path.join("run");
        let temp = path.join("tmp");
        private_dir(&runtime);
        private_dir(&temp);
        private_dir(&path.join("root"));
        let fixture = Self {
            state: path.join("state.json"),
            index: path.join("index.txt"),
            uid: fs::metadata(&path).unwrap().uid(),
            path,
            runtime,
            temp,
        };
        fixture.create_index("index.txt");
        fixture
    }

    fn create_index(&self, name: &str) -> PathBuf {
        let index = self.path.join(name);
        let mut guard = IndexWriterGuard::acquire(&index).unwrap();
        let mut writer = FileIndexWriter::new(&mut guard);
        writer.set_root_path(self.path.join("root"));
        writer.set_scan_policy(ScanOptions::default());
        writer.save().unwrap();
        index
    }

    fn spawn(
        &self,
        state: &Path,
        index: &Path,
        endpoint: Option<&Path>,
        runtime: Runtime,
    ) -> OwnedChild<'_> {
        let stderr = self.path.join(format!(
            "child-{}.stderr",
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-file-search-daemon"));
        command
            .current_dir(&self.path)
            .env(SERVICE_STATE_ENV, state)
            .env("TMPDIR", &self.temp)
            .env_remove("XDG_RUNTIME_DIR")
            .arg("service-run")
            .arg(index)
            .arg(endpoint.map_or_else(|| std::ffi::OsStr::new(DEFAULT_ENDPOINT), Path::as_os_str))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(fs::File::create(&stderr).unwrap()));
        if matches!(runtime, Runtime::Xdg) {
            command.env("XDG_RUNTIME_DIR", &self.runtime);
        }
        OwnedChild {
            child: command.spawn().unwrap(),
            stderr,
            state: state.to_path_buf(),
            index: index.to_path_buf(),
            requested: endpoint.map(Path::to_path_buf),
            discovery_root: match runtime {
                Runtime::Xdg => self.runtime.clone(),
                Runtime::Fallback => self.temp.clone(),
            },
            _fixture: self,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct OwnedChild<'a> {
    child: Child,
    stderr: PathBuf,
    state: PathBuf,
    index: PathBuf,
    requested: Option<PathBuf>,
    discovery_root: PathBuf,
    _fixture: &'a Fixture,
}

impl OwnedChild<'_> {
    async fn ready(&mut self) -> ServiceState {
        timeout(CHILD_BOUND, async {
            loop {
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "service exited: {}",
                    fs::read_to_string(&self.stderr).unwrap()
                );
                let candidates: Vec<PathBuf> = self.requested.as_ref().map_or_else(
                    || {
                        fs::read_dir(&self.discovery_root)
                            .into_iter()
                            .flatten()
                            .filter_map(Result::ok)
                            .flat_map(|entry| {
                                fs::read_dir(entry.path())
                                    .into_iter()
                                    .flatten()
                                    .filter_map(Result::ok)
                            })
                            .map(|entry| entry.path())
                            .filter(|path| {
                                fs::symlink_metadata(path)
                                    .is_ok_and(|metadata| metadata.file_type().is_socket())
                            })
                            .collect()
                    },
                    |path| vec![path.clone()],
                );
                for endpoint in candidates {
                    if let Ok(Ok(line)) = timeout(
                        RPC_BOUND,
                        send_ipc_request(endpoint.to_str().unwrap(), PING),
                    )
                    .await
                    {
                        let response: Value = serde_json::from_str(&line).unwrap();
                        let Ok(state) = serde_json::from_value::<ServiceState>(
                            response["result"]["service"].clone(),
                        ) else {
                            continue;
                        };
                        if state.pid != self.child.id() {
                            continue;
                        }
                        assert_eq!(Path::new(&state.endpoint), endpoint);
                        assert_eq!(state.index_path, fs::canonicalize(&self.index).unwrap());
                        assert!(state.instance_id.as_ref().is_some_and(|id| !id.is_empty()));
                        // Model service start's advisory publication using the returned child identity.
                        write_state(&self.state, &state).unwrap();
                        return state;
                    }
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "service did not become ready: {}",
                fs::read_to_string(&self.stderr).unwrap()
            )
        })
    }

    async fn wait(&mut self) -> ExitStatus {
        timeout(CHILD_BOUND, async {
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "owned child did not exit: {}",
                fs::read_to_string(&self.stderr).unwrap()
            )
        })
    }

    async fn rejected(&mut self) {
        assert_eq!(self.wait().await.code(), Some(1));
        assert!(!fs::read(&self.stderr).unwrap().is_empty());
        assert!(IndexWriterGuard::acquire(&self.index).is_ok());
    }

    fn crash(&mut self) {
        self.child.kill().unwrap();
        assert!(!self.child.wait().unwrap().success());
    }

    async fn shutdown(&mut self, state: &ServiceState) {
        assert_eq!(
            rpc(&state.endpoint, &shutdown_request(state)).await["result"]["status"],
            "shutting_down"
        );
        assert!(self.wait().await.success());
        assert!(IndexWriterGuard::acquire(&self.index).is_ok());
    }
}

impl Drop for OwnedChild<'_> {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
