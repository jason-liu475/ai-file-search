use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_core::PathId;
use ai_file_search_daemon::handle_json_line;
use ai_file_search_indexer::{FileIndexStore, IndexedFile, ScanOptions, Scanner};
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
        r#"{"id":3,"method":"open","params":{}}"#,
    );

    assert_eq!(response, Response::error(3, "unknown method: open"));
}

#[test]
fn handler_returns_ping_status() {
    let fixture = TestDir::new("handler_returns_ping_status");
    let response = handle_json_line(
        &fixture.path().join("index.txt"),
        r#"{"id":4,"method":"ping","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":4,\"result\":{\"status\":\"ok\"}}\n"
    );
}

#[test]
fn handler_returns_shutdown_status() {
    let fixture = TestDir::new("handler_returns_shutdown_status");
    let response = handle_json_line(
        &fixture.path().join("index.txt"),
        r#"{"id":5,"method":"shutdown","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":5,\"result\":{\"status\":\"shutting_down\"}}\n"
    );
}

#[test]
fn handler_returns_method_catalog() {
    let fixture = TestDir::new("handler_returns_method_catalog");
    let response = handle_json_line(
        &fixture.path().join("index.txt"),
        r#"{"id":6,"method":"methods","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        concat!(
            "{\"id\":6,\"result\":{",
            "\"methods\":[",
            "{\"name\":\"methods\",\"params\":{}},",
            "{\"name\":\"ping\",\"params\":{}},",
            "{\"name\":\"index_status\",\"params\":{\"exclude_names\":\"optional string array\",\"root\":\"optional string with stored root metadata (if supplied, must match); otherwise required\"}},",
            "{\"name\":\"refresh\",\"params\":{\"exclude_names\":\"optional string array\",\"root\":\"optional string; must match stored root\"}},",
            "{\"name\":\"reindex\",\"params\":{\"exclude_names\":\"optional string array\",\"root\":\"optional string; must match stored root\"}},",
            "{\"name\":\"search\",\"params\":{\"limit\":\"optional u64 default 20\",\"query\":\"string\"}},",
            "{\"name\":\"shutdown\",\"params\":{}},",
            "{\"name\":\"stats\",\"params\":{}}",
            "],\"protocol\":\"ai-file-search-json-rpc\",\"version\":1}}\n"
        )
    );
}

#[test]
fn handler_refreshes_index_from_root() {
    let fixture = TestDir::new("handler_refreshes_index_from_root");
    let root = fixture.path().join("root");
    fs::create_dir_all(root.join("Documents")).expect("documents fixture should be created");
    fs::create_dir_all(root.join("node_modules")).expect("excluded fixture should be created");
    fs::write(root.join("Documents").join("report.pdf"), "report")
        .expect("report fixture should be written");
    fs::write(root.join("node_modules").join("ignored.txt"), "ignored")
        .expect("ignored fixture should be written");

    let index_path = fixture.path().join("index.txt");
    save_index(&index_path, vec![indexed_file("stale.txt", 1, 1)]);
    let request = serde_json::json!({
        "id": 7,
        "method": "refresh",
        "params": {
            "root": root.to_string_lossy(),
            "exclude_names": ["node_modules"],
        }
    })
    .to_string();

    let response = handle_json_line(&index_path, &request);

    assert_eq!(
        response.to_json_line(),
        "{\"id\":7,\"result\":{\"added\":1,\"removed\":1,\"scanned_files\":1,\"unchanged\":0,\"updated\":0}}\n"
    );
    let store = FileIndexStore::open(&index_path).expect("refreshed store should open");
    assert_eq!(store.file_count(), 1);
    assert_eq!(store.search_by_name("report").len(), 1);
    assert!(store.search_by_name("ignored").is_empty());
}

#[test]
fn handler_reindexes_index_from_root() {
    let fixture = TestDir::new("handler_reindexes_index_from_root");
    let root = fixture.path().join("root");
    fs::create_dir_all(root.join("Documents")).expect("documents fixture should be created");
    fs::write(root.join("Documents").join("final.pdf"), "final")
        .expect("final fixture should be written");

    let index_path = fixture.path().join("index.txt");
    save_index(&index_path, vec![indexed_file("stale.txt", 1, 1)]);
    let request = serde_json::json!({
        "id": 8,
        "method": "reindex",
        "params": {
            "root": root.to_string_lossy(),
        }
    })
    .to_string();

    let response = handle_json_line(&index_path, &request);

    assert_eq!(
        response.to_json_line(),
        "{\"id\":8,\"result\":{\"added\":1,\"removed\":1,\"scanned_files\":1,\"unchanged\":0,\"updated\":0}}\n"
    );
    let store = FileIndexStore::open(&index_path).expect("reindexed store should open");
    assert_eq!(store.file_count(), 1);
    assert_eq!(store.search_by_name("final").len(), 1);
    assert!(store.search_by_name("stale").is_empty());
}

#[test]
fn handler_reindexes_from_stored_root_when_root_param_is_omitted() {
    let fixture = TestDir::new("handler_reindexes_from_stored_root_when_root_param_is_omitted");
    let root = fixture.path().join("root");
    fs::create_dir_all(root.join("Documents")).expect("documents fixture should be created");
    fs::write(root.join("Documents").join("stored-root.pdf"), "stored")
        .expect("stored root fixture should be written");

    let index_path = fixture.path().join("index.txt");
    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.set_root_path(&root);
    store.replace_all(vec![indexed_file("stale.txt", 1, 1)]);
    store.save().expect("store should save");

    let response = handle_json_line(&index_path, r#"{"id":9,"method":"reindex","params":{}}"#);

    assert_eq!(
        response.to_json_line(),
        "{\"id\":9,\"result\":{\"added\":1,\"removed\":1,\"scanned_files\":1,\"unchanged\":0,\"updated\":0}}\n"
    );
    let reindexed = FileIndexStore::open(&index_path).expect("reindexed store should open");
    assert_eq!(reindexed.file_count(), 1);
    assert_eq!(reindexed.root_path(), Some(root.as_path()));
    assert_eq!(reindexed.search_by_name("stored-root").len(), 1);
    assert!(reindexed.search_by_name("stale").is_empty());
}

#[test]
fn handler_rejects_refresh_root_that_differs_from_stored_root() {
    let fixture = TestDir::new("handler_rejects_refresh_root_that_differs_from_stored_root");
    let allowed_root = fixture.path().join("allowed-root");
    let denied_root = fixture.path().join("denied-root");
    fs::create_dir_all(allowed_root.join("Documents")).expect("allowed root should be created");
    fs::create_dir_all(denied_root.join("Documents")).expect("denied root should be created");
    fs::write(denied_root.join("Documents").join("secret.pdf"), "secret")
        .expect("denied root fixture should be written");

    let index_path = fixture.path().join("index.txt");
    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.set_root_path(&allowed_root);
    store.replace_all(vec![indexed_file("stale.txt", 1, 1)]);
    store.save().expect("store should save");
    let request = serde_json::json!({
        "id": 10,
        "method": "refresh",
        "params": {
            "root": denied_root.to_string_lossy(),
        }
    })
    .to_string();

    let response = handle_json_line(&index_path, &request);

    assert_eq!(
        response.to_json_line(),
        "{\"id\":10,\"error\":{\"message\":\"root does not match stored index root\"}}\n"
    );
    let unchanged = FileIndexStore::open(&index_path).expect("unchanged store should open");
    assert_eq!(unchanged.root_path(), Some(allowed_root.as_path()));
    assert_eq!(unchanged.search_by_name("stale").len(), 1);
    assert!(unchanged.search_by_name("secret").is_empty());
}

#[test]
fn handler_returns_current_index_status_without_rewriting_index() {
    let fixture = TestDir::new("handler_returns_current_index_status_without_rewriting_index");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).expect("root fixture should be created");
    fs::write(root.join("current.txt"), "current").expect("current fixture should be written");

    let index_path = root.join("index.txt");
    save_scanned_index(&index_path, &root);
    let index_before = fs::read(&index_path).expect("index should be readable before status");

    let response = handle_json_line(
        &index_path,
        r#"{"id":11,"method":"index_status","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":11,\"result\":{\"added\":0,\"needs_refresh\":false,\"removed\":0,\"scanned_files\":1,\"unchanged\":1,\"updated\":0}}\n"
    );
    assert_eq!(
        fs::read(&index_path).expect("index should be readable after status"),
        index_before
    );
}

#[test]
fn handler_excludes_absolute_index_path_for_relative_stored_root() {
    let fixture = TestDir::new_relative("index-status");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).expect("root fixture should be created");
    fs::write(root.join("current.txt"), "current").expect("current fixture should be written");

    let absolute_root = std::env::current_dir()
        .expect("current directory should be available")
        .join(&root);
    let index_path = absolute_root.join("index.txt");
    save_scanned_index(&index_path, &root);
    let index_before = fs::read(&index_path).expect("index should be readable before status");

    let response = handle_json_line(
        &index_path,
        r#"{"id":16,"method":"index_status","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":16,\"result\":{\"added\":0,\"needs_refresh\":false,\"removed\":0,\"scanned_files\":1,\"unchanged\":1,\"updated\":0}}\n"
    );
    assert_eq!(
        fs::read(&index_path).expect("index should be readable after status"),
        index_before
    );
}

#[test]
fn handler_returns_stale_index_status_without_rewriting_index() {
    let fixture = TestDir::new("handler_returns_stale_index_status_without_rewriting_index");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).expect("root fixture should be created");
    fs::write(root.join("unchanged.txt"), "unchanged")
        .expect("unchanged fixture should be written");
    fs::write(root.join("updated.txt"), "old").expect("updated fixture should be written");
    fs::write(root.join("removed.txt"), "removed").expect("removed fixture should be written");

    let index_path = fixture.path().join("index.txt");
    save_scanned_index(&index_path, &root);
    fs::write(root.join("updated.txt"), "new content")
        .expect("updated fixture should be rewritten");
    fs::remove_file(root.join("removed.txt")).expect("removed fixture should be removed");
    fs::write(root.join("added.txt"), "added").expect("added fixture should be written");
    let index_before = fs::read(&index_path).expect("index should be readable before status");

    let response = handle_json_line(
        &index_path,
        r#"{"id":12,"method":"index_status","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":12,\"result\":{\"added\":1,\"needs_refresh\":true,\"removed\":1,\"scanned_files\":3,\"unchanged\":1,\"updated\":1}}\n"
    );
    assert_eq!(
        fs::read(&index_path).expect("index should be readable after status"),
        index_before
    );
}

#[test]
fn handler_rejects_index_status_root_that_differs_from_stored_root() {
    let fixture = TestDir::new("handler_rejects_index_status_root_that_differs_from_stored_root");
    let allowed_root = fixture.path().join("allowed-root");
    let denied_root = fixture.path().join("denied-root");
    fs::create_dir_all(&allowed_root).expect("allowed root should be created");
    fs::create_dir_all(&denied_root).expect("denied root should be created");

    let index_path = fixture.path().join("index.txt");
    save_scanned_index(&index_path, &allowed_root);
    let index_before = fs::read(&index_path).expect("index should be readable before status");
    let request = serde_json::json!({
        "id": 13,
        "method": "index_status",
        "params": {
            "root": denied_root.to_string_lossy(),
        }
    })
    .to_string();

    let response = handle_json_line(&index_path, &request);

    assert_eq!(
        response.to_json_line(),
        "{\"id\":13,\"error\":{\"message\":\"root does not match stored index root\"}}\n"
    );
    assert_eq!(
        fs::read(&index_path).expect("index should be readable after status"),
        index_before
    );
}

#[test]
fn handler_requires_index_status_root_without_metadata() {
    let fixture = TestDir::new("handler_requires_index_status_root_without_metadata");
    let index_path = fixture.path().join("index.txt");
    save_index(&index_path, Vec::new());

    let response = handle_json_line(
        &index_path,
        r#"{"id":14,"method":"index_status","params":{}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":14,\"error\":{\"message\":\"missing string param: root\"}}\n"
    );
}

#[test]
fn handler_rejects_invalid_index_status_exclusions() {
    let fixture = TestDir::new("handler_rejects_invalid_index_status_exclusions");
    let root = fixture.path().join("root");
    fs::create_dir_all(&root).expect("root fixture should be created");

    let index_path = fixture.path().join("index.txt");
    save_scanned_index(&index_path, &root);
    let response = handle_json_line(
        &index_path,
        r#"{"id":15,"method":"index_status","params":{"exclude_names":"target"}}"#,
    );

    assert_eq!(
        response.to_json_line(),
        "{\"id\":15,\"error\":{\"message\":\"exclude_names must be an array of strings\"}}\n"
    );
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

    fn new_relative(name: &str) -> Self {
        let path = PathBuf::from("target")
            .join(format!("aifs-relative-root-{}-{name}", std::process::id()));

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

fn save_scanned_index(index_path: &Path, root: &Path) {
    let files = Scanner::new(ScanOptions::default())
        .scan(root)
        .expect("root fixture should scan");
    let mut store = FileIndexStore::open(index_path).expect("store should open");
    store.set_root_path(root);
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
