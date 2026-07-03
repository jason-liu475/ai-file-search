use std::fs;
use std::path::Path;

use ai_file_search_cli::run;
use ai_file_search_indexer::FileIndexStore;

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
        "usage: ai-file-search <search <root> <query> [--exclude-name <name>...]|index <root> <index-file> [--exclude-name <name>...]|refresh <root> <index-file> [--exclude-name <name>...]|status <root> <index-file> [--exclude-name <name>...] [--json]|stats <index-file> [--json]|query <index-file> <query> [--json]|bench <root> <query> [--exclude-name <name>...]|fixture <root> <count>>\n"
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
    let index_contents = fs::read_to_string(index_path).expect("index file should be readable");
    let lines = index_contents.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "aifs-index-v1");
    assert!(lines[1].starts_with("meta\troot\t"));

    let fields = lines[2].split('\t').collect::<Vec<_>>();
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0], "6");
    assert!(
        fields[1].parse::<u64>().expect("mtime should be numeric") > 0,
        "mtime should be recorded"
    );
    assert_eq!(fields[2], "Documents/quarterly-report.pdf");
}

#[test]
fn index_command_excludes_named_directories() {
    let fixture = TestDir::new("index_command_excludes_named_directories");
    fixture.write_file("Documents/quarterly-report.pdf", "report");
    fixture.write_file("node_modules/cache.bin", "dependency cache");
    let index_path = fixture.path().join("index.txt");

    let result = run([
        "index",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        index_path.to_str().expect("index path should be UTF-8"),
        "--exclude-name",
        "node_modules",
    ]);

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stderr, "");
    assert_eq!(result.stdout, "indexed 1 files\n");

    let index_contents = fs::read_to_string(index_path).expect("index file should be readable");
    assert!(index_contents.contains("Documents/quarterly-report.pdf"));
    assert!(!index_contents.contains("node_modules/cache.bin"));
}

#[test]
fn exclude_name_requires_a_value() {
    let fixture = TestDir::new("exclude_name_requires_a_value");
    let index_path = fixture.path().join("index.txt");

    let result = run([
        "index",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        index_path.to_str().expect("index path should be UTF-8"),
        "--exclude-name",
    ]);

    assert_eq!(result.exit_code, 2);
    assert_eq!(result.stdout, "");
    assert_eq!(
        result.stderr,
        "usage: ai-file-search <search <root> <query> [--exclude-name <name>...]|index <root> <index-file> [--exclude-name <name>...]|refresh <root> <index-file> [--exclude-name <name>...]|status <root> <index-file> [--exclude-name <name>...] [--json]|stats <index-file> [--json]|query <index-file> <query> [--json]|bench <root> <query> [--exclude-name <name>...]|fixture <root> <count>>\n"
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
fn query_command_can_print_json_results_with_metadata() {
    let fixture = TestDir::new("query_command_can_print_json_results_with_metadata");
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
        "--json",
    ]);

    assert_eq!(query_result.exit_code, 0);
    assert_eq!(query_result.stderr, "");
    assert!(query_result.stdout.starts_with(
        "{\"files\":[{\"path\":\"Documents/quarterly-report.pdf\",\"size_bytes\":6,\"modified_unix_seconds\":"
    ));
    assert!(query_result.stdout.ends_with("}]}\n"));
}

#[test]
fn json_flag_is_rejected_for_commands_without_machine_output() {
    let fixture = TestDir::new("json_flag_is_rejected_for_commands_without_machine_output");

    let result = run([
        "fixture",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        "1",
        "--json",
    ]);

    assert_eq!(result.exit_code, 2);
    assert_eq!(result.stdout, "");
    assert_eq!(
        result.stderr,
        "usage: ai-file-search <search <root> <query> [--exclude-name <name>...]|index <root> <index-file> [--exclude-name <name>...]|refresh <root> <index-file> [--exclude-name <name>...]|status <root> <index-file> [--exclude-name <name>...] [--json]|stats <index-file> [--json]|query <index-file> <query> [--json]|bench <root> <query> [--exclude-name <name>...]|fixture <root> <count>>\n"
    );
}

#[test]
fn refresh_command_removes_stale_paths_from_saved_index() {
    let fixture = TestDir::new("refresh_command_removes_stale_paths_from_saved_index");
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

    fixture.remove_file("Documents/quarterly-report.pdf");
    let refresh_result = run([
        "refresh",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        index_path.to_str().expect("index path should be UTF-8"),
    ]);
    assert_eq!(refresh_result.exit_code, 0);
    assert_eq!(refresh_result.stderr, "");
    assert_eq!(
        refresh_result.stdout,
        "refreshed 1 files\nadded=0\nupdated=0\nremoved=1\nunchanged=1\n"
    );

    let stale_query_result = run([
        "query",
        index_path.to_str().expect("index path should be UTF-8"),
        "report",
    ]);

    assert_eq!(stale_query_result.exit_code, 0);
    assert_eq!(stale_query_result.stderr, "");
    assert_eq!(stale_query_result.stdout, "");

    let store = FileIndexStore::open(&index_path).expect("refreshed index should open");
    assert_eq!(store.root_path(), Some(fixture.path()));
}

#[test]
fn status_command_reports_changes_without_rewriting_index() {
    let fixture = TestDir::new("status_command_reports_changes_without_rewriting_index");
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
    let original_index_contents =
        fs::read_to_string(&index_path).expect("index file should be readable");

    fixture.remove_file("Documents/quarterly-report.pdf");
    fixture.write_file("Documents/new-plan.txt", "plan");

    let status_result = run([
        "status",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        index_path.to_str().expect("index path should be UTF-8"),
    ]);

    assert_eq!(status_result.exit_code, 0);
    assert_eq!(status_result.stderr, "");
    assert_eq!(
        status_result.stdout,
        "scanned 2 files\nadded=1\nupdated=0\nremoved=1\nunchanged=1\n"
    );
    assert_eq!(
        fs::read_to_string(&index_path).expect("index file should stay readable"),
        original_index_contents
    );
}

#[test]
fn status_command_can_print_json_summary() {
    let fixture = TestDir::new("status_command_can_print_json_summary");
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

    fixture.remove_file("Documents/quarterly-report.pdf");
    fixture.write_file("Documents/new-plan.txt", "plan");

    let status_result = run([
        "status",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        index_path.to_str().expect("index path should be UTF-8"),
        "--json",
    ]);

    assert_eq!(status_result.exit_code, 0);
    assert_eq!(status_result.stderr, "");
    assert_eq!(
        status_result.stdout,
        "{\"scanned_files\":2,\"added\":1,\"updated\":0,\"removed\":1,\"unchanged\":1}\n"
    );
}

#[test]
fn stats_command_reports_saved_index_totals_without_scanning_root() {
    let fixture = TestDir::new("stats_command_reports_saved_index_totals_without_scanning_root");
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

    fixture.remove_file("Documents/quarterly-report.pdf");
    let stats_result = run([
        "stats",
        index_path.to_str().expect("index path should be UTF-8"),
    ]);

    assert_eq!(stats_result.exit_code, 0);
    assert_eq!(stats_result.stderr, "");
    assert_eq!(stats_result.stdout, "files=2\ntotal_bytes=13\n");
}

#[test]
fn stats_command_can_print_json_totals() {
    let fixture = TestDir::new("stats_command_can_print_json_totals");
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

    let stats_result = run([
        "stats",
        index_path.to_str().expect("index path should be UTF-8"),
        "--json",
    ]);

    assert_eq!(stats_result.exit_code, 0);
    assert_eq!(stats_result.stderr, "");
    assert_eq!(stats_result.stdout, "{\"files\":2,\"total_bytes\":13}\n");
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

#[test]
fn fixture_command_creates_deterministic_files() {
    let fixture = TestDir::new("fixture_command_creates_deterministic_files");
    let dataset = fixture.path().join("dataset");

    let result = run([
        "fixture",
        dataset.to_str().expect("dataset path should be UTF-8"),
        "3",
    ]);

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stderr, "");
    assert_eq!(result.stdout, "generated 3 files\n");
    assert!(dataset.join("group-000").join("file-000000.txt").exists());
    assert!(dataset.join("group-000").join("file-000001.txt").exists());
    assert!(dataset.join("group-000").join("file-000002.txt").exists());
}

#[test]
fn fixture_command_rejects_invalid_count() {
    let fixture = TestDir::new("fixture_command_rejects_invalid_count");

    let result = run([
        "fixture",
        fixture
            .path()
            .to_str()
            .expect("fixture path should be UTF-8"),
        "not-a-number",
    ]);

    assert_eq!(result.exit_code, 2);
    assert_eq!(result.stdout, "");
    assert!(result.stderr.starts_with("invalid count: "));
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

    fn remove_file(&self, relative_path: &str) {
        fs::remove_file(self.path.join(relative_path)).expect("fixture file should be removed");
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self.path.exists() {
            fs::remove_dir_all(&self.path).expect("fixture directory should be removed");
        }
    }
}
