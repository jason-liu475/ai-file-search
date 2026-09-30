use std::fs;
use std::path::{Path, PathBuf};

use ai_file_search_core::PathId;
use ai_file_search_indexer::{FileIndexStore, FileIndexWriter, IndexWriterGuard, IndexedFile};

#[test]
fn finds_saved_file_after_reopening_store() {
    let fixture = TestDir::new("finds_saved_file_after_reopening_store");
    let index_path = fixture.path().join("index.txt");

    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut store = FileIndexWriter::open(&mut guard).expect("store should open");
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

    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut store = FileIndexWriter::open(&mut guard).expect("store should open");
    store.upsert_file(indexed_file(report_path.as_normalized(), 18, 1_700_000_000));
    store.save().expect("store should save initial contents");

    drop(store);
    let mut reopened = FileIndexWriter::open(&mut guard).expect("store should reopen");
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

    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut store = FileIndexWriter::open(&mut guard).expect("store should open");
    store.upsert_file(indexed_file(
        "Documents/quarterly-report.pdf",
        18,
        1_700_000_000,
    ));
    store.save().expect("store should save initial contents");

    drop(store);
    let mut reopened = FileIndexWriter::open(&mut guard).expect("store should reopen");
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
fn root_path_metadata_round_trips_after_reopening_store() {
    let fixture = TestDir::new("root_path_metadata_round_trips_after_reopening_store");
    let index_path = fixture.path().join("index.txt");
    let root_path = fixture.path().join("workspace");

    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut store = FileIndexWriter::open(&mut guard).expect("store should open");
    store.set_root_path(&root_path);
    store.upsert_file(indexed_file(
        "Documents/quarterly-report.pdf",
        18,
        1_700_000_000,
    ));
    store.save().expect("store should save");

    let reopened = FileIndexStore::open(&index_path).expect("store should reopen");

    assert_eq!(reopened.root_path(), Some(root_path.as_path()));
    assert_eq!(reopened.search_by_name("report").len(), 1);
    assert!(
        fs::read_to_string(&index_path)
            .expect("index file should be readable")
            .lines()
            .any(|line| line.starts_with("meta\troot\t"))
    );
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

    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut store = FileIndexWriter::open(&mut guard).expect("store should open");
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

    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut store = FileIndexWriter::open(&mut guard).expect("store should open");
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

    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut store = FileIndexWriter::open(&mut guard).expect("store should open");
    store.upsert_file(indexed_file(
        "Documents/quarterly-report.pdf",
        18,
        1_700_000_000,
    ));
    store.save().expect("store should save");

    assert!(!fixture.path().join("index.txt.tmp").exists());
}

#[test]
fn save_leaves_preexisting_legacy_temporary_file_untouched() {
    let fixture = TestDir::new("legacy-temp-untouched");
    let index_path = fixture.path().join("index.txt");
    let old_temp = fixture.path().join("index.txt.tmp");
    fs::write(&old_temp, b"unrelated contents").unwrap();
    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut store = FileIndexWriter::new(&mut guard);
    store.upsert_file(indexed_file("new.txt", 7, 1));

    store.save().unwrap();

    assert_eq!(fs::read(&old_temp).unwrap(), b"unrelated contents");
    assert_eq!(FileIndexStore::open(&index_path).unwrap().file_count(), 1);
}

#[test]
fn save_does_not_follow_preexisting_legacy_temporary_hard_link() {
    let fixture = TestDir::new("legacy-temp-hard-link");
    let index_path = fixture.path().join("index.txt");
    let target = fixture.path().join("unrelated.txt");
    let old_temp = fixture.path().join("index.txt.tmp");
    fs::write(&target, b"unrelated contents").unwrap();
    fs::hard_link(&target, &old_temp).unwrap();
    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut store = FileIndexWriter::new(&mut guard);
    store.upsert_file(indexed_file("new.txt", 7, 1));

    store.save().unwrap();

    assert_eq!(fs::read(&target).unwrap(), b"unrelated contents");
    assert_eq!(fs::read(&old_temp).unwrap(), b"unrelated contents");
}

#[test]
fn failed_replacement_cleans_only_owned_temporary_file() {
    let fixture = TestDir::new("failed-replacement-cleanup");
    let index_path = fixture.path().join("index.txt");
    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    fs::create_dir(&index_path).unwrap();
    fs::write(index_path.join("marker"), b"keep").unwrap();
    let store = FileIndexWriter::new(&mut guard);

    assert!(store.save().is_err());

    assert_eq!(fs::read(index_path.join("marker")).unwrap(), b"keep");
    let entries = fs::read_dir(fixture.path()).unwrap().count();
    assert_eq!(
        entries, 2,
        "only destination and persistent lock should remain"
    );
}

#[test]
fn cloned_reader_remains_an_independent_snapshot_after_publication() {
    let fixture = TestDir::new("independent-reader-snapshot");
    let index_path = fixture.path().join("index.txt");
    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut writer = FileIndexWriter::new(&mut guard);
    writer.upsert_file(indexed_file("old.txt", 7, 1));
    writer.save().unwrap();
    let snapshot = FileIndexStore::open(&index_path).unwrap().clone();

    writer.replace_all(vec![indexed_file("new.txt", 9, 2)]);
    writer.save().unwrap();

    assert_eq!(snapshot.search_by_name("old").len(), 1);
    assert!(snapshot.search_by_name("new").is_empty());
    let latest = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(latest.search_by_name("new").len(), 1);
    assert_eq!(latest.total_size_bytes(), 9);
}

#[test]
fn writer_new_rebuilds_without_reading_invalid_old_metadata() {
    let fixture = TestDir::new("writer-new-rebuild");
    let index_path = fixture.path().join("index.txt");
    let original = b"aifs-index-v1\nmeta\tscan_policy\tunsupported\n";
    fs::write(&index_path, original).unwrap();
    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    assert_eq!(
        FileIndexWriter::open(&mut guard).err().unwrap().kind(),
        std::io::ErrorKind::InvalidData
    );
    let writer = FileIndexWriter::new(&mut guard);
    assert_eq!(writer.file_count(), 0);
    assert_eq!(fs::read(&index_path).unwrap(), original);
    writer.save().unwrap();
    assert_eq!(fs::read(&index_path).unwrap(), b"aifs-index-v1\n");
}

#[test]
fn publication_preserves_an_open_read_only_snapshot_handle() {
    use std::io::Read;

    let fixture = TestDir::new("read-only-snapshot-handle");
    let index_path = fixture.path().join("index.txt");
    let original = b"aifs-index-v1\n7\t1\told.txt\n";
    fs::write(&index_path, original).unwrap();
    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let mut reader = fs::File::open(&index_path).unwrap();
    let mut writer = FileIndexWriter::new(&mut guard);
    writer.upsert_file(indexed_file("new.txt", 9, 2));

    writer.save().unwrap();

    let mut old_bytes = Vec::new();
    reader.read_to_end(&mut old_bytes).unwrap();
    assert_eq!(old_bytes, original);
    assert_eq!(
        fs::read(&index_path).unwrap(),
        b"aifs-index-v1\n9\t2\tnew.txt\n"
    );
}

#[cfg(windows)]
#[test]
fn replacement_share_mode_failure_preserves_old_snapshot_and_cleans_temp() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = TestDir::new("share-mode-failure");
    let index_path = fixture.path().join("index.txt");
    let original = b"aifs-index-v1\n7\t1\told.txt\n";
    fs::write(&index_path, original).unwrap();
    let mut guard = IndexWriterGuard::acquire(&index_path).unwrap();
    let reader = fs::OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001 | 0x0000_0002)
        .open(&index_path)
        .unwrap();
    let mut writer = FileIndexWriter::new(&mut guard);
    writer.upsert_file(indexed_file("new.txt", 9, 2));

    assert!(writer.save().is_err());

    assert_eq!(fs::read(&index_path).unwrap(), original);
    assert_eq!(fs::read_dir(fixture.path()).unwrap().count(), 2);
    drop(reader);
    writer.save().unwrap();
    assert_eq!(FileIndexStore::open(&index_path).unwrap().file_count(), 1);
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
