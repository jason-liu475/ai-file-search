use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use ai_file_search_daemon::service::{
    ServiceCoordination, ServiceInstanceGuard, ServiceState, ServiceStatus, read_state,
    remove_state, remove_state_if_matches, render_status_json, render_status_text, write_state,
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
        auto_refresh_seconds: Some(300),
        instance_id: None,
    };

    write_state(&state_path, &state).expect("state should write");

    let loaded = read_state(&state_path).expect("state should read");
    assert_eq!(loaded, Some(state));
}

#[test]
fn legacy_service_state_defaults_auto_refresh_to_none() {
    let fixture = TestDir::new("legacy_service_state_defaults_auto_refresh_to_none");
    let state_path = fixture.path().join("service-state.json");
    fs::write(
        &state_path,
        r#"{"endpoint":"legacy","pid":42,"index_path":"index.txt","started_unix_seconds":1}"#,
    )
    .expect("legacy fixture should write");

    let state = read_state(&state_path)
        .expect("legacy state should read")
        .expect("legacy state should exist");

    assert_eq!(state.auto_refresh_seconds, None);
    assert_eq!(state.instance_id, None);
}

#[test]
fn state_round_trips_optional_instance_generation_without_changing_legacy_rendering() {
    let fixture = TestDir::new("instance-generation");
    let path = fixture.path().join("state.json");
    let mut state = sample_state();
    state.instance_id = Some("child-generation".into());
    write_state(&path, &state).unwrap();
    assert_eq!(read_state(&path).unwrap(), Some(state.clone()));
    let mut older = state.clone();
    older.instance_id = Some("previous-generation-with-identical-pid".into());
    assert!(!remove_state_if_matches(&path, &older).unwrap());
    assert_eq!(read_state(&path).unwrap(), Some(state.clone()));
    let current_rendering = render_status_json(&ServiceStatus::Running(state.clone()));
    state.instance_id = None;
    assert_eq!(
        current_rendering,
        render_status_json(&ServiceStatus::Running(state))
    );
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
        auto_refresh_seconds: Some(300),
        instance_id: None,
    };

    assert_eq!(
        render_status_text(&ServiceStatus::Running(state)),
        "running endpoint=aifs-test pid=42 index=C:/tmp/index.txt auto refresh: 300s\n"
    );
}

#[test]
fn service_status_text_omits_auto_refresh_when_disabled() {
    let state = ServiceState {
        endpoint: "aifs-test".to_owned(),
        pid: 42,
        index_path: PathBuf::from("C:/tmp/index.txt"),
        started_unix_seconds: 1_782_281_286,
        auto_refresh_seconds: None,
        instance_id: None,
    };

    assert!(!render_status_text(&ServiceStatus::Running(state)).contains("auto refresh:"));
}

#[test]
fn service_status_json_omits_auto_refresh_when_disabled() {
    let state = ServiceState {
        endpoint: "aifs-test".to_owned(),
        pid: 42,
        index_path: PathBuf::from("C:/tmp/index.txt"),
        started_unix_seconds: 1_782_281_286,
        auto_refresh_seconds: None,
        instance_id: None,
    };

    assert!(!render_status_json(&ServiceStatus::Running(state)).contains("auto_refresh_seconds"));
}

#[test]
fn service_status_json_includes_auto_refresh_when_enabled() {
    let state = ServiceState {
        endpoint: "aifs-test".to_owned(),
        pid: 42,
        index_path: PathBuf::from("C:/tmp/index.txt"),
        started_unix_seconds: 1_782_281_286,
        auto_refresh_seconds: Some(300),
        instance_id: None,
    };

    assert!(
        render_status_json(&ServiceStatus::Running(state)).contains("\"auto_refresh_seconds\":300")
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

#[test]
fn state_read_enforces_64_kib_including_trailing_whitespace() {
    let fixture = TestDir::new("bounded_read");
    let path = fixture.path().join("state.json");
    let mut bytes = serde_json::to_vec(&sample_state()).unwrap();
    bytes.resize(64 * 1024, b' ');
    fs::write(&path, &bytes).unwrap();
    assert_eq!(read_state(&path).unwrap(), Some(sample_state()));

    bytes.push(b' ');
    fs::write(&path, &bytes).unwrap();
    let error = read_state(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("64 KiB"));
}

#[test]
fn state_read_rejects_invalid_utf8_and_trailing_json() {
    let fixture = TestDir::new("invalid_state_bytes");
    let path = fixture.path().join("state.json");
    fs::write(&path, [0xff, 0xfe]).unwrap();
    assert_eq!(
        read_state(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    let mut bytes = serde_json::to_vec(&sample_state()).unwrap();
    bytes.extend_from_slice(b" {}");
    fs::write(&path, bytes).unwrap();
    assert_eq!(
        read_state(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn atomic_publication_replaces_state_with_newline_and_no_temporary_leftovers() {
    let fixture = TestDir::new("atomic_replace");
    let path = fixture.path().join("state.json");
    write_state(&path, &sample_state()).unwrap();
    let mut replacement = sample_state();
    replacement.pid += 1;
    write_state(&path, &replacement).unwrap();
    assert_eq!(read_state(&path).unwrap(), Some(replacement));
    assert!(fs::read(&path).unwrap().ends_with(b"\n"));
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 1);
}

#[test]
fn publication_limit_includes_newline_and_failure_preserves_old_bytes() {
    let fixture = TestDir::new("bounded_publication");
    let path = fixture.path().join("state.json");
    let mut exact = sample_state();
    let overhead = serde_json::to_vec_pretty(&exact).unwrap().len() + 1 - exact.endpoint.len();
    exact.endpoint = "a".repeat(64 * 1024 - overhead);
    write_state(&path, &exact).unwrap();
    let old_bytes = fs::read(&path).unwrap();
    assert_eq!(old_bytes.len(), 64 * 1024);
    assert_eq!(read_state(&path).unwrap(), Some(exact.clone()));

    exact.endpoint.push('a');
    let error = write_state(&path, &exact).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("64 KiB"));
    assert_eq!(fs::read(&path).unwrap(), old_bytes);
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 1);
}

#[test]
fn publication_counts_json_escaping_not_input_string_length() {
    let fixture = TestDir::new("escaped_publication_limit");
    let path = fixture.path().join("state.json");
    write_state(&path, &sample_state()).unwrap();
    let old_bytes = fs::read(&path).unwrap();
    let mut too_large = sample_state();
    too_large.endpoint = "\"".repeat(40 * 1024);
    assert_eq!(
        write_state(&path, &too_large).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read(&path).unwrap(), old_bytes);
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 1);
}

#[test]
fn reading_state_directory_returns_explicit_nonregular_error() {
    let fixture = TestDir::new("state_directory_read");
    let path = fixture.path().join("state.json");
    fs::create_dir(&path).unwrap();
    let error = read_state(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("regular file"));
}

#[test]
fn failed_publication_does_not_remove_an_existing_directory_or_foreign_temp() {
    let fixture = TestDir::new("directory_publish_failure");
    let path = fixture.path().join("state.json");
    fs::create_dir(&path).unwrap();
    let marker = path.join("keep.txt");
    fs::write(&marker, b"keep").unwrap();
    let foreign = fixture.path().join("state.json.foreign.tmp");
    fs::write(&foreign, b"foreign").unwrap();

    assert!(write_state(&path, &sample_state()).is_err());
    assert_eq!(
        read_state(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(fs::read(&marker).unwrap(), b"keep");
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn reading_socket_state_is_rejected_without_opening_it() {
    use std::os::unix::net::UnixListener;

    let fixture = TestDir::new("socket_state");
    let path = fixture.path().join("state.json");
    let _listener = UnixListener::bind(&path).unwrap();
    assert_eq!(
        read_state(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(ServiceCoordination::startup_active(&path).is_err());
}

#[cfg(windows)]
#[test]
fn windows_rename_failure_preserves_old_bytes_and_cleans_only_owned_temp() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = TestDir::new("share_mode_publish_failure");
    let path = fixture.path().join("state.json");
    write_state(&path, &sample_state()).unwrap();
    let old = fs::read(&path).unwrap();
    let foreign = fixture.path().join("state.json.foreign.tmp");
    fs::write(&foreign, b"foreign").unwrap();
    // FILE_SHARE_READ | FILE_SHARE_WRITE deliberately excludes FILE_SHARE_DELETE.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(3)
        .open(&path)
        .unwrap();
    let mut replacement = sample_state();
    replacement.pid += 1;

    assert!(write_state(&path, &replacement).is_err());
    assert_eq!(fs::read(&path).unwrap(), old);
    assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 2);
    drop(held);
    write_state(&path, &replacement).unwrap();
    assert_eq!(read_state(&path).unwrap(), Some(replacement));
}

#[test]
fn match_removal_checks_every_state_field() {
    let fixture = TestDir::new("owner_matched_remove");
    let path = fixture.path().join("state.json");
    let expected = sample_state();
    assert!(!remove_state_if_matches(&path, &expected).unwrap());
    let mut variants = Vec::new();
    let mut state = expected.clone();
    state.endpoint.push_str("-other");
    variants.push(state);
    let mut state = expected.clone();
    state.pid += 1;
    variants.push(state);
    let mut state = expected.clone();
    state.index_path.push("other");
    variants.push(state);
    let mut state = expected.clone();
    state.started_unix_seconds += 1;
    variants.push(state);
    let mut state = expected.clone();
    state.auto_refresh_seconds = None;
    variants.push(state);

    for replacement in variants {
        write_state(&path, &replacement).unwrap();
        assert!(!remove_state_if_matches(&path, &expected).unwrap());
        assert_eq!(read_state(&path).unwrap(), Some(replacement));
    }
    write_state(&path, &expected).unwrap();
    assert!(remove_state_if_matches(&path, &expected).unwrap());
    assert!(!path.exists());
}

#[test]
fn match_removal_propagates_malformed_state_instead_of_deleting_it() {
    let fixture = TestDir::new("malformed_remove");
    let path = fixture.path().join("state.json");
    fs::write(&path, b"{broken").unwrap();
    assert_eq!(
        remove_state_if_matches(&path, &sample_state())
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read(path).unwrap(), b"{broken");
}

#[test]
fn ownership_probes_never_create_missing_parents_or_locks() {
    let fixture = TestDir::new("no_create_probes");
    let path = fixture.path().join("new/nested/state.json");
    assert!(!ServiceCoordination::startup_active(&path).unwrap());
    assert!(!ServiceCoordination::instance_active(&path).unwrap());
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 0);

    let existing_parent_path = fixture.path().join("state.json");
    assert!(!ServiceCoordination::startup_active(&existing_parent_path).unwrap());
    assert!(!ServiceCoordination::instance_active(&existing_parent_path).unwrap());
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 0);
}

#[test]
fn startup_and_instance_locks_are_independent_and_persistent() {
    let fixture = TestDir::new("independent_locks");
    let path = fixture.path().join("new/state.json");
    let coordinator = ServiceCoordination::acquire(&path).unwrap();
    assert!(coordinator.state_path().is_absolute());
    assert!(ServiceCoordination::startup_active(&path).unwrap());
    assert!(!ServiceCoordination::instance_active(&path).unwrap());
    assert_eq!(
        ServiceCoordination::acquire(&path).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );

    let instance = ServiceInstanceGuard::acquire(&path).unwrap();
    assert!(ServiceCoordination::instance_active(&path).unwrap());
    assert_eq!(
        ServiceInstanceGuard::acquire(&path).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    let expected_state = fs::canonicalize(path.parent().unwrap())
        .unwrap()
        .join("state.json");
    assert_eq!(
        instance.artifact_paths(),
        vec![
            expected_state.clone(),
            expected_state.with_file_name("state.json.startup.lock"),
            expected_state.with_file_name("state.json.instance.lock"),
        ]
    );
    assert_eq!(coordinator.state_path(), expected_state);
    assert!(!path.exists());

    drop(coordinator);
    assert!(!ServiceCoordination::startup_active(&path).unwrap());
    assert!(ServiceCoordination::instance_active(&path).unwrap());
    drop(instance);
    assert!(!ServiceCoordination::instance_active(&path).unwrap());
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 2);
    let _coordinator = ServiceCoordination::acquire(&path).unwrap();
    let _instance = ServiceInstanceGuard::acquire(&path).unwrap();
}

#[test]
fn acquiring_and_probing_existing_lock_files_never_truncates_them() {
    let fixture = TestDir::new("no_lock_truncation");
    let path = fixture.path().join("state.json");
    let startup = fixture.path().join("state.json.startup.lock");
    let instance = fixture.path().join("state.json.instance.lock");
    fs::write(&startup, b"startup marker").unwrap();
    fs::write(&instance, b"instance marker").unwrap();
    assert!(!ServiceCoordination::startup_active(&path).unwrap());
    assert!(!ServiceCoordination::instance_active(&path).unwrap());
    let coordinator = ServiceCoordination::acquire(&path).unwrap();
    let instance_guard = ServiceInstanceGuard::acquire(&path).unwrap();
    drop(coordinator);
    drop(instance_guard);
    assert_eq!(fs::read(startup).unwrap(), b"startup marker");
    assert_eq!(fs::read(instance).unwrap(), b"instance marker");
}

#[test]
fn path_aliases_share_startup_and_instance_identity() {
    let fixture = TestDir::new("path_aliases");
    let path = fixture.path().join("new/state.json");
    let alias = fixture.path().join("./unused/../new/./state.json");
    let coordinator = ServiceCoordination::acquire(&alias).unwrap();
    let instance = ServiceInstanceGuard::acquire(&path).unwrap();
    assert_eq!(coordinator.state_path(), &instance.artifact_paths()[0]);
    assert!(ServiceCoordination::startup_active(&path).unwrap());
    assert!(ServiceCoordination::instance_active(&alias).unwrap());
    assert_eq!(
        ServiceCoordination::acquire(&path).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(
        ServiceInstanceGuard::acquire(&alias).unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );

    let relative = std::path::absolute(".").unwrap();
    let relative_path = format!(
        "aifs-missing-parent-{}-relative/state.json",
        std::process::id()
    );
    assert!(!ServiceCoordination::startup_active(Path::new(&relative_path)).unwrap());
    assert!(!relative.join(relative_path).parent().unwrap().exists());
}

#[test]
fn probes_and_acquisition_reject_state_and_lock_directories() {
    let fixture = TestDir::new("directory_artifacts");
    let path = fixture.path().join("state.json");
    fs::create_dir(&path).unwrap();
    assert_eq!(
        ServiceCoordination::startup_active(&path)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(
        ServiceCoordination::instance_active(&path)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(ServiceCoordination::acquire(&path).is_err());
    assert!(ServiceInstanceGuard::acquire(&path).is_err());
    fs::remove_dir(&path).unwrap();

    let startup = fixture.path().join("state.json.startup.lock");
    fs::create_dir(&startup).unwrap();
    assert_eq!(
        ServiceCoordination::startup_active(&path)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(ServiceCoordination::acquire(&path).is_err());
    let instance = fixture.path().join("state.json.instance.lock");
    fs::create_dir(&instance).unwrap();
    assert_eq!(
        ServiceCoordination::instance_active(&path)
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(ServiceInstanceGuard::acquire(&path).is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_parents_share_identity_but_artifact_links_are_rejected() {
    use std::os::unix::fs::symlink;

    let fixture = TestDir::new("symlink_identity");
    let parent = fixture.path().join("actual");
    fs::create_dir(&parent).unwrap();
    let alias = fixture.path().join("alias");
    symlink(&parent, &alias).unwrap();
    let path = parent.join("state.json");
    let coordinator = ServiceCoordination::acquire(&alias.join("new/state.json")).unwrap();
    assert!(ServiceCoordination::startup_active(&parent.join("new/state.json")).unwrap());
    drop(coordinator);
    let target = parent.join("target");
    fs::write(&target, b"keep").unwrap();
    symlink(&target, &path).unwrap();
    assert!(ServiceCoordination::startup_active(&path).is_err());
    assert!(ServiceInstanceGuard::acquire(&path).is_err());
    assert!(write_state(&path, &sample_state()).is_err());
    assert_eq!(
        read_state(&path).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert_eq!(fs::read(&target).unwrap(), b"keep");
    fs::remove_file(&path).unwrap();
    symlink(&target, parent.join("state.json.startup.lock")).unwrap();
    assert!(ServiceCoordination::startup_active(&path).is_err());
    assert!(ServiceCoordination::acquire(&path).is_err());
    symlink(&target, parent.join("state.json.instance.lock")).unwrap();
    assert!(ServiceCoordination::instance_active(&path).is_err());
    assert!(ServiceInstanceGuard::acquire(&path).is_err());
}

#[cfg(unix)]
#[test]
fn unix_state_and_lock_files_have_private_permissions() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = TestDir::new("private_permissions");
    let path = fixture.path().join("state.json");
    write_state(&path, &sample_state()).unwrap();
    let _coordinator = ServiceCoordination::acquire(&path).unwrap();
    let instance = ServiceInstanceGuard::acquire(&path).unwrap();
    for artifact in instance.artifact_paths() {
        assert_eq!(
            fs::metadata(artifact).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn lock_ownership_is_observed_and_released_across_processes() {
    let fixture = TestDir::new("subprocess_locks");
    let path = fixture.path().join("state.json");
    let coordinator = ServiceCoordination::acquire(&path).unwrap();
    let instance = ServiceInstanceGuard::acquire(&path).unwrap();
    run_lock_probe(&path, true, true);
    drop(coordinator);
    run_lock_probe(&path, false, true);
    drop(instance);
    run_lock_probe(&path, false, false);
}

#[test]
#[ignore = "invoked in an isolated subprocess by the ownership test"]
fn service_lock_subprocess_probe() {
    let path = PathBuf::from(std::env::var_os("AIFS_TEST_LOCK_PATH").unwrap());
    let startup = std::env::var("AIFS_TEST_STARTUP_ACTIVE").unwrap() == "true";
    let instance = std::env::var("AIFS_TEST_INSTANCE_ACTIVE").unwrap() == "true";
    assert_eq!(ServiceCoordination::startup_active(&path).unwrap(), startup);
    assert_eq!(
        ServiceCoordination::instance_active(&path).unwrap(),
        instance
    );
    let startup_acquisition = ServiceCoordination::acquire(&path);
    if startup {
        assert_eq!(
            startup_acquisition.unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    } else {
        drop(startup_acquisition.unwrap());
    }
    let instance_acquisition = ServiceInstanceGuard::acquire(&path);
    if instance {
        assert_eq!(
            instance_acquisition.unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    } else {
        drop(instance_acquisition.unwrap());
    }
}

fn run_lock_probe(path: &Path, startup: bool, instance: bool) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "service_lock_subprocess_probe",
            "--ignored",
            "--nocapture",
        ])
        .env("AIFS_TEST_LOCK_PATH", path)
        .env("AIFS_TEST_STARTUP_ACTIVE", startup.to_string())
        .env("AIFS_TEST_INSTANCE_ACTIVE", instance.to_string())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn service_status_renders_new_status_names_and_optional_state() {
    for (status, name) in [
        (ServiceStatus::Starting, "starting"),
        (ServiceStatus::Unresponsive(None), "unresponsive"),
    ] {
        assert_eq!(render_status_text(&status), format!("{name}\n"));
        let value: serde_json::Value = serde_json::from_str(&render_status_json(&status)).unwrap();
        assert_eq!(value, serde_json::json!({ "status": name }));
    }
    let status = ServiceStatus::Unresponsive(Some(sample_state()));
    assert!(render_status_text(&status).starts_with("unresponsive endpoint="));
    let value: serde_json::Value = serde_json::from_str(&render_status_json(&status)).unwrap();
    assert_eq!(value["status"], "unresponsive");
    assert_eq!(value["pid"], sample_state().pid);
    assert_eq!(value["auto_refresh_seconds"], 300);
}

#[test]
fn error_status_preserves_reason_without_required_state_fields() {
    let reason = "malformed state: \"broken\"";
    let status = ServiceStatus::Error(reason.to_owned());
    assert_eq!(render_status_text(&status), format!("error {reason}\n"));
    let value: serde_json::Value = serde_json::from_str(&render_status_json(&status)).unwrap();
    assert_eq!(
        value,
        serde_json::json!({ "status": "error", "reason": reason })
    );
}

#[test]
fn stale_status_keeps_existing_text_and_json_shape() {
    let status = ServiceStatus::Stale(sample_state());
    assert_eq!(
        render_status_text(&status),
        "stale endpoint=aifs-test pid=42 index=index.txt auto refresh: 300s\n"
    );
    let value: serde_json::Value = serde_json::from_str(&render_status_json(&status)).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "status": "stale", "endpoint": "aifs-test", "pid": 42,
            "index_path": "index.txt", "started_unix_seconds": 1,
            "auto_refresh_seconds": 300,
        })
    );
}

fn sample_state() -> ServiceState {
    ServiceState {
        endpoint: "aifs-test".to_owned(),
        pid: 42,
        index_path: PathBuf::from("index.txt"),
        started_unix_seconds: 1,
        auto_refresh_seconds: Some(300),
        instance_id: None,
    }
}

static TEST_DIRECTORY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let sequence = TEST_DIRECTORY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        #[cfg(windows)]
        let path = std::env::temp_dir().join(format!(
            "ai-file-search-service-state-{name}-{}-{}",
            std::process::id(),
            sequence
        ));
        #[cfg(unix)]
        let path = PathBuf::from("/tmp").join(format!("aifs-st-{}-{sequence}", std::process::id()));

        fs::create_dir(&path)
            .unwrap_or_else(|error| panic!("fixture {name} creation failed: {error}"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        #[cfg(unix)]
        let path = fs::canonicalize(path).unwrap();

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
