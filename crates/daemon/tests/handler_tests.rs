use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_core::PathId;
use ai_file_search_daemon::handle_json_line;
use ai_file_search_indexer::{FileIndexStore, IndexedFile};
use ai_file_search_protocol::Response;

#[test]
fn handler_returns_index_stats() {
    let fixture = TestDir::new("handler_returns_index_stats");
    let index_path = fixture.path().join("index.txt");
    save_index(
        &index_path,
        vec![
            indexed_file("Documents/report.pdf", 6, 1_700_000_000),
            indexed_file("Downloads/archive.zip", 7, 1_700_000_001),
        ],
    );

    let response = handle_json_line(&index_path, r#"{"id":1,"method":"stats","params":{}}"#);

    assert_eq!(
        response.to_json_line(),
        "{\"id\":1,\"result\":{\"files\":2,\"total_bytes\":13}}\n"
    );
}

#[test]
fn handler_returns_limited_search_results() {
    let fixture = TestDir::new("handler_returns_limited_search_results");
    let index_path = fixture.path().join("index.txt");
    save_index(
        &index_path,
        vec![
            indexed_file("Documents/report.pdf", 6, 1_700_000_000),
            indexed_file("Documents/report-draft.pdf", 5, 1_700_000_001),
        ],
    );

    let response = handle_json_line(
        &index_path,
        r#"{"id":2,"method":"search","params":{"query":"report","limit":1}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":2,\"result\":{\"files\":[{\"modified_unix_seconds\":1700000001,\"path\":\"Documents/report-draft.pdf\",\"size_bytes\":5}]}}\n"
    );
}

#[test]
fn handler_rejects_unknown_methods() {
    let fixture = TestDir::new("handler_rejects_unknown_methods");
    let response = handle_json_line(
        &fixture.path().join("index.txt"),
        r#"{"id":3,"method":"refresh","params":{}}"#,
    );

    assert_eq!(response, Response::error(3, "unknown method: refresh"));
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ai-file-search-daemon-{name}-{}",
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
