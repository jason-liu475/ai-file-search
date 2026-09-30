use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_file_search_daemon::service::{SERVICE_STATE_ENV, read_state};
use ai_file_search_daemon::{handle_json_line, send_ipc_request};
use ai_file_search_indexer::{
    FileIndexStore, FileIndexWriter, IndexWriterGuard, ScanOptions, Scanner,
};
use serde_json::{Value, json};

#[tokio::test]
async fn managed_custom_state_inside_root_is_excluded_from_status_and_publication() {
    check_managed_artifacts(false).await;
}

#[tokio::test]
async fn managed_relative_state_resolves_against_start_child_cwd() {
    check_managed_artifacts(true).await;
}

async fn check_managed_artifacts(relative_state: bool) {
    let fixture = Fixture::new(relative_state);
    let state = if relative_state {
        PathBuf::from("root/owned.runtime")
    } else {
        fixture.root.join("owned.runtime")
    };
    let mut service = StopGuard {
        cwd: fixture.path.clone(),
        state: state.clone(),
        armed: true,
    };
    let stderr = fixture.path.join("start.stderr");
    let start = Command::new(env!("CARGO_BIN_EXE_ai-file-search-daemon"))
        .current_dir(&fixture.path)
        .env(SERVICE_STATE_ENV, &state)
        .args(["service", "start"])
        .arg(&fixture.index)
        .args(["--endpoint", &fixture.endpoint])
        .stdout(Stdio::null())
        .stderr(Stdio::from(fs::File::create(&stderr).unwrap()))
        .status()
        .unwrap();
    assert!(start.success(), "{}", fs::read_to_string(&stderr).unwrap());
    assert!(fixture.root.join("owned.runtime").is_file());
    let managed_state = read_state(&fixture.root.join("owned.runtime"))
        .unwrap()
        .expect("start must publish its actual endpoint");
    assert_eq!(managed_state.endpoint, fixture.endpoint);
    let endpoint = &managed_state.endpoint;
    #[cfg(unix)]
    let endpoint_marker = {
        let socket = Path::new(endpoint);
        assert_eq!(socket.parent().unwrap(), fixture.root);
        let marker = socket.with_file_name(format!(
            ".{}.aifs-endpoint.lock",
            socket.file_name().unwrap().to_str().unwrap()
        ));
        assert!(marker.is_file());
        (marker.clone(), fs::read(marker).unwrap())
    };
    let state_temporary = fixture.root.join(".owned.runtime.aifs-tmp-abandoned.tmp");
    fs::write(&state_temporary, "reserved service temporary").unwrap();
    let before = fs::read(&fixture.index).unwrap();
    assert_eq!(
        rpc(endpoint, "stats", json!({})).await,
        json!({"files": 4, "total_bytes": 16})
    );
    let before_search = rpc(endpoint, "search", json!({"query":"owned.runtime"})).await;
    assert_eq!(
        rpc(endpoint, "index_status", json!({})).await,
        summary(0, 4)
    );
    assert_eq!(fs::read(&fixture.index).unwrap(), before);
    for method in ["refresh", "reindex"] {
        let mut expected = summary(0, 4);
        expected.as_object_mut().unwrap().remove("needs_refresh");
        assert_eq!(rpc(endpoint, method, json!({})).await, expected);
        assert_eq!(fs::read(&fixture.index).unwrap(), before);
    }
    assert_eq!(
        rpc(endpoint, "search", json!({"query":"owned.runtime"})).await,
        before_search
    );
    assert_eq!(
        rpc(endpoint, "stats", json!({})).await,
        json!({"files": 4, "total_bytes": 16})
    );
    fs::write(fixture.root.join("added.txt"), "added").unwrap();
    let mut expected = summary(1, 4);
    expected.as_object_mut().unwrap().remove("needs_refresh");
    assert_eq!(rpc(endpoint, "refresh", json!({})).await, expected);
    assert_eq!(
        rpc(endpoint, "index_status", json!({})).await,
        summary(0, 5)
    );
    let store = FileIndexStore::open(&fixture.index).unwrap();
    assert_saved_scope(&store, &state_temporary);
    #[cfg(unix)]
    {
        assert!(store.search_by_name("aifs-endpoint.lock").is_empty());
        assert_eq!(fs::read(&endpoint_marker.0).unwrap(), endpoint_marker.1);
    }
    assert_eq!(
        rpc(endpoint, "search", json!({"query":"owned.runtime"})).await["files"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    service.stop();
    assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());
}

fn assert_saved_scope(store: &FileIndexStore, state_temporary: &Path) {
    assert_eq!(store.file_count(), 5);
    assert_eq!(store.total_size_bytes(), 21);
    assert_eq!(store.search_by_name("owned.runtime").len(), 1);
    assert_eq!(
        store.search_by_name("owned.runtime")[0]
            .relative_path
            .as_normalized(),
        "elsewhere/owned.runtime"
    );
    assert_eq!(store.search_by_name("ordinary.tmp").len(), 1);
    assert!(store.search_by_name("aifs-tmp").is_empty());
    assert_eq!(
        fs::read_to_string(state_temporary).unwrap(),
        "reserved service temporary"
    );
    assert_eq!(store.search_by_name("secret").len(), 0);
    assert_eq!(
        store.scan_policy(),
        Some(&ScanOptions::default().exclude_name("target"))
    );
}

#[test]
fn direct_handler_does_not_guess_managed_state_artifacts() {
    let fixture = Fixture::new(false);
    fs::write(fixture.root.join("owned.runtime"), "user file").unwrap();
    let request = json!({"id": 1, "method": "refresh", "params": {}}).to_string();
    let response = handle_json_line(&fixture.index, &request);
    let response: Value = serde_json::from_str(&response.to_json_line()).unwrap();
    assert_eq!(response["result"]["added"], 1);
    assert_eq!(
        FileIndexStore::open(&fixture.index)
            .unwrap()
            .search_by_name("owned.runtime")
            .len(),
        2
    );
}

fn summary(added: usize, unchanged: usize) -> Value {
    json!({"scanned_files": added + unchanged, "added": added, "updated": 0,
        "removed": 0, "unchanged": unchanged, "needs_refresh": added != 0})
}

async fn rpc(endpoint: &str, method: &str, params: Value) -> Value {
    let request = json!({"id": 1, "method": method, "params": params}).to_string();
    let response = tokio::time::timeout(Duration::from_secs(5), async {
        for _ in 0..100 {
            if let Ok(response) = send_ipc_request(endpoint, &request).await {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("service endpoint remained unavailable");
    })
    .await
    .unwrap();
    let response: Value = serde_json::from_str(&response).unwrap();
    assert!(response.get("error").is_none(), "{response}");
    response["result"].clone()
}

struct StopGuard {
    cwd: PathBuf,
    state: PathBuf,
    armed: bool,
}

impl StopGuard {
    fn stop(&mut self) {
        let output = self.command().output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.armed = false;
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-file-search-daemon"));
        command
            .current_dir(&self.cwd)
            .env(SERVICE_STATE_ENV, &self.state)
            .args(["service", "stop"]);
        command
    }
}

impl Drop for StopGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.command().output();
        }
    }
}

struct Fixture {
    path: PathBuf,
    root: PathBuf,
    index: PathBuf,
    endpoint: String,
}

impl Fixture {
    fn new(relative: bool) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(
            "aifs-managed-artifact-{}-{nonce}-{relative}",
            std::process::id()
        );
        #[cfg(windows)]
        let path = std::env::temp_dir().join(&name);
        #[cfg(unix)]
        let path = PathBuf::from("/tmp").join(format!(
            "aifs-ma-{}-{nonce:x}-{relative}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        #[cfg(unix)]
        let path = fs::canonicalize(path).unwrap();
        let root = path.join("root");
        fs::create_dir_all(root.join("elsewhere")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        for file in [
            "kept.txt",
            "ordinary.tmp",
            "elsewhere/owned.runtime",
            "service-state.json",
            "target/secret.txt",
        ] {
            fs::write(root.join(file), "kept").unwrap();
        }
        let index = root.join("index.txt");
        fs::write(
            root.join(".index.txt.aifs-tmp-owned"),
            "reserved publication artifact",
        )
        .unwrap();
        let options = ScanOptions::default().exclude_name("target");
        let mut guard = IndexWriterGuard::acquire(&index).unwrap();
        let files = Scanner::new(options.clone())
            .scan_for_index(&root, guard.index_path())
            .unwrap();
        let mut writer = FileIndexWriter::new(&mut guard);
        writer.set_root_path(fs::canonicalize(&root).unwrap());
        writer.set_scan_policy(options);
        writer.replace_all(files);
        writer.save().unwrap();
        drop(writer);
        drop(guard);
        let endpoint = if cfg!(windows) {
            name
        } else {
            root.join("s.sock").to_string_lossy().into_owned()
        };
        Self {
            path,
            root,
            index,
            endpoint,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
