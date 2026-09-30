use super::*;
use std::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

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
