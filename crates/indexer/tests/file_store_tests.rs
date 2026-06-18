use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_core::PathId;
use ai_file_search_indexer::{FileIndexStore, IndexedFile};

#[test]
fn finds_saved_file_after_reopening_store() {
    let fixture = TestDir::new("finds_saved_file_after_reopening_store");
    let index_path = fixture.path().join("index.txt");

    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.upsert_file(IndexedFile {
        relative_path: PathId::from_user_path("Documents/quarterly-report.pdf"),
    });
    store.save().expect("store should save");

    let reopened = FileIndexStore::open(&index_path).expect("store should reopen");
    let results = reopened.search_by_name("report");
    let paths = results
        .iter()
        .map(|file| file.relative_path.as_normalized())
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["Documents/quarterly-report.pdf"]);
}

#[test]
fn removed_file_stays_removed_after_reopening_store() {
    let fixture = TestDir::new("removed_file_stays_removed_after_reopening_store");
    let index_path = fixture.path().join("index.txt");
    let report_path = PathId::from_user_path("Documents/quarterly-report.pdf");

    let mut store = FileIndexStore::open(&index_path).expect("store should open");
    store.upsert_file(IndexedFile {
        relative_path: report_path.clone(),
    });
    store.save().expect("store should save initial contents");

    let mut reopened = FileIndexStore::open(&index_path).expect("store should reopen");
    reopened.remove_path(&report_path);
    reopened.save().expect("store should save removal");

    let reopened_again = FileIndexStore::open(&index_path).expect("store should reopen again");
    let results = reopened_again.search_by_name("report");

    assert!(results.is_empty());
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
