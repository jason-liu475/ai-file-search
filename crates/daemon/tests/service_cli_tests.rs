use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_daemon::run_with_state_path;
use ai_file_search_daemon::service::{ServiceState, write_state};
use ai_file_search_indexer::FileIndexStore;

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
    FileIndexStore::open(&index_path)
        .expect("store should open")
        .save()
        .expect("store should save without root metadata");

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
