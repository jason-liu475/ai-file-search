#![cfg(any(windows, unix))]

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ai_file_search_core::PathId;
use ai_file_search_daemon::service::SERVICE_STATE_ENV;
use ai_file_search_daemon::{handle_json_line, send_ipc_request};
use ai_file_search_indexer::{FileIndexWriter, IndexWriterGuard, IndexedFile, ScanOptions};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::time::{Instant, sleep, timeout};

const IO_BOUND: Duration = Duration::from_secs(2);
const READ_BOUND: Duration = Duration::from_secs(7);
const FRAME_CAP: usize = 64 * 1024;
static FIXTURE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const PING: &str = r#"{"id":17,"method":"ping","params":{}}"#;

#[tokio::test]
async fn managed_no_auto_idle_connection_has_a_total_read_deadline() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    fixture.ping().await;
    let mut client = connect(&fixture.endpoint).await;
    let started = Instant::now();
    assert_closed(&mut client, READ_BOUND).await;
    assert!(started.elapsed() >= Duration::from_secs(4));
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_auto_refresh_idle_connection_has_a_total_read_deadline() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(Some(300));
    fixture.ping().await;
    let mut client = connect(&fixture.endpoint).await;
    assert_closed(&mut client, READ_BOUND).await;
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_slow_trickle_does_not_restart_the_read_deadline() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    fixture.ping().await;
    let mut client = connect(&fixture.endpoint).await;
    client.write_all(b"{").await.unwrap();
    let started = Instant::now();
    let (mut reader, mut writer) = tokio::io::split(client);
    let trickle = async {
        loop {
            sleep(Duration::from_millis(350)).await;
            if writer.write_all(b" ").await.is_err() {
                break;
            }
        }
    };
    let closed = async {
        let _ = assert_closed(&mut reader, READ_BOUND).await;
    };
    tokio::pin!(trickle);
    tokio::pin!(closed);
    tokio::select! {
        () = &mut closed => {},
        () = &mut trickle => closed.await,
    }
    assert!(started.elapsed() >= Duration::from_secs(4));
    assert!(started.elapsed() < READ_BOUND);
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_oversized_unterminated_frame_is_rejected_before_the_read_deadline() {
    check_oversized_frame(false).await;
}

#[tokio::test]
async fn managed_oversized_newline_frame_is_not_dispatched() {
    check_oversized_frame(true).await;
}

async fn check_oversized_frame(newline: bool) {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    fixture.ping().await;
    let mut client = connect(&fixture.endpoint).await;
    let mut frame = PING.as_bytes().to_vec();
    frame.resize(FRAME_CAP + 1, b' ');
    if newline {
        frame.push(b'\n');
    }
    let written = timeout(IO_BOUND, client.write_all(&frame)).await.unwrap();
    if let Err(error) = written {
        assert!(disconnect_error(&error), "{error}");
    }
    let response = assert_closed(&mut client, IO_BOUND).await;
    assert!(
        response.is_empty(),
        "oversized input was dispatched: {response:?}"
    );
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_empty_eof_is_local_to_the_connection() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    fixture.ping().await;
    let client = connect(&fixture.endpoint).await;
    drop(client);
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_rapid_connect_drop_before_frames_preserves_ping_and_shutdown() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    let identity = fixture.ping().await["result"]["service"].clone();
    timeout(Duration::from_secs(5), async {
        for _ in 0..32 {
            let client = connect(&fixture.endpoint).await;
            drop(client);
        }
    })
    .await
    .expect("early-disconnect burst stalled the managed accept loop");
    assert_eq!(fixture.ping().await["result"]["service"], identity);
    assert!(child.0.try_wait().unwrap().is_none());
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_eof_without_newline_does_not_dispatch_shutdown() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    let identity = fixture.ping().await["result"]["service"].clone();
    let request = shutdown_request(&identity);
    let mut client = connect(&fixture.endpoint).await;
    timeout(IO_BOUND, client.write_all(request.as_bytes()))
        .await
        .unwrap()
        .unwrap();
    // Give the server time to consume the partial frame before producing EOF.
    sleep(Duration::from_millis(100)).await;
    drop(client);
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_ping_client_held_open_does_not_block_the_next_request() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    fixture.ping().await;
    let mut client = BufReader::new(connect(&fixture.endpoint).await);
    client
        .get_mut()
        .write_all(format!("{PING}\n").as_bytes())
        .await
        .unwrap();
    let response = read_frame(&mut client).await;
    assert_eq!(response["id"], 17);
    assert_eq!(response["result"]["status"], "ok");
    assert!(assert_closed(&mut client, IO_BOUND).await.is_empty());
    // Retain the client's handle during both following connections.
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
    drop(client);
}

#[tokio::test]
async fn managed_connection_dispatches_only_one_frame_even_when_two_are_buffered() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    let identity = fixture.ping().await["result"]["service"].clone();
    let mut client = BufReader::new(connect(&fixture.endpoint).await);
    let requests = format!("{PING}\n{}\n", shutdown_request(&identity));
    client
        .get_mut()
        .write_all(requests.as_bytes())
        .await
        .unwrap();
    assert_eq!(read_frame(&mut client).await["id"], 17);
    assert!(assert_closed(&mut client, IO_BOUND).await.is_empty());
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_malformed_json_does_not_prevent_next_ping_and_shutdown() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    fixture.ping().await;
    let mut client = BufReader::new(connect(&fixture.endpoint).await);
    client.get_mut().write_all(b"{not-json}\n").await.unwrap();
    assert!(read_frame(&mut client).await.get("error").is_some());
    assert!(assert_closed(&mut client, IO_BOUND).await.is_empty());
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_disconnected_response_reader_does_not_stop_the_server() {
    let fixture = Fixture::new();
    fixture.large_index();
    let mut child = fixture.spawn(None);
    fixture.ping().await;
    let mut client = connect(&fixture.endpoint).await;
    client
        .write_all(format!("{}\n", large_search()).as_bytes())
        .await
        .unwrap();
    sleep(Duration::from_millis(100)).await;
    drop(client);
    fixture.ping().await;
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_nonreading_client_has_a_total_write_deadline() {
    let fixture = Fixture::new();
    fixture.large_index();
    let request = large_search();
    assert!(
        handle_json_line(&fixture.index, &request)
            .to_json_line()
            .len()
            > 1024 * 1024
    );
    let mut child = fixture.spawn(None);
    fixture.ping().await;
    let mut client = connect(&fixture.endpoint).await;
    client
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let started = Instant::now();
    // The response exceeds the pipe/socket buffer; keep the handle but never read it.
    sleep(Duration::from_secs(6)).await;
    fixture.ping().await;
    assert!(started.elapsed() < Duration::from_secs(8));
    fixture.shutdown(&mut child).await;
    drop(client);
}

#[tokio::test]
async fn managed_shutdown_rejects_a_different_service_identity() {
    let fixture = Fixture::new();
    let mut child = fixture.spawn(None);
    let identity = fixture.ping().await["result"]["service"].clone();
    assert_eq!(identity["pid"], child.0.id());
    assert_eq!(
        identity["index_path"],
        json!(fs::canonicalize(&fixture.index).unwrap())
    );
    assert_eq!(identity["endpoint"], fixture.endpoint);
    assert!(identity["auto_refresh_seconds"].is_null());
    assert!(!identity["instance_id"].as_str().unwrap().is_empty());
    let started = identity["started_unix_seconds"].as_u64().unwrap();
    assert!(started > 0);
    for field in ["pid", "instance_id", "started_unix_seconds"] {
        let mut wrong = identity.clone();
        wrong[field] = match field {
            "pid" => json!(u64::from(child.0.id()) + 1),
            "instance_id" => json!("different-generation"),
            _ => json!(started + 1),
        };
        let response = fixture.request(&shutdown_request(&wrong)).await;
        assert!(response.get("error").is_some(), "{response}");
        assert_eq!(fixture.ping().await["result"]["service"], identity);
    }
    fixture.shutdown(&mut child).await;
}

#[tokio::test]
async fn managed_endpoint_collision_cannot_replace_an_existing_child() {
    let first = Fixture::new();
    let mut child = first.spawn(None);
    let identity = first.ping().await["result"]["service"].clone();
    let mut second = Fixture::new();
    second.endpoint.clone_from(&first.endpoint);
    let mut rejected = second.spawn(None);
    assert_eq!(rejected.wait().await.code(), Some(1));
    assert!(IndexWriterGuard::acquire(&second.index).is_ok());
    assert_eq!(first.ping().await["result"]["service"], identity);
    assert!(child.0.try_wait().unwrap().is_none());
    first.shutdown(&mut child).await;
}

fn large_search() -> String {
    json!({"id": 20, "method": "search", "params": {"query": "blocked", "limit": 8192}}).to_string()
}

fn shutdown_request(identity: &Value) -> String {
    let params = if identity.is_null() {
        json!({})
    } else {
        json!({"service": identity})
    };
    json!({"id":18,"method":"shutdown","params":params}).to_string()
}

async fn read_frame<S: AsyncRead + Unpin>(client: &mut BufReader<S>) -> Value {
    let mut line = String::new();
    timeout(IO_BOUND, client.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(
        line.ends_with('\n'),
        "response must be a complete frame: {line:?}"
    );
    serde_json::from_str(&line).unwrap()
}

#[cfg(windows)]
type Client = tokio::net::windows::named_pipe::NamedPipeClient;
#[cfg(unix)]
type Client = tokio::net::UnixStream;

async fn connect(endpoint: &str) -> Client {
    timeout(Duration::from_secs(3), async {
        loop {
            #[cfg(windows)]
            let result = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(format!(r"\\.\pipe\{endpoint}"));
            #[cfg(unix)]
            let result = tokio::net::UnixStream::connect(endpoint).await;
            match result {
                Ok(client) => return client,
                Err(error) if retry_connect(&error) => sleep(Duration::from_millis(20)).await,
                Err(error) => panic!("IPC connect failed: {error}"),
            }
        }
    })
    .await
    .expect("IPC endpoint remained unavailable or pipe busy")
}

fn retry_connect(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) || cfg!(windows) && matches!(error.raw_os_error(), Some(2 | 231))
}

fn disconnect_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionAborted
    )
}

async fn assert_closed<S: AsyncRead + Unpin>(client: &mut S, bound: Duration) -> Vec<u8> {
    timeout(bound, async {
        let mut bytes = [0; 1024];
        let mut response = Vec::new();
        loop {
            match client.read(&mut bytes).await {
                Ok(0) => return response,
                Ok(count) => {
                    response.extend_from_slice(&bytes[..count]);
                    assert!(
                        response.len() <= FRAME_CAP,
                        "unexpected excess response data"
                    );
                }
                Err(error) if disconnect_error(&error) => return response,
                Err(error) => panic!("unexpected read error: {error}"),
            }
        }
    })
    .await
    .expect("managed connection was not closed within its deadline")
}

struct ChildGuard(Child);

impl ChildGuard {
    async fn wait(&mut self) -> ExitStatus {
        timeout(IO_BOUND, async {
            loop {
                if let Some(status) = self.0.try_wait().expect("child status should be readable") {
                    return status;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("owned service child did not exit")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Fixture {
    path: PathBuf,
    index: PathBuf,
    state: PathBuf,
    endpoint: String,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = format!("aifs-mt-{}-{nonce}-{sequence}", std::process::id());
        let path = std::env::temp_dir().join(&name);
        fs::create_dir(&path).unwrap();
        let root = path.join("root");
        fs::create_dir(&root).unwrap();
        let index = path.join("index.txt");
        let mut guard = IndexWriterGuard::acquire(&index).unwrap();
        let mut writer = FileIndexWriter::new(&mut guard);
        writer.set_root_path(fs::canonicalize(&root).unwrap());
        writer.set_scan_policy(ScanOptions::default());
        writer.save().unwrap();
        let endpoint = if cfg!(windows) {
            name
        } else {
            path.join("service.sock").to_string_lossy().into_owned()
        };
        Self {
            state: path.join("state.json"),
            path,
            index,
            endpoint,
        }
    }

    fn spawn(&self, auto_refresh: Option<u64>) -> ChildGuard {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ai-file-search-daemon"));
        command
            .env(SERVICE_STATE_ENV, &self.state)
            .current_dir(&self.path)
            .arg("service-run")
            .arg(&self.index)
            .arg(&self.endpoint)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(seconds) = auto_refresh {
            command.args(["--auto-refresh-seconds", &seconds.to_string()]);
        }
        ChildGuard(command.spawn().expect("managed child should spawn"))
    }

    fn large_index(&self) {
        let mut guard = IndexWriterGuard::acquire(&self.index).unwrap();
        let mut writer = FileIndexWriter::open(&mut guard).unwrap();
        writer.replace_all(
            (0..8192)
                .map(|number| IndexedFile {
                    relative_path: PathId::from_user_path(&format!(
                        "blocked-{number:05}-{}.txt",
                        "x".repeat(128)
                    )),
                    size_bytes: 1,
                    modified_unix_seconds: 1_700_000_000,
                })
                .collect(),
        );
        writer.save().unwrap();
    }

    async fn request(&self, request: &str) -> Value {
        let response = timeout(IO_BOUND, async {
            loop {
                match send_ipc_request(&self.endpoint, request).await {
                    Ok(response) => return response,
                    Err(error) if retry_connect(&error) => sleep(Duration::from_millis(20)).await,
                    Err(error) => panic!("IPC request failed: {error}"),
                }
            }
        })
        .await
        .expect("managed service did not respond");
        assert!(response.ends_with('\n'), "{response:?}");
        serde_json::from_str(&response).unwrap()
    }

    async fn ping(&self) -> Value {
        let response = self.request(PING).await;
        assert_eq!(response["id"], 17, "{response}");
        assert_eq!(response["result"]["status"], "ok", "{response}");
        response
    }

    async fn shutdown(&self, child: &mut ChildGuard) {
        let identity = self.ping().await["result"]["service"].clone();
        let response = self.request(&shutdown_request(&identity)).await;
        assert_eq!(response["id"], 18, "{response}");
        assert_eq!(response["result"]["status"], "shutting_down", "{response}");
        assert!(child.wait().await.success());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
