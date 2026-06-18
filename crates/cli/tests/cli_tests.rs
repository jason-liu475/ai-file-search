use std::fs;
use std::path::Path;

use ai_file_search_cli::run;

#[test]
fn search_command_prints_matching_relative_paths() {
    let fixture = TestDir::new("search_command_prints_matching_relative_paths");
    fixture.write_file("Documents/quarterly-report.pdf", "report");
    fixture.write_file("Downloads/archive.zip", "archive");

    let result = run([
        "search",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        "report",
    ]);

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "Documents/quarterly-report.pdf\n");
    assert_eq!(result.stderr, "");
}

#[test]
fn missing_arguments_return_usage_error() {
    let result = run(["search"]);

    assert_eq!(result.exit_code, 2);
    assert_eq!(result.stdout, "");
    assert_eq!(
        result.stderr,
        "usage: ai-file-search <search <root> <query>|index <root> <index-file>|query <index-file> <query>|bench <root> <query>>\n"
    );
}

#[test]
fn index_command_writes_index_file() {
    let fixture = TestDir::new("index_command_writes_index_file");
    fixture.write_file("Documents/quarterly-report.pdf", "report");
    let index_path = fixture.path().join("index.txt");

    let result = run([
        "index",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        index_path.to_str().expect("index path should be UTF-8"),
    ]);

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stderr, "");
    assert_eq!(result.stdout, "indexed 1 files\n");
    assert_eq!(
        fs::read_to_string(index_path).expect("index file should be readable"),
        "Documents/quarterly-report.pdf\n"
    );
}

#[test]
fn query_command_reads_saved_index_file() {
    let fixture = TestDir::new("query_command_reads_saved_index_file");
    fixture.write_file("Documents/quarterly-report.pdf", "report");
    fixture.write_file("Downloads/archive.zip", "archive");
    let index_path = fixture.path().join("index.txt");

    let index_result = run([
        "index",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        index_path.to_str().expect("index path should be UTF-8"),
    ]);
    assert_eq!(index_result.exit_code, 0);

    let query_result = run([
        "query",
        index_path.to_str().expect("index path should be UTF-8"),
        "report",
    ]);

    assert_eq!(query_result.exit_code, 0);
    assert_eq!(query_result.stderr, "");
    assert_eq!(query_result.stdout, "Documents/quarterly-report.pdf\n");
}

#[test]
fn bench_command_reports_scan_and_search_metrics() {
    let fixture = TestDir::new("bench_command_reports_scan_and_search_metrics");
    fixture.write_file("Documents/quarterly-report.pdf", "report");
    fixture.write_file("Downloads/archive.zip", "archive");

    let result = run([
        "bench",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        "report",
    ]);

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stderr, "");
    assert!(result.stdout.contains("files=2\n"));
    assert!(result.stdout.contains("matches=1\n"));
    assert!(result.stdout.contains("scan_ms="));
    assert!(result.stdout.contains("search_ms="));
}

struct TestDir {
    path: std::path::PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("ai-file-search-cli-{name}-{}", std::process::id()));

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
