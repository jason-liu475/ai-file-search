use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_core::PathId;
use ai_file_search_indexer::{FileIndexStore, IndexedFile};

#[test]
fn finds_saved_file_after_reopening_store() {
    let fixture = TestDir::new("finds_saved_file_after_reopening_store");
    let index_path = fixture.path().join("index.txt");

    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.upsert_file(indexed_file(
        "Documents/quarterly-report.pdf",
        18,
        1_700_000_000,
    ));
    store.save().expect("store should save");

    let reopened = FileIndexStore::open(&index_path).expect("store should reopen");
    let results = reopened.search_by_name("report");
    let paths = results
        .iter()
        .map(|file| file.relative_path.as_normalized())
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["Documents/quarterly-report.pdf"]);
    assert_eq!(results[0].size_bytes, 18);
    assert_eq!(results[0].modified_unix_seconds, 1_700_000_000);
    assert_eq!(
        fs::read_to_string(index_path).expect("index file should be readable"),
        "aifs-index-v1\n18\t1700000000\tDocuments/quarterly-report.pdf\n"
    );
}

#[test]
fn removed_file_stays_removed_after_reopening_store() {
    let fixture = TestDir::new("removed_file_stays_removed_after_reopening_store");
    let index_path = fixture.path().join("index.txt");
    let report_path = PathId::from_user_path("Documents/quarterly-report.pdf");

    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.upsert_file(indexed_file(report_path.as_normalized(), 18, 1_700_000_000));
    store.save().expect("store should save initial contents");

    let mut reopened = FileIndexStore::open(&index_path).expect("store should reopen");
    reopened.remove_path(&report_path);
    reopened.save().expect("store should save removal");

    let reopened_again = FileIndexStore::open(&index_path).expect("store should reopen again");
    let results = reopened_again.search_by_name("report");

    assert!(results.is_empty());
}

#[test]
fn replaced_files_are_saved_after_reopening_store() {
    let fixture = TestDir::new("replaced_files_are_saved_after_reopening_store");
    let index_path = fixture.path().join("index.txt");

    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.upsert_file(indexed_file(
        "Documents/quarterly-report.pdf",
        18,
        1_700_000_000,
    ));
    store.save().expect("store should save initial contents");

    let mut reopened = FileIndexStore::open(&index_path).expect("store should reopen");
    reopened.replace_all(vec![indexed_file(
        "Downloads/archive.zip",
        11,
        1_700_000_001,
    )]);
    reopened.save().expect("store should save replacement");

    let reopened_again = FileIndexStore::open(&index_path).expect("store should reopen again");
    let report_results = reopened_again.search_by_name("report");
    let archive_results = reopened_again.search_by_name("archive");

    assert!(report_results.is_empty());
    assert_eq!(archive_results.len(), 1);
    assert_eq!(
        archive_results[0].relative_path.as_normalized(),
        "Downloads/archive.zip"
    );
    assert_eq!(archive_results[0].size_bytes, 11);
    assert_eq!(archive_results[0].modified_unix_seconds, 1_700_000_001);
}

#[test]
fn opens_legacy_path_only_index_files() {
    let fixture = TestDir::new("opens_legacy_path_only_index_files");
    let index_path = fixture.path().join("index.txt");
    fs::write(&index_path, "Documents/quarterly-report.pdf\n").expect("legacy index should write");

    let store = FileIndexStore::open(&index_path).expect("legacy store should open");
    let results = store.search_by_name("report");

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].relative_path.as_normalized(),
        "Documents/quarterly-report.pdf"
    );
    assert_eq!(results[0].size_bytes, 0);
    assert_eq!(results[0].modified_unix_seconds, 0);
}

#[test]
fn preserves_tabs_in_paths_when_reopening_store() {
    let fixture = TestDir::new("preserves_tabs_in_paths_when_reopening_store");
    let index_path = fixture.path().join("index.txt");

    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.upsert_file(indexed_file(
        "Documents/quarterly\treport.pdf",
        18,
        1_700_000_000,
    ));
    store.save().expect("store should save");

    let reopened = FileIndexStore::open(&index_path).expect("store should reopen");
    let results = reopened.search_by_name("report");

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].relative_path.as_normalized(),
        "Documents/quarterly\treport.pdf"
    );
    assert_eq!(results[0].size_bytes, 18);
    assert_eq!(results[0].modified_unix_seconds, 1_700_000_000);
}

#[cfg(unix)]
#[test]
fn save_replaces_read_only_index_file() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = TestDir::new("save_replaces_read_only_index_file");
    let index_path = fixture.path().join("index.txt");
    fs::write(&index_path, "Documents/old-report.pdf\n").expect("old index should write");
    fs::set_permissions(&index_path, fs::Permissions::from_mode(0o444))
        .expect("old index should become read-only");

    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.replace_all(vec![indexed_file(
        "Documents/new-report.pdf",
        18,
        1_700_000_000,
    )]);

    store
        .save()
        .expect("store should replace the read-only index file");

    let reopened = FileIndexStore::open(&index_path).expect("store should reopen");
    let results = reopened.search_by_name("new");

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].relative_path.as_normalized(),
        "Documents/new-report.pdf"
    );
}

#[test]
fn save_does_not_leave_temporary_index_file() {
    let fixture = TestDir::new("save_does_not_leave_temporary_index_file");
    let index_path = fixture.path().join("index.txt");

    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.upsert_file(indexed_file(
        "Documents/quarterly-report.pdf",
        18,
        1_700_000_000,
    ));
    store.save().expect("store should save");

    assert!(!fixture.path().join("index.txt.tmp").exists());
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ai-file-search-file-store-{name}-{}",
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

fn indexed_file(path: &str, size_bytes: u64, modified_unix_seconds: u64) -> IndexedFile {
    IndexedFile {
        relative_path: PathId::from_user_path(path),
        size_bytes,
        modified_unix_seconds,
    }
}
