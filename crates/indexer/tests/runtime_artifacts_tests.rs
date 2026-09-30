use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use ai_file_search_indexer::{ScanOptions, Scanner};

#[test]
fn resolves_absolute_relative_and_parent_alias_artifacts_once_per_scan() {
    let fixture = TestDir::new("resolved");
    fixture.write("state.json", "runtime");
    fixture.write("nested/state.json", "user");
    fixture.write("ordinary.tmp", "user");
    fixture.write("index.txt.lock", "lock");
    fixture.write(".index.txt.aifs-tmp-stale", "temp");
    fs::create_dir(fixture.path.join("alias")).unwrap();
    let relative = fixture
        .path
        .strip_prefix(std::env::current_dir().unwrap())
        .unwrap()
        .join("state.json");
    for artifact in [
        fixture.path.join("state.json"),
        relative,
        fixture.path.join("alias/../state.json"),
    ] {
        let files = Scanner::new(ScanOptions::default())
            .scan_for_index_with_artifacts(
                &fixture.path,
                &fixture.path.join("index.txt"),
                &[artifact],
            )
            .unwrap();
        let paths = files
            .iter()
            .map(|file| file.relative_path.as_normalized())
            .collect::<Vec<_>>();
        assert_eq!(paths, ["nested/state.json", "ordinary.tmp"]);
    }
}

#[test]
fn missing_or_external_artifacts_do_not_create_directories_or_hide_user_names() {
    let fixture = TestDir::new("missing");
    fixture.write("state.json", "user");
    let missing = fixture.path.join("missing/nested/state.json");
    let external = fixture.path.with_extension("outside").join("state.json");
    let files = Scanner::new(ScanOptions::default())
        .scan_for_index_with_artifacts(
            &fixture.path,
            &fixture.path.join("index.txt"),
            &[missing.clone(), external.clone()],
        )
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].relative_path.as_normalized(), "state.json");
    assert!(!missing.parent().unwrap().exists());
    assert!(!external.parent().unwrap().exists());
}

#[test]
fn runtime_artifacts_preserve_directory_exclusions_and_plain_scan_behavior() {
    let fixture = TestDir::new("policy");
    fixture.write("public.txt", "user");
    fixture.write("state.json", "runtime");
    fixture.write("private/secret.txt", "secret");
    let scanner = Scanner::new(ScanOptions::default().exclude_name("private"));
    let files = scanner
        .scan_for_index_with_artifacts(
            &fixture.path,
            &fixture.path.join("index.txt"),
            &[fixture.path.join("state.json")],
        )
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].relative_path.as_normalized(), "public.txt");
    assert_eq!(
        scanner
            .scan_for_index(&fixture.path, &fixture.path.join("index.txt"))
            .unwrap()
            .len(),
        2
    );
    assert_eq!(scanner.scan(&fixture.path).unwrap().len(), 2);
}

#[test]
fn a_directory_is_not_a_valid_runtime_file_exclusion() {
    let fixture = TestDir::new("directory");
    fixture.write("user/document.txt", "user");
    let error = Scanner::new(ScanOptions::default())
        .scan_for_index_with_artifacts(
            &fixture.path,
            &fixture.path.join("index.txt"),
            &[fixture.path.join("user")],
        )
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
}

#[cfg(unix)]
#[test]
fn resolves_runtime_file_and_parent_symlink_aliases_without_following_scan_links() {
    use std::os::unix::fs::symlink;
    let fixture = TestDir::new("links");
    fixture.write("real/state.json", "runtime");
    fixture.write("real/document.txt", "user");
    symlink(fixture.path.join("real"), fixture.path.join("parent-link")).unwrap();
    symlink(
        fixture.path.join("real/state.json"),
        fixture.path.join("state-link"),
    )
    .unwrap();
    for artifact in [
        fixture.path.join("parent-link/state.json"),
        fixture.path.join("state-link"),
    ] {
        let files = Scanner::new(ScanOptions::default())
            .scan_for_index_with_artifacts(
                &fixture.path,
                &fixture.path.join("index.txt"),
                &[artifact],
            )
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path.as_normalized(), "real/document.txt");
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "aifs-runtime-artifacts-{name}-{}-{}",
                std::process::id(),
                NEXT_ID.fetch_add(1, Ordering::Relaxed),
            ));
        fs::create_dir_all(&path).unwrap();
        Self { path }
    }
    fn write(&self, name: &str, contents: &str) {
        let path = self.path.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
