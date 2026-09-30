use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ai_file_search_core::PathId;
use ai_file_search_indexer::{
    FileIndexStore, FileIndexWriter, IndexWriterGuard, IndexedFile, MemoryIndexStore, ScanOptions,
};

#[test]
fn legacy_line_boundaries_match_str_lines() {
    let fixture = TestDir::new("legacy-lines");
    let index_path = fixture.path.join("index.txt");
    for contents in [
        "",
        "\n",
        "\r\n",
        "\n\r\n",
        "first.txt",
        "first.txt\n",
        "first.txt\r\n",
        "\nfirst.txt\n\nlast.txt",
        "first.txt\r\n\r\nlast.txt\r\n",
        "first.txt\r",
        "first\rname.txt\nlast.txt\r",
        "\naifs-index-v1\nlast.txt",
        "aifs-index-v1\r",
    ] {
        fs::write(&index_path, contents).unwrap();
        let mut expected = contents
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| indexed_file(line, 0, 0))
            .collect::<Vec<_>>();
        expected.sort_by(|left, right| {
            left.relative_path
                .as_normalized()
                .cmp(right.relative_path.as_normalized())
        });

        let store = FileIndexStore::open(&index_path).unwrap();

        assert_eq!(store.all_files(), expected, "contents: {contents:?}");
        assert!(store.root_path().is_none());
        assert!(store.scan_policy().is_none());
    }
}

#[test]
fn versioned_line_boundaries_preserve_header_and_last_record() {
    let fixture = TestDir::new("versioned-lines");
    let index_path = fixture.path.join("index.txt");
    for newline in ["\n", "\r\n"] {
        for suffix in ["", newline] {
            let contents = format!(
                "aifs-index-v1{newline}{newline}7\t11\tfirst.txt{newline}{newline}9\t13\tlast.txt{suffix}"
            );
            fs::write(&index_path, contents).unwrap();
            let store = FileIndexStore::open(&index_path).unwrap();
            assert_eq!(
                store.all_files(),
                vec![
                    indexed_file("first.txt", 7, 11),
                    indexed_file("last.txt", 9, 13),
                ]
            );
        }
    }
    for contents in ["aifs-index-v1", "aifs-index-v1\n", "aifs-index-v1\r\n\r\n"] {
        fs::write(&index_path, contents).unwrap();
        assert_eq!(FileIndexStore::open(&index_path).unwrap().file_count(), 0);
    }
    fs::write(&index_path, "aifs-index-v1\n7\t11\tlast.txt\r").unwrap();
    assert_eq!(
        FileIndexStore::open(&index_path).unwrap().all_files(),
        vec![indexed_file("last.txt\r", 7, 11)]
    );
}

#[test]
fn missing_index_returns_empty_without_creating_directories() {
    let fixture = TestDir::new("missing");
    let parent = fixture.path.join("absent");
    let store = FileIndexStore::open(&parent.join("index.txt")).unwrap();
    assert_eq!(store.file_count(), 0);
    assert!(store.root_path().is_none());
    assert!(store.scan_policy().is_none());
    assert!(!parent.exists());
}

#[test]
fn directory_open_error_is_not_an_empty_snapshot() {
    let fixture = TestDir::new("directory-error");
    assert!(FileIndexStore::open(&fixture.path).is_err());
}

#[test]
fn invalid_path_open_error_is_not_an_empty_snapshot() {
    let fixture = TestDir::new("invalid-path");
    let path = fixture.path.join("invalid\0index.txt");
    let expected = File::open(&path).unwrap_err();
    assert_ne!(expected.kind(), io::ErrorKind::NotFound);

    let actual = FileIndexStore::open(&path)
        .expect_err("a failed existence probe must not hide an open error");

    assert_eq!(actual.kind(), expected.kind());
}

#[cfg(windows)]
#[test]
fn sharing_violation_is_not_an_empty_snapshot() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = TestDir::new("sharing-error");
    let path = fixture.path.join("index.txt");
    fs::write(&path, "first.txt\n").unwrap();
    let handle = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();
    let expected = File::open(&path).unwrap_err();
    let actual = FileIndexStore::open(&path).unwrap_err();
    assert_eq!(actual.kind(), expected.kind());
    drop(handle);
    assert_eq!(FileIndexStore::open(&path).unwrap().file_count(), 1);
}

#[test]
fn invalid_utf8_never_returns_a_partial_snapshot() {
    let fixture = TestDir::new("invalid-utf8");
    let path = fixture.path.join("index.txt");
    for prefix in ["first.txt\n", "aifs-index-v1\n7\t11\tfirst.txt\n"] {
        for suffix in [&b"bad\xff.txt\n"[..], &b"bad\xc3"[..]] {
            let mut bytes = prefix.as_bytes().to_vec();
            bytes.extend_from_slice(suffix);
            fs::write(&path, &bytes).unwrap();
            let error = FileIndexStore::open(&path).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
}

#[test]
fn unicode_and_long_records_cross_the_read_buffer() {
    let fixture = TestDir::new("long-records");
    let path = fixture.path.join("index.txt");
    let long_path = format!(
        "nested/{}/report\t\u{6587}\u{4ef6}.txt",
        "a\u{6587}".repeat(6_000)
    );
    for (contents, expected) in [
        (
            format!("{long_path}\r\nlast.txt"),
            vec![
                indexed_file("last.txt", 0, 0),
                indexed_file(&long_path, 0, 0),
            ],
        ),
        (
            format!("aifs-index-v1\r\n7\t11\t{long_path}\r\n9\t13\tlast.txt"),
            vec![
                indexed_file("last.txt", 9, 13),
                indexed_file(&long_path, 7, 11),
            ],
        ),
    ] {
        fs::write(&path, contents).unwrap();
        assert_eq!(FileIndexStore::open(&path).unwrap().all_files(), expected);
    }
}

#[test]
fn many_records_load_in_path_order_with_last_duplicate_winning() {
    let fixture = TestDir::new("many-records");
    let path = fixture.path.join("index.txt");
    let count = 20_000_u64;
    {
        let mut output = BufWriter::new(File::create(&path).unwrap());
        writeln!(output, "aifs-index-v1").unwrap();
        for id in (0..count).rev() {
            writeln!(output, "{id}\t{}\tfiles/{id:05}.txt", id + 1).unwrap();
        }
        write!(output, "77\t88\tfiles/00042.txt").unwrap();
        output.flush().unwrap();
    }
    let store = FileIndexStore::open(&path).unwrap();
    let files = store.all_files();
    assert_eq!(u64::try_from(files.len()).unwrap(), count);
    for (id, file) in (0..count).zip(files) {
        let (size, modified) = if id == 42 { (77, 88) } else { (id, id + 1) };
        assert_eq!(
            file,
            indexed_file(&format!("files/{id:05}.txt"), size, modified)
        );
    }
}

#[test]
fn root_and_policy_metadata_preserve_line_boundary_parity() {
    let fixture = TestDir::new("metadata-boundaries");
    let path = fixture.path.join("index.txt");
    for newline in ["\n", "\r\n"] {
        let contents = format!(
            "aifs-index-v1{newline}meta\troot\tfirst{newline}meta\troot{newline}meta\texclude_name\tprivate{newline}meta\tfuture\tignored{newline}meta\tscan_policy\t1{newline}7\t11\tfile.txt{newline}meta\troot\troot\\\\with\\tTabs\\nLines\\rCR\\qEnd\\{newline}meta\texclude_name\t"
        );
        fs::write(&path, contents).unwrap();
        let store = FileIndexStore::open(&path).unwrap();
        assert_eq!(
            store.root_path(),
            Some(Path::new("root\\with\tTabs\nLines\rCR\\qEnd\\"))
        );
        assert_eq!(
            store.scan_policy(),
            Some(
                &ScanOptions::default()
                    .exclude_name("private")
                    .exclude_name("")
            )
        );
        assert_eq!(store.all_files(), vec![indexed_file("file.txt", 7, 11)]);
    }
}

#[test]
fn malformed_file_numbers_keep_the_legacy_defaulting_behavior() {
    let fixture = TestDir::new("default-numbers");
    let path = fixture.path.join("index.txt");
    fs::write(
        &path,
        "aifs-index-v1\nnot-a-number\t-1\ta.txt\n18446744073709551616\t\tb.txt\n9\t13\tc\tname.txt",
    )
    .unwrap();
    assert_eq!(
        FileIndexStore::open(&path).unwrap().all_files(),
        vec![
            indexed_file("a.txt", 0, 0),
            indexed_file("b.txt", 0, 0),
            indexed_file("c\tname.txt", 9, 13),
        ]
    );
}

#[test]
fn memory_iterator_borrows_ordered_records_and_has_exact_remaining_length() {
    let mut store = MemoryIndexStore::new();
    let mut empty = borrowed_iterator(store.iter_files());
    assert_eq!(empty.len(), 0);
    assert_eq!(empty.next(), None);
    assert_eq!(empty.next_back(), None);
    drop(empty);
    store.replace_all(vec![
        indexed_file("z.txt", 9, 1),
        indexed_file("nested\\a.txt", 1, 2),
        indexed_file("nested/a.txt", 7, 3),
        indexed_file("m.txt", 5, 4),
    ]);

    let mut files = borrowed_iterator(store.iter_files());
    assert_eq!(files.len(), 3);
    let first = files.next().unwrap();
    assert_eq!(first, &indexed_file("m.txt", 5, 4));
    assert!(std::ptr::eq(first, store.iter_files().next().unwrap()));
    assert_eq!(files.len(), 2);
    assert_eq!(files.next_back(), Some(&indexed_file("z.txt", 9, 1)));
    assert_eq!(files.len(), 1);
    assert_eq!(files.next(), Some(&indexed_file("nested/a.txt", 7, 3)));
    assert_eq!(files.len(), 0);
    assert_eq!(files.next_back(), None);
    assert_eq!(files.next(), None);
    assert_eq!(
        store.iter_files().cloned().collect::<Vec<_>>(),
        store.all_files()
    );
}

#[test]
fn snapshot_and_writer_deref_expose_the_borrowed_iterator() {
    let fixture = TestDir::new("snapshot-iterator");
    let path = fixture.path.join("index.txt");
    assert_eq!(
        borrowed_iterator(FileIndexStore::new(&path).iter_files()).len(),
        0
    );
    fs::write(
        &path,
        "aifs-index-v1\n9\t1\tz.txt\n1\t2\ta.txt\n7\t3\ta.txt",
    )
    .unwrap();
    let store = FileIndexStore::open(&path).unwrap();
    let mut records = borrowed_iterator(store.iter_files());
    assert_eq!(records.len(), 2);
    let first = records.next().unwrap();
    assert_eq!(first, &indexed_file("a.txt", 7, 3));
    assert!(std::ptr::eq(first, store.iter_files().next().unwrap()));
    assert_eq!(records.next_back(), Some(&indexed_file("z.txt", 9, 1)));
    assert_eq!(records.len(), 0);

    let mut guard = IndexWriterGuard::acquire(&path).unwrap();
    let writer = FileIndexWriter::open(&mut guard).unwrap();
    assert_eq!(borrowed_iterator(writer.iter_files()).len(), 2);
    assert_eq!(
        writer.iter_files().cloned().collect::<Vec<_>>(),
        store.all_files()
    );
}

fn borrowed_iterator<'a>(
    files: impl DoubleEndedIterator<Item = &'a IndexedFile> + ExactSizeIterator,
) -> impl DoubleEndedIterator<Item = &'a IndexedFile> + ExactSizeIterator {
    files
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ai-file-search-streaming-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}

fn indexed_file(path: &str, size_bytes: u64, modified_unix_seconds: u64) -> IndexedFile {
    IndexedFile {
        relative_path: PathId::from_user_path(path),
        size_bytes,
        modified_unix_seconds,
    }
}
