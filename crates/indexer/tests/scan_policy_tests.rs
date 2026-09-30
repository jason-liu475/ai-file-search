use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ai_file_search_indexer::{FileIndexStore, ScanOptions};

#[test]
fn scan_policy_round_trips_escaped_names_in_sorted_order() {
    let fixture = TestDir::new("escaped-policy");
    let index_path = fixture.path().join("index.txt");
    let options = ScanOptions::default()
        .exclude_name("tabs\tand\nnewlines\r")
        .exclude_name("back\\slash")
        .exclude_name(".git")
        .exclude_name(".git");
    let mut store = FileIndexStore::new(&index_path);
    store.set_root_path("root\\with\ttabs\n");
    store.set_scan_policy(options.clone());
    store.save().unwrap();

    let reopened = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(reopened.scan_policy(), Some(&options));
    assert_eq!(reopened.root_path(), Some(Path::new("root\\with\ttabs\n")));
    assert_eq!(
        reopened
            .scan_policy()
            .unwrap()
            .excluded_names()
            .collect::<Vec<_>>(),
        vec![".git", "back\\slash", "tabs\tand\nnewlines\r"]
    );
}

#[test]
fn legacy_unknown_policy_is_distinct_from_explicitly_empty() {
    let fixture = TestDir::new("legacy-policy");
    let index_path = fixture.path().join("index.txt");
    for contents in ["readme.txt\n", "aifs-index-v1\n7\t1\treadme.txt\n"] {
        fs::write(&index_path, contents).unwrap();
        let store = FileIndexStore::open(&index_path).unwrap();
        assert!(store.scan_policy().is_none());
        assert_eq!(
            store.resolve_scan_options(None).unwrap(),
            ScanOptions::default()
        );
        store.save().unwrap();
        assert!(
            FileIndexStore::open(&index_path)
                .unwrap()
                .scan_policy()
                .is_none()
        );
    }

    let mut store = FileIndexStore::new(&index_path);
    store.set_scan_policy(ScanOptions::default());
    store.save().unwrap();
    assert_eq!(
        FileIndexStore::open(&index_path).unwrap().scan_policy(),
        Some(&ScanOptions::default())
    );
}

#[test]
fn known_policy_is_inherited_and_explicit_matching_sets_are_accepted() {
    let fixture = TestDir::new("resolve-known");
    let index_path = fixture.path().join("index.txt");
    let options = ScanOptions::default()
        .exclude_name(".git")
        .exclude_name("private");
    let mut store = FileIndexStore::new(&index_path);
    store.set_scan_policy(options.clone());
    assert_eq!(store.resolve_scan_options(None).unwrap(), options);
    assert_eq!(
        store
            .resolve_scan_options(Some(
                ScanOptions::default()
                    .exclude_name("private")
                    .exclude_name(".git")
                    .exclude_name(".git")
            ))
            .unwrap(),
        options
    );
    assert_eq!(
        store.resolve_scan_options(Some(ScanOptions::default())),
        Err("exclude_names does not match stored scan policy")
    );
    assert_eq!(
        store.resolve_scan_options(Some(ScanOptions::default().exclude_name("different"))),
        Err("exclude_names does not match stored scan policy")
    );
}

#[test]
fn legacy_explicit_scope_does_not_confirm_policy() {
    let fixture = TestDir::new("resolve-legacy");
    let index_path = fixture.path().join("index.txt");
    let store = FileIndexStore::new(&index_path);
    let options = ScanOptions::default().exclude_name("private");
    assert_eq!(
        store.resolve_scan_options(Some(options.clone())).unwrap(),
        options
    );
    assert!(store.scan_policy().is_none());
}

#[test]
fn known_empty_policy_rejects_new_exclusions() {
    let fixture = TestDir::new("resolve-empty");
    let mut store = FileIndexStore::new(&fixture.path().join("index.txt"));
    store.set_scan_policy(ScanOptions::default());
    assert_eq!(
        store.resolve_scan_options(None).unwrap(),
        ScanOptions::default()
    );
    assert_eq!(
        store.resolve_scan_options(Some(ScanOptions::default().exclude_name("private"))),
        Err("exclude_names does not match stored scan policy")
    );
}

#[test]
fn new_store_does_not_read_or_modify_existing_snapshot() {
    let fixture = TestDir::new("new-destination");
    let index_path = fixture.path().join("index.txt");
    let contents = "aifs-index-v1\nmeta\tscan_policy\tunsupported\n";
    fs::write(&index_path, contents).unwrap();

    let store = FileIndexStore::new(&index_path);
    assert_eq!(store.file_count(), 0);
    assert!(store.root_path().is_none());
    assert!(store.scan_policy().is_none());
    assert_eq!(fs::read_to_string(&index_path).unwrap(), contents);
}

#[test]
fn policy_metadata_order_does_not_change_scope() {
    let fixture = TestDir::new("record-order");
    let index_path = fixture.path().join("index.txt");
    fs::write(
        &index_path,
        "aifs-index-v1\nmeta\texclude_name\tprivate\n7\t1\treadme.txt\nmeta\tscan_policy\t1\n",
    )
    .unwrap();

    let store = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(
        store.scan_policy(),
        Some(&ScanOptions::default().exclude_name("private"))
    );
    assert_eq!(store.file_count(), 1);
}

#[test]
fn stored_scan_policy_survives_open_and_save() {
    let fixture = TestDir::new("policy-preservation");
    let index_path = fixture.path().join("index.txt");
    fs::write(
        &index_path,
        "aifs-index-v1\nmeta\troot\tworkspace\nmeta\tscan_policy\t1\nmeta\texclude_name\tnode_modules\nmeta\texclude_name\t.git\nmeta\texclude_name\t.git\n7\t1\treadme.txt\n",
    )
    .unwrap();

    let store = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(store.file_count(), 1);
    store.save().unwrap();

    let contents = fs::read_to_string(&index_path).unwrap();
    assert_eq!(
        contents,
        "aifs-index-v1\nmeta\troot\tworkspace\nmeta\tscan_policy\t1\nmeta\texclude_name\t.git\nmeta\texclude_name\tnode_modules\n7\t1\treadme.txt\n"
    );
}

#[test]
fn explicitly_empty_scan_policy_survives_save() {
    let fixture = TestDir::new("empty-policy");
    let index_path = fixture.path().join("index.txt");
    let contents = "aifs-index-v1\nmeta\tscan_policy\t1\n";
    fs::write(&index_path, contents).unwrap();

    FileIndexStore::open(&index_path).unwrap().save().unwrap();

    assert_eq!(fs::read_to_string(&index_path).unwrap(), contents);
}

#[test]
fn malformed_scan_policy_records_are_rejected_without_writing() {
    let fixture = TestDir::new("invalid-policy");
    let index_path = fixture.path().join("index.txt");
    let records = [
        "meta\tscan_policy\t2",
        "meta\tscan_policy\t",
        "meta\tscan_policy",
        "meta\tscan_policy\t1\tunexpected",
        "meta\tscan_policy\t1\nmeta\tscan_policy\t1",
        "meta\tscan_policy\t1\nmeta\tscan_policy\t2",
        "meta\texclude_name\t.git",
        "meta\tscan_policy\t1\nmeta\texclude_name",
    ];
    for record in records {
        let contents = format!("aifs-index-v1\n{record}\n7\t1\treadme.txt\n");
        fs::write(&index_path, &contents).unwrap();
        let error = FileIndexStore::open(&index_path)
            .expect_err("invalid policy must not silently become unknown or empty");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData, "{record}");
        assert_eq!(fs::read_to_string(&index_path).unwrap(), contents);
    }
}

#[test]
fn unrelated_metadata_keeps_legacy_reader_compatibility() {
    let fixture = TestDir::new("unknown-metadata");
    let index_path = fixture.path().join("index.txt");
    fs::write(
        &index_path,
        "aifs-index-v1\nmeta\tfuture_field\tvalue\n7\t1\treadme.txt\n",
    )
    .unwrap();

    let store = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(store.file_count(), 1);
    assert_eq!(store.search_by_name("readme")[0].size_bytes, 7);
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ai-file-search-scan-policy-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("unique fixture directory should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("owned fixture should be removed");
    }
}
