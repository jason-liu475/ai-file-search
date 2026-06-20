use ai_file_search_core::PathId;
use ai_file_search_indexer::{IndexedFile, MemoryIndexStore};

#[test]
fn finds_upserted_file_by_name_substring() {
    let mut store = MemoryIndexStore::new();

    store.upsert_file(indexed_file("Documents/quarterly-report.pdf"));

    let results = store.search_by_name("report");
    let paths = results
        .iter()
        .map(|file| file.relative_path.as_normalized())
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["Documents/quarterly-report.pdf"]);
}

#[test]
fn replaces_existing_file_when_same_path_is_upserted() {
    let mut store = MemoryIndexStore::new();
    let file = indexed_file("Documents/quarterly-report.pdf");

    store.upsert_file(file.clone());
    store.upsert_file(file);

    let results = store.search_by_name("report");

    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].relative_path.as_normalized(),
        "Documents/quarterly-report.pdf"
    );
}

#[test]
fn removes_file_from_search_results() {
    let mut store = MemoryIndexStore::new();
    let path = PathId::from_user_path("Documents/quarterly-report.pdf");

    store.upsert_file(indexed_file(path.as_normalized()));
    store.remove_path(&path);

    let results = store.search_by_name("report");

    assert!(results.is_empty());
}

fn indexed_file(path: &str) -> IndexedFile {
    IndexedFile {
        relative_path: PathId::from_user_path(path),
        size_bytes: 0,
        modified_unix_seconds: 0,
    }
}
