use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_core::PathId;
use ai_file_search_daemon::handle_json_stream;
use ai_file_search_indexer::{FileIndexStore, IndexedFile};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[tokio::test]
async fn stream_handler_serves_multiple_json_rpc_lines() {
    let fixture = TestDir::new("stream_handler_serves_multiple_json_rpc_lines");
    let index_path = fixture.path().join("index.txt");
    save_index(
        &index_path,
        vec![indexed_file("Documents/report.pdf", 6, 1_700_000_000)],
    );

    let (client, server) = tokio::io::duplex(4096);
    let handler_index_path = index_path.clone();
    let handler = tokio::spawn(async move {
        handle_json_stream(&handler_index_path, server)
            .await
            .expect("stream handler should finish cleanly");
    });

    let mut client = BufReader::new(client);
    client
        .get_mut()
        .write_all(b"{\"id\":1,\"method\":\"stats\",\"params\":{}}\n")
        .await
        .expect("request should write");
    client
        .get_mut()
        .write_all(b"{\"id\":2,\"method\":\"search\",\"params\":{\"query\":\"report\"}}\n")
        .await
        .expect("request should write");
    client
        .get_mut()
        .shutdown()
        .await
        .expect("client should shutdown writes");

    let mut first_response = String::new();
    client
        .read_line(&mut first_response)
        .await
        .expect("first response should read");
    let mut second_response = String::new();
    client
        .read_line(&mut second_response)
        .await
        .expect("second response should read");

    assert_eq!(
        first_response,
        "{\"id\":1,\"result\":{\"files\":1,\"total_bytes\":6}}\n"
    );
    assert_eq!(
        second_response,
        "{\"id\":2,\"result\":{\"files\":[{\"modified_unix_seconds\":1700000000,\"path\":\"Documents/report.pdf\",\"size_bytes\":6}]}}\n"
    );

    handler.await.expect("handler task should join");
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ai-file-search-daemon-transport-{name}-{}",
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

fn save_index(index_path: &Path, files: Vec<IndexedFile>) {
    let mut store = FileIndexStore::open(index_path).expect("store should open");
    store.replace_all(files);
    store.save().expect("store should save");
}

fn indexed_file(path: &str, size_bytes: u64, modified_unix_seconds: u64) -> IndexedFile {
    IndexedFile {
        relative_path: PathId::from_user_path(path),
        size_bytes,
        modified_unix_seconds,
    }
}
