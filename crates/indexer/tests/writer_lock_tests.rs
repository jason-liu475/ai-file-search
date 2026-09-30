use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use ai_file_search_indexer::{IndexWriterGuard, ScanOptions, Scanner};

#[test]
fn writer_lock_rejects_second_owner_and_releases_on_drop() {
    let fixture = TestDir::new("writer-lock-release");
    let path = fixture.path.join("index.txt");
    let guard = IndexWriterGuard::acquire(&path).unwrap();
    assert_eq!(
        guard.index_path(),
        fixture.path.canonicalize().unwrap().join("index.txt")
    );
    assert!(guard.lock_path().ends_with("index.txt.lock"));
    let error = IndexWriterGuard::acquire(&path).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::WouldBlock);
    assert!(error.to_string().contains("index is busy"));
    let lock_path = guard.lock_path().to_path_buf();
    drop(guard);
    assert!(lock_path.exists());
    let _guard = IndexWriterGuard::acquire(&path).unwrap();
}

#[test]
fn resolved_parent_alias_uses_the_same_lock() {
    let fixture = TestDir::new("writer-lock-alias");
    fs::create_dir(fixture.path.join("child")).unwrap();
    fs::write(fixture.path.join("index.txt"), "old snapshot").unwrap();
    let _guard = IndexWriterGuard::acquire(&fixture.path.join("index.txt")).unwrap();
    let alias = fixture.path.join("child").join("..").join("index.txt");
    assert_eq!(
        IndexWriterGuard::acquire(&alias).err().unwrap().kind(),
        ErrorKind::WouldBlock
    );
}

#[test]
fn different_index_destinations_have_independent_ownership() {
    let fixture = TestDir::new("writer-lock-independent");
    let _first = IndexWriterGuard::acquire(&fixture.path.join("first.txt")).unwrap();
    let _second = IndexWriterGuard::acquire(&fixture.path.join("second.txt")).unwrap();
}

#[test]
fn creates_parent_and_preserves_existing_lock_contents() {
    let fixture = TestDir::new("writer-lock-parent");
    let path = fixture.path.join("nested/index.txt");
    let guard = IndexWriterGuard::acquire(&path).unwrap();
    let lock = guard.lock_path().to_path_buf();
    drop(guard);
    fs::write(&lock, "do not truncate").unwrap();
    let guard = IndexWriterGuard::acquire(&path).unwrap();
    drop(guard);
    assert_eq!(fs::read_to_string(lock).unwrap(), "do not truncate");
}

#[test]
fn rejects_nonregular_lock_without_changing_destination() {
    let fixture = TestDir::new("writer-lock-nonregular");
    fs::write(fixture.path.join("index.txt"), "old snapshot").unwrap();
    fs::create_dir(fixture.path.join("index.txt.lock")).unwrap();
    assert!(IndexWriterGuard::acquire(&fixture.path.join("index.txt")).is_err());
    assert_eq!(
        fs::read_to_string(fixture.path.join("index.txt")).unwrap(),
        "old snapshot"
    );
}

#[test]
fn indexed_scan_excludes_only_its_own_reserved_artifacts() {
    let fixture = TestDir::new("indexed-scan-artifacts");
    for name in [
        "index.txt",
        "index.txt.lock",
        ".index.txt.aifs-tmp-12-0",
        "index.txt.tmp",
        "report.tmp",
        "report.txt",
        "other.txt.lock",
    ] {
        fs::write(fixture.path.join(name), "data").unwrap();
    }
    fs::create_dir(fixture.path.join("nested")).unwrap();
    fs::write(fixture.path.join("nested/index.txt.lock"), "user file").unwrap();
    fs::write(
        fixture.path.join("nested/.index.txt.aifs-tmp-12-0"),
        "user file",
    )
    .unwrap();
    let scanner = Scanner::new(ScanOptions::default());
    let files = scanner
        .scan_for_index(&fixture.path, &fixture.path.join("index.txt"))
        .unwrap();
    let paths = files
        .iter()
        .map(|file| file.relative_path.as_normalized())
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        [
            "index.txt.tmp",
            "nested/.index.txt.aifs-tmp-12-0",
            "nested/index.txt.lock",
            "other.txt.lock",
            "report.tmp",
            "report.txt"
        ]
    );
    assert_eq!(scanner.scan(&fixture.path).unwrap().len(), 9);
}

#[test]
fn indexed_scan_excludes_lock_before_first_snapshot_exists() {
    let fixture = TestDir::new("indexed-scan-first-index");
    let index = fixture.path.join("index.txt");
    let _guard = IndexWriterGuard::acquire(&index).unwrap();
    fs::write(fixture.path.join("report.txt"), "data").unwrap();
    let files = Scanner::new(ScanOptions::default())
        .scan_for_index(&fixture.path, &index)
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].relative_path.as_normalized(), "report.txt");
}

#[test]
fn external_destination_does_not_exclude_similarly_named_user_files() {
    let fixture = TestDir::new("indexed-scan-external");
    fs::write(fixture.path.join("index.txt.lock"), "user file").unwrap();
    let external = fixture
        .path
        .with_extension("uncreated")
        .join("nested/index.txt");
    let files = Scanner::new(ScanOptions::default())
        .scan_for_index(&fixture.path, &external)
        .unwrap();
    assert_eq!(files.len(), 1);
    assert!(!external.parent().unwrap().exists());
}

#[cfg(unix)]
#[test]
fn symlink_parent_alias_uses_the_same_lock_and_scan_identity() {
    use std::os::unix::fs::symlink;
    let fixture = TestDir::new("writer-lock-symlink");
    fs::create_dir(fixture.path.join("real")).unwrap();
    symlink(fixture.path.join("real"), fixture.path.join("alias")).unwrap();
    let real = fixture.path.join("real/index.txt");
    let _guard = IndexWriterGuard::acquire(&real).unwrap();
    let alias = fixture.path.join("alias/index.txt");
    assert_eq!(
        IndexWriterGuard::acquire(&alias).err().unwrap().kind(),
        ErrorKind::WouldBlock
    );
    assert!(
        Scanner::new(ScanOptions::default())
            .scan_for_index(&fixture.path, &alias)
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn symlink_lock_is_rejected_without_touching_target() {
    use std::os::unix::fs::symlink;
    let fixture = TestDir::new("writer-lock-link");
    fs::write(fixture.path.join("user.txt"), "keep").unwrap();
    symlink(
        fixture.path.join("user.txt"),
        fixture.path.join("index.txt.lock"),
    )
    .unwrap();
    assert!(IndexWriterGuard::acquire(&fixture.path.join("index.txt")).is_err());
    assert_eq!(
        fs::read_to_string(fixture.path.join("user.txt")).unwrap(),
        "keep"
    );
}

#[cfg(unix)]
#[test]
fn symlink_index_alias_resolves_target_and_excludes_real_snapshot() {
    use std::os::unix::fs::symlink;
    let fixture = TestDir::new("writer-lock-final-link");
    let target = fixture.path.join("real.txt");
    fs::write(&target, "snapshot").unwrap();
    let alias = fixture.path.join("alias.txt");
    symlink(&target, &alias).unwrap();
    let guard = IndexWriterGuard::acquire(&alias).unwrap();
    assert_eq!(guard.index_path(), target.canonicalize().unwrap());
    assert_eq!(
        IndexWriterGuard::acquire(&target).err().unwrap().kind(),
        ErrorKind::WouldBlock
    );
    assert!(
        Scanner::new(ScanOptions::default())
            .scan_for_index(&fixture.path, &alias)
            .unwrap()
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn unsafe_lock_symlink_does_not_hide_its_target_from_read_only_scan() {
    use std::os::unix::fs::symlink;
    let fixture = TestDir::new("indexed-scan-lock-link");
    fs::write(fixture.path.join("user.txt"), "keep").unwrap();
    symlink(
        fixture.path.join("user.txt"),
        fixture.path.join("index.txt.lock"),
    )
    .unwrap();
    let files = Scanner::new(ScanOptions::default())
        .scan_for_index(&fixture.path, &fixture.path.join("index.txt"))
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].relative_path.as_normalized(), "user.txt");
}

#[cfg(unix)]
#[test]
fn new_unix_lock_permissions_are_private() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = TestDir::new("writer-lock-permissions");
    let guard = IndexWriterGuard::acquire(&fixture.path.join("index.txt")).unwrap();
    let mode = fs::metadata(guard.lock_path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0);
}

#[cfg(windows)]
#[test]
fn windows_case_alias_uses_the_same_lock_before_snapshot_exists() {
    let fixture = TestDir::new("writer-lock-case");
    let _guard = IndexWriterGuard::acquire(&fixture.path.join("INDEX.txt")).unwrap();
    assert_eq!(
        IndexWriterGuard::acquire(&fixture.path.join("index.txt"))
            .err()
            .unwrap()
            .kind(),
        ErrorKind::WouldBlock
    );
    assert!(
        Scanner::new(ScanOptions::default())
            .scan_for_index(&fixture.path, &fixture.path.join("index.txt"))
            .unwrap()
            .is_empty()
    );
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ai-file-search-{name}-{}", std::process::id()));
        if path.exists() {
            fs::remove_dir_all(&path).unwrap();
        }
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).unwrap();
        }
    }
}
