use super::*;
use std::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

#[cfg(windows)]
#[test]
fn complete_pipe_prefix_is_case_insensitive_without_double_prefixing() {
    for endpoint in [r"\\.\pipe\custom", r"\\.\PIPE\custom", r"\\.\PiPe\custom"] {
        assert_eq!(pipe_name(endpoint), endpoint);
    }
    assert_eq!(pipe_name("custom"), r"\\.\pipe\custom");
    assert_eq!(pipe_name(""), r"\\.\pipe\");
}

#[cfg(windows)]
#[tokio::test]
async fn owned_legacy_acl_requires_restart_but_remains_stoppable() {
    use tokio::net::windows::named_pipe::ServerOptions;
    let fixture = Fixture::new("legacy-acl");
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let mut writer = FileIndexWriter::new(&mut guard);
    writer.set_root_path(fixture.path.clone());
    writer.save().unwrap();
    let state_path = fixture.path.join("state.json");
    let instance = ServiceInstanceGuard::acquire(&state_path).unwrap();
    let mut state = test_identity(guard.index_path());
    state.endpoint = fixture.path.file_name().unwrap().to_str().unwrap().into();
    write_state(&state_path, &state).unwrap();
    let before = fs::read(&state_path).unwrap();
    let endpoint = pipe_name(&state.endpoint);
    let mut pending = ServerOptions::new()
        .first_pipe_instance(true)
        .reject_remote_clients(true)
        .create(&endpoint)
        .unwrap();
    let identity = service_identity(&state);
    let worker = tokio::spawn(async move {
        let _ownership = (guard, instance);
        loop {
            if pending.connect().await.is_err() {
                let _ = pending.disconnect();
                tokio::task::yield_now().await;
                continue;
            }
            let next = ServerOptions::new().create(&endpoint).unwrap();
            let mut connected = std::mem::replace(&mut pending, next);
            let Ok(line) = bounded_io::read_request(&mut connected).await else {
                continue;
            };
            let request = Request::from_json_line(&line).unwrap();
            let shutdown = request.method == "shutdown";
            let result = if shutdown {
                json!({"status":"shutting_down"})
            } else {
                json!({"status":"ok","service":identity})
            };
            bounded_io::write_response(
                &mut connected,
                &Response::success(request.id, result).to_json_line(),
            )
            .await
            .unwrap();
            if shutdown {
                break;
            }
        }
    });
    let started = service_start(
        &[
            fixture.index.to_str().unwrap().into(),
            "--endpoint".into(),
            state.endpoint.clone(),
        ],
        &state_path,
    )
    .await;
    let preserved = fs::read(&state_path);
    let active = ServiceCoordination::instance_active(&state_path);
    let stopped = service_stop(&state_path).await;
    worker.abort();
    let _ = worker.await;
    assert_eq!(started.exit_code, 1);
    assert!(
        started.stderr.contains("security upgrade required"),
        "{}",
        started.stderr
    );
    assert!(started.stderr.contains("stop/start"));
    assert_eq!(preserved.unwrap(), before);
    assert!(active.unwrap());
    assert_eq!(stopped.exit_code, 0, "{}", stopped.stderr);
    assert!(!state_path.exists());
    assert!(IndexWriterGuard::acquire(&fixture.index).is_ok());
}

#[test]
fn health_response_requires_structured_success_with_matching_id() {
    for line in [
        r#"{"id":2,"result":{"status":"ok"}}"#,
        r#"{"id":1,"error":{"message":"status: ok"}}"#,
        r#"{"id":1,"result":{"status":"ok"},"error":null}"#,
        r#"{"id":1,"result":"status: ok"}"#,
        r#"{"result":{"status":"ok"}}"#,
        "not json",
    ] {
        assert!(response_result(line).is_err(), "accepted {line}");
    }
    assert_eq!(
        response_result(r#"{"id":1,"result":{"status":"ok"}}"#).unwrap(),
        json!({"status":"ok"})
    );
}

#[test]
fn service_identity_checks_pid_index_endpoint_and_configuration() {
    let state = test_identity(Path::new("index.txt"));
    let identity = service_identity(&state);
    assert!(identity_matches(&identity, &state));
    for (field, changed) in [
        ("pid", json!(state.pid + 1)),
        ("index_path", json!("other-index.txt")),
        ("endpoint", json!("other-endpoint")),
        ("auto_refresh_seconds", json!(300)),
        (
            "started_unix_seconds",
            json!(state.started_unix_seconds + 1),
        ),
        ("instance_id", json!("different-generation")),
    ] {
        let mut impostor = identity.clone();
        impostor[field] = changed;
        assert!(!identity_matches(&impostor, &state), "ignored {field}");
    }
    assert!(!identity_matches(&json!({"status":"ok"}), &state));
}

#[tokio::test]
async fn managed_ping_reports_identity_without_changing_direct_ping() {
    let fixture = Fixture::new("identity");
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let identity = test_identity(guard.index_path());
    let (mut client, server) = duplex(4096);
    client
        .write_all(b"{\"id\":1,\"method\":\"ping\"}\n")
        .await
        .unwrap();
    let status = handle_managed_connection(&mut guard, server, &identity, &[])
        .await
        .unwrap();
    assert_eq!(status, StreamStatus::ClientDisconnected);
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert_eq!(
        response_result(&response).unwrap(),
        json!({"status":"ok","service":service_identity(&identity)})
    );
    assert_eq!(
        handle_json_line(&fixture.index, r#"{"id":1,"method":"ping"}"#).result,
        Some(json!({"status":"ok"}))
    );
}

#[tokio::test]
async fn targeted_shutdown_rejects_mismatched_identity_without_releasing_writer() {
    let fixture = Fixture::new("mismatch");
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let identity = test_identity(guard.index_path());
    let (mut client, server) = duplex(4096);
    let mut target = service_identity(&identity);
    target["instance_id"] = json!("previous-generation-with-same-pid");
    let request = format!(
        "{}\n",
        json!({"id":1,"method":"shutdown","params":{"service":target}})
    );
    client.write_all(request.as_bytes()).await.unwrap();
    assert_eq!(
        handle_managed_connection(&mut guard, server, &identity, &[])
            .await
            .unwrap(),
        StreamStatus::ClientDisconnected
    );
    let mut response = String::new();
    client.read_to_string(&mut response).await.unwrap();
    assert!(response_result(&response).is_err());
    assert!(response.contains("service identity mismatch"));
    assert!(IndexWriterGuard::acquire(&fixture.index).is_err());
}

#[tokio::test]
async fn valid_shutdown_is_honored_when_response_reader_disconnects() {
    let fixture = Fixture::new("disconnected-shutdown");
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let identity = test_identity(guard.index_path());
    let (mut client, server) = duplex(4096);
    let request = format!(
        "{}\n",
        json!({"id":1,"method":"shutdown","params":{"service":service_identity(&identity)}})
    );
    client.write_all(request.as_bytes()).await.unwrap();
    drop(client);
    assert_eq!(
        handle_managed_connection(&mut guard, server, &identity, &[])
            .await
            .unwrap(),
        StreamStatus::ShutdownRequested
    );
}

#[tokio::test]
async fn incomplete_managed_request_cannot_run_a_write_or_release_writer() {
    let fixture = Fixture::new("incomplete");
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let mut store = FileIndexWriter::new(&mut guard);
    store.set_root_path(&fixture.path);
    store.set_scan_policy(ScanOptions::default());
    store.save().unwrap();
    drop(store);
    let before = fs::read(&fixture.index).unwrap();
    let identity = test_identity(guard.index_path());
    let (mut client, server) = duplex(4096);
    client
        .write_all(b"{\"id\":1,\"method\":\"reindex\"}")
        .await
        .unwrap();
    drop(client);
    assert_eq!(
        handle_managed_connection(&mut guard, server, &identity, &[])
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(fs::read(&fixture.index).unwrap(), before);
    assert!(IndexWriterGuard::acquire(&fixture.index).is_err());
}

fn test_identity(index: &Path) -> ServiceState {
    ServiceState {
        endpoint: "test-endpoint".into(),
        pid: std::process::id(),
        index_path: index.to_owned(),
        started_unix_seconds: 1,
        auto_refresh_seconds: None,
        instance_id: Some("test-generation".into()),
    }
}

#[test]
fn managed_generations_do_not_repeat_with_the_same_process_and_configuration() {
    let ids = (0..128)
        .map(|_| new_instance_id())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), 128);
}

#[tokio::test]
async fn transient_accept_errors_preserve_the_listener_and_next_request() {
    let mut calls = 0;
    let accepted = accept_with_retry(|| {
        calls += 1;
        std::future::ready(match calls {
            1 => Err(io::Error::from(io::ErrorKind::Interrupted)),
            2 => Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
            _ => Ok("next-ping-connection"),
        })
    })
    .await
    .unwrap();
    assert_eq!(accepted, "next-ping-connection");
    assert_eq!(calls, 3);
    let error = accept_with_retry(|| {
        std::future::ready::<io::Result<()>>(Err(io::Error::from(io::ErrorKind::PermissionDenied)))
    })
    .await
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
}

struct Fixture {
    path: PathBuf,
    index: PathBuf,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aifs-managed-private-{name}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self {
            index: path.join("index.txt"),
            path,
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
