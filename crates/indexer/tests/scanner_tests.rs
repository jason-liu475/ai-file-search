use std::fs;
use std::path::Path;

use ai_file_search_indexer::{ScanOptions, Scanner};

#[test]
fn scans_files_directly_under_root() {
    let fixture = TestDir::new("scans_files_directly_under_root");
    fixture.write_file("report.txt", "quarterly report");

    let scanner = Scanner::new(ScanOptions::default());
    let files = scanner.scan(fixture.path()).expect("scan should succeed");

    let paths = files
        .iter()
        .map(|file| file.relative_path.as_normalized())
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["report.txt"]);
    assert_eq!(files[0].size_bytes, 16);
    assert!(files[0].modified_unix_seconds > 0);
}

#[test]
fn scans_files_in_nested_directories() {
    let fixture = TestDir::new("scans_files_in_nested_directories");
    fixture.write_file("Documents/report.txt", "quarterly report");
    fixture.write_file("Downloads/archive.zip", "zip contents");

    let scanner = Scanner::new(ScanOptions::default());
    let files = scanner.scan(fixture.path()).expect("scan should succeed");

    let paths = files
        .iter()
        .map(|file| file.relative_path.as_normalized())
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["Documents/report.txt", "Downloads/archive.zip"]);
}

#[test]
fn skips_directories_with_excluded_names() {
    let fixture = TestDir::new("skips_directories_with_excluded_names");
    fixture.write_file("Documents/report.txt", "quarterly report");
    fixture.write_file("node_modules/cache.bin", "dependency cache");

    let scanner = Scanner::new(ScanOptions::default().exclude_name("node_modules"));
    let files = scanner.scan(fixture.path()).expect("scan should succeed");

    let paths = files
        .iter()
        .map(|file| file.relative_path.as_normalized())
        .collect::<Vec<_>>();

    assert_eq!(paths, vec!["Documents/report.txt"]);
}

struct TestDir {
    path: std::path::PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("ai-file-search-{name}-{}", std::process::id()));

        if path.exists() {
            fs::remove_dir_all(&path).expect("old fixture should be removable");
        }
        fs::create_dir_all(&path).expect("fixture directory should be created");

        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write_file(&self, relative_path: &str, contents: &str) {
        let path = self.path.join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent directory should be created");
        }
        fs::write(path, contents).expect("fixture file should be written");
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).expect("fixture directory should be removed");
        }
    }
}
