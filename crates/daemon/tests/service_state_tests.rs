use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_daemon::service::{
    ServiceState, ServiceStatus, read_state, remove_state, render_status_json, render_status_text,
    write_state,
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
    assert_eq!(
        render_status_json(&ServiceStatus::Stopped),
        "{\"status\":\"stopped\"}\n"
    );
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
        path.push(format!(
            "ai-file-search-service-state-{name}-{}",
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
