use ai_file_search_core::PathId;
use ai_file_search_indexer::{IndexedFile, RefreshSummary};

#[test]
fn compares_added_updated_removed_and_unchanged_files() {
    let old_files = vec![
        indexed_file("Documents/unchanged.txt", 10, 100),
        indexed_file("Documents/updated.txt", 10, 100),
        indexed_file("Documents/removed.txt", 10, 100),
    ];
    let new_files = vec![
        indexed_file("Documents/unchanged.txt", 10, 100),
        indexed_file("Documents/updated.txt", 11, 101),
        indexed_file("Documents/added.txt", 10, 100),
    ];

    let summary = RefreshSummary::compare(&old_files, &new_files);

    assert_eq!(summary.added, 1);
    assert_eq!(summary.updated, 1);
    assert_eq!(summary.removed, 1);
    assert_eq!(summary.unchanged, 1);
}

fn indexed_file(path: &str, size_bytes: u64, modified_unix_seconds: u64) -> IndexedFile {
    IndexedFile {
        relative_path: PathId::from_user_path(path),
        size_bytes,
        modified_unix_seconds,
    }
}
