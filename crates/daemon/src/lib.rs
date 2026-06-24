use std::io;
use std::path::Path;

use ai_file_search_indexer::FileIndexStore;
use ai_file_search_protocol::{Request, Response};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[must_use]
pub fn handle_json_line(index_path: &Path, line: &str) -> Response {
    let request = match Request::from_json_line(line) {
        Ok(request) => request,
        Err(error) => return Response::error(0, format!("invalid request: {error}")),
    };

    match request.method.as_str() {
        "stats" => stats(index_path, request.id),
        "search" => search(index_path, &request),
        method => Response::error(request.id, format!("unknown method: {method}")),
    }
}

fn stats(index_path: &Path, id: u64) -> Response {
    let store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => return Response::error(id, format!("index open failed: {error}")),
    };

    Response::success(
        id,
        json!({
            "files": store.file_count(),
            "total_bytes": store.total_size_bytes(),
        }),
    )
}

fn search(index_path: &Path, request: &Request) -> Response {
    let Some(query) = request.params.get("query").and_then(|query| query.as_str()) else {
        return Response::error(request.id, "missing string param: query");
    };
    let limit = request
        .params
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .and_then(|limit| usize::try_from(limit).ok())
        .unwrap_or(20);

    let store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => return Response::error(request.id, format!("index open failed: {error}")),
    };
    let files = store
        .search_by_name(query)
        .into_iter()
        .take(limit)
        .map(|file| {
            json!({
                "path": file.relative_path.as_normalized(),
                "size_bytes": file.size_bytes,
                "modified_unix_seconds": file.modified_unix_seconds,
            })
        })
        .collect::<Vec<_>>();

    Response::success(request.id, json!({ "files": files }))
}

/// Handles newline-delimited JSON-RPC requests on a bidirectional async stream.
///
/// # Errors
///
/// Returns an I/O error when the stream cannot be read from or written to.
pub async fn handle_json_stream<S>(index_path: &Path, stream: S) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = BufReader::new(stream);
    let mut line = String::new();

    loop {
        line.clear();
        let bytes_read = stream.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Ok(());
        }

        let response = handle_json_line(index_path, &line);
        stream
            .get_mut()
            .write_all(response.to_json_line().as_bytes())
            .await?;
        stream.get_mut().flush().await?;
    }
}

/// Sends one newline-delimited JSON-RPC request on a bidirectional async stream.
///
/// # Errors
///
/// Returns an I/O error when the stream cannot be written to or read from.
pub async fn send_json_request<S>(stream: S, request: &str) -> io::Result<String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut stream = stream;
    stream.write_all(request.as_bytes()).await?;
    if !request.ends_with('\n') {
        stream.write_all(b"\n").await?;
    }
    stream.flush().await?;

    let mut stream = BufReader::new(stream);
    let mut response = String::new();
    stream.read_line(&mut response).await?;

    Ok(response)
}

#[cfg(windows)]
fn pipe_name(endpoint: &str) -> String {
    const PIPE_PREFIX: &str = r"\\.\pipe\";
    if endpoint.starts_with(PIPE_PREFIX) {
        endpoint.to_owned()
    } else {
        format!("{PIPE_PREFIX}{endpoint}")
    }
}

/// Serves JSON-RPC requests over the platform IPC transport.
///
/// # Errors
///
/// Returns an I/O error when the endpoint cannot be created or a client stream
/// cannot be handled.
#[cfg(windows)]
pub async fn serve_ipc(index_path: &Path, endpoint: &str) -> io::Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    let endpoint = pipe_name(endpoint);
    loop {
        let server = ServerOptions::new().create(&endpoint)?;
        server.connect().await?;
        handle_json_stream(index_path, server).await?;
    }
}

/// Sends one JSON-RPC request over the platform IPC transport.
///
/// # Errors
///
/// Returns an I/O error when the endpoint cannot be opened or used.
#[cfg(windows)]
pub async fn send_ipc_request(endpoint: &str, request: &str) -> io::Result<String> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let client = ClientOptions::new().open(pipe_name(endpoint))?;
    send_json_request(client, request).await
}

/// Serves JSON-RPC requests over the platform IPC transport.
///
/// # Errors
///
/// Returns an I/O error when the endpoint cannot be created or a client stream
/// cannot be handled.
#[cfg(unix)]
pub async fn serve_ipc(index_path: &Path, endpoint: &str) -> io::Result<()> {
    use tokio::net::UnixListener;

    let _ = std::fs::remove_file(endpoint);
    let listener = UnixListener::bind(endpoint)?;
    loop {
        let (stream, _) = listener.accept().await?;
        handle_json_stream(index_path, stream).await?;
    }
}

/// Sends one JSON-RPC request over the platform IPC transport.
///
/// # Errors
///
/// Returns an I/O error when the endpoint cannot be opened or used.
#[cfg(unix)]
pub async fn send_ipc_request(endpoint: &str, request: &str) -> io::Result<String> {
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(endpoint).await?;
    send_json_request(stream, request).await
}
