use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use ai_file_search_cli::{CliResult, run};
use ai_file_search_indexer::FileIndexStore;

#[test]
fn index_persists_canonical_absolute_root_and_sorted_deduplicated_policy() {
    let fixture = TestDir::new("persistent_scope");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("dataset/private/secret.txt", "secret");
    fixture.write_file("dataset/cache/cache.txt", "cache");

    let output = fixture.command(&[
        "index",
        "dataset",
        "index.txt",
        "--exclude-name",
        "private",
        "--exclude-name",
        "cache",
        "--exclude-name",
        "private",
    ]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(output.stdout, b"indexed 1 files\n");

    let index_path = fixture.path.join("index.txt");
    let store = FileIndexStore::open(&index_path).expect("index should open");
    let canonical_root = fs::canonicalize(fixture.path.join("dataset")).unwrap();
    assert_eq!(store.root_path(), Some(canonical_root.as_path()));
    assert!(store.root_path().unwrap().is_absolute());
    let contents = fs::read_to_string(index_path).unwrap();
    assert!(contents.lines().any(|line| line == "meta\tscan_policy\t1"));
    assert_eq!(
        contents
            .lines()
            .filter(|line| line.starts_with("meta\texclude_name\t"))
            .collect::<Vec<_>>(),
        ["meta\texclude_name\tcache", "meta\texclude_name\tprivate"]
    );
}

#[test]
fn index_persists_known_empty_policy() {
    let fixture = TestDir::new("known_empty");
    fixture.write_file("dataset/public.txt", "public");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke("index", &root, &index_path, &[]));

    let contents = fs::read_to_string(index_path).unwrap();
    assert!(contents.lines().any(|line| line == "meta\tscan_policy\t1"));
    assert!(!contents.contains("meta\texclude_name\t"));
}

#[test]
fn refresh_and_status_inherit_excluded_private_directory_when_flags_are_omitted() {
    let fixture = TestDir::new("inherited_exclusions");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("dataset/private/secret.txt", "secret");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke(
        "index",
        &root,
        &index_path,
        &["--exclude-name", "private"],
    ));
    fixture.write_file("dataset/private/new-secret.txt", "another secret");
    let original_bytes = fs::read(&index_path).unwrap();

    let status = invoke("status", &root, &index_path, &["--json"]);
    assert_success(&status);
    assert_eq!(
        status.stdout,
        "{\"scanned_files\":1,\"added\":0,\"updated\":0,\"removed\":0,\"unchanged\":1}\n"
    );
    assert_eq!(fs::read(&index_path).unwrap(), original_bytes);

    let refresh = invoke("refresh", &root, &index_path, &[]);
    assert_success(&refresh);
    assert_eq!(
        refresh.stdout,
        "refreshed 1 files\nadded=0\nupdated=0\nremoved=0\nunchanged=1\n"
    );
    let store = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(store.file_count(), 1);
    assert!(store.search_by_name("secret").is_empty());
    assert!(
        fs::read_to_string(&index_path)
            .unwrap()
            .contains("meta\texclude_name\tprivate\n")
    );
}

#[test]
fn refresh_and_status_accept_reordered_duplicate_explicit_exclusions() {
    let fixture = TestDir::new("matching_exclusions");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("dataset/private/secret.txt", "secret");
    fixture.write_file("dataset/cache/cache.txt", "cache");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke(
        "index",
        &root,
        &index_path,
        &["--exclude-name", "private", "--exclude-name", "cache"],
    ));
    let flags = [
        "--exclude-name",
        "cache",
        "--exclude-name",
        "private",
        "--exclude-name",
        "cache",
    ];

    for command in ["status", "refresh"] {
        let result = invoke(command, &root, &index_path, &flags);
        assert_success(&result);
        assert!(
            result
                .stdout
                .contains("added=0\nupdated=0\nremoved=0\nunchanged=1\n")
        );
    }
}

#[test]
fn policy_mismatch_precedes_scan_failure_and_preserves_index_bytes() {
    let fixture = TestDir::new("policy_mismatch");
    fixture.write_file("dataset/public.txt", "public");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke(
        "index",
        &root,
        &index_path,
        &["--exclude-name", "private"],
    ));
    let original_bytes = fs::read(&index_path).unwrap();
    fs::rename(&root, fixture.path.join("moved-dataset")).unwrap();

    for command in ["refresh", "status"] {
        let result = invoke(command, &root, &index_path, &["--exclude-name", "cache"]);
        assert_error(&result, "exclude_names does not match stored scan policy\n");
        assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
    }
}

#[test]
fn known_empty_policy_rejects_explicit_exclusions_before_scan() {
    let fixture = TestDir::new("empty_policy_mismatch");
    fixture.write_file("dataset/public.txt", "public");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke("index", &root, &index_path, &[]));
    let original_bytes = fs::read(&index_path).unwrap();
    fs::rename(&root, fixture.path.join("moved-dataset")).unwrap();

    for command in ["refresh", "status"] {
        let result = invoke(command, &root, &index_path, &["--exclude-name", "private"]);
        assert_error(&result, "exclude_names does not match stored scan policy\n");
        assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
    }
}

#[test]
fn refresh_and_status_open_invalid_policy_before_scanning() {
    let fixture = TestDir::new("open_before_scan");
    fixture.write_file("index.txt", "aifs-index-v1\nmeta\tscan_policy\t2\n");
    let index_path = fixture.path.join("index.txt");
    let original_bytes = fs::read(&index_path).unwrap();

    for command in ["refresh", "status"] {
        let result = invoke(
            command,
            &fixture.path.join("missing-root"),
            &index_path,
            &[],
        );
        assert_eq!(result.exit_code, 1);
        assert_eq!(result.stdout, "");
        assert!(
            result.stderr.starts_with("index open failed:"),
            "{result:?}"
        );
        assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
    }
}

#[test]
fn known_policy_rejects_a_different_root_without_writing() {
    let fixture = TestDir::new("root_mismatch");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("other/secret.txt", "secret");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke("index", &root, &index_path, &[]));
    let original_bytes = fs::read(&index_path).unwrap();

    for command in ["refresh", "status"] {
        let result = invoke(command, &fixture.path.join("other"), &index_path, &[]);
        assert_error(&result, "root does not match stored index root\n");
        assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
    }
}

#[test]
fn known_policy_without_root_is_rejected_before_scan() {
    let fixture = TestDir::new("missing_stored_root");
    fixture.write_file("index.txt", "aifs-index-v1\nmeta\tscan_policy\t1\n");
    let index_path = fixture.path.join("index.txt");
    let original_bytes = fs::read(&index_path).unwrap();

    for command in ["refresh", "status"] {
        let result = invoke(
            command,
            &fixture.path.join("missing-root"),
            &index_path,
            &[],
        );
        assert_error(&result, "index has no stored root\n");
        assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
    }
}

#[test]
fn known_policy_accepts_canonical_relative_root_aliases() {
    let fixture = TestDir::new("root_aliases");
    fixture.write_file("dataset/public.txt", "public");
    fs::create_dir(fixture.path.join("dataset/child")).unwrap();
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke("index", &root, &index_path, &[]));

    for command in ["status", "refresh"] {
        let output = fixture.command(&[command, "dataset/child/..", "index.txt"]);
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("unchanged=1\n")
        );
    }
    let canonical_root = fs::canonicalize(&root).unwrap();
    let store = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(store.root_path(), Some(canonical_root.as_path()));
}

#[test]
fn legacy_policy_uses_explicit_exclusions_without_confirming_scope() {
    let fixture = TestDir::new("legacy_explicit");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("dataset/private/secret.txt", "secret");
    fixture.write_file("index.txt", "aifs-index-v1\nmeta\troot\told-root\n");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");

    for command in ["status", "refresh"] {
        let result = invoke(command, &root, &index_path, &["--exclude-name", "private"]);
        assert_success(&result);
        assert!(result.stdout.contains("1 files\nadded=1\n"));
        assert!(
            !fs::read_to_string(&index_path)
                .unwrap()
                .contains("meta\tscan_policy\t")
        );
    }
    assert!(
        FileIndexStore::open(&index_path)
            .unwrap()
            .search_by_name("secret")
            .is_empty()
    );
}

#[test]
fn legacy_policy_uses_default_scope_when_exclusions_are_omitted() {
    let fixture = TestDir::new("legacy_default");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("dataset/private/secret.txt", "secret");
    fixture.write_file("index.txt", "stale.txt\n");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    let original_bytes = fs::read(&index_path).unwrap();

    let status = invoke("status", &root, &index_path, &[]);
    assert_success(&status);
    assert_eq!(
        status.stdout,
        "scanned 2 files\nadded=2\nupdated=0\nremoved=1\nunchanged=0\n"
    );
    assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
    let refresh = invoke("refresh", &root, &index_path, &[]);
    assert_success(&refresh);
    assert_eq!(
        refresh.stdout,
        "refreshed 2 files\nadded=2\nupdated=0\nremoved=1\nunchanged=0\n"
    );
    assert!(
        !fs::read_to_string(&index_path)
            .unwrap()
            .contains("meta\tscan_policy\t")
    );
}

#[test]
fn refresh_without_existing_index_preserves_unknown_policy() {
    let fixture = TestDir::new("missing_index_legacy");
    fixture.write_file("dataset/public.txt", "public");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke("refresh", &root, &index_path, &[]));
    let store = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(store.file_count(), 1);
    assert_eq!(store.root_path(), Some(root.as_path()));
    assert!(
        !fs::read_to_string(&index_path)
            .unwrap()
            .contains("meta\tscan_policy\t")
    );
}

#[test]
fn index_rebuild_drops_stale_paths_and_newly_excluded_directories() {
    let fixture = TestDir::new("rebuild_scope");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("dataset/stale.txt", "stale");
    fixture.write_file("dataset/private/secret.txt", "secret");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke("index", &root, &index_path, &[]));
    fs::remove_file(root.join("stale.txt")).unwrap();

    let result = invoke("index", &root, &index_path, &["--exclude-name", "private"]);
    assert_success(&result);
    assert_eq!(result.stdout, "indexed 1 files\n");
    let store = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(store.file_count(), 1);
    assert!(store.search_by_name("secret").is_empty());
    assert!(store.search_by_name("stale").is_empty());
}

#[test]
fn index_rebuild_can_change_root_without_retaining_old_paths() {
    let fixture = TestDir::new("rebuild_root");
    fixture.write_file("dataset/old.txt", "old");
    fixture.write_file("other/new.txt", "new");
    let index_path = fixture.path.join("index.txt");
    assert_success(&invoke(
        "index",
        &fixture.path.join("dataset"),
        &index_path,
        &[],
    ));
    assert_success(&invoke(
        "index",
        &fixture.path.join("other"),
        &index_path,
        &[],
    ));

    let store = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(store.file_count(), 1);
    assert!(store.search_by_name("old").is_empty());
    let root = fs::canonicalize(fixture.path.join("other")).unwrap();
    assert_eq!(store.root_path(), Some(root.as_path()));
}

#[test]
fn index_rebuild_replaces_malformed_and_unsupported_policy() {
    let fixture = TestDir::new("rebuild_invalid_policy");
    fixture.write_file("dataset/public.txt", "public");
    let root = fixture.path.join("dataset");
    let index_path = fixture.path.join("index.txt");

    for metadata in [
        "meta\tscan_policy\t2",
        "meta\tscan_policy",
        "meta\texclude_name\tprivate",
    ] {
        fs::write(
            &index_path,
            format!("aifs-index-v1\n{metadata}\n0\t0\tstale.txt\n"),
        )
        .unwrap();
        assert_success(&invoke("index", &root, &index_path, &[]));
        let store = FileIndexStore::open(&index_path).unwrap();
        assert_eq!(store.file_count(), 1);
        assert!(store.search_by_name("stale").is_empty());
        assert!(
            fs::read_to_string(&index_path)
                .unwrap()
                .contains("meta\tscan_policy\t1\n")
        );
    }
}

#[test]
fn canonical_root_self_excludes_originally_relative_index_destination() {
    let fixture = TestDir::new("relative_index");
    fixture.write_file("dataset/public.txt", "public");
    let root = fs::canonicalize(fixture.path.join("dataset")).unwrap();
    let root = root.to_str().unwrap();

    // The first build creates the missing index parent; later scans must omit the index.
    for command in ["index", "index", "status", "refresh"] {
        let output = fixture.command(&[command, root, "./dataset/snapshots/index.txt"]);
        assert!(output.status.success(), "{output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("1 files\n"), "{command}: {stdout}");
    }
    let store = FileIndexStore::open(&fixture.path.join("dataset/snapshots/index.txt")).unwrap();
    assert_eq!(store.file_count(), 1);
    assert!(store.search_by_name("index").is_empty());
}

fn invoke(command: &str, root: &Path, index_path: &Path, flags: &[&str]) -> CliResult {
    let mut args = vec![
        command,
        root.to_str().unwrap(),
        index_path.to_str().unwrap(),
    ];
    args.extend_from_slice(flags);
    run(args)
}

fn assert_success(result: &CliResult) {
    assert_eq!(result.exit_code, 0, "{result:?}");
    assert_eq!(result.stderr, "");
}

fn assert_error(result: &CliResult, message: &str) {
    assert_eq!(result.exit_code, 1, "{result:?}");
    assert_eq!(result.stdout, "");
    assert_eq!(result.stderr, message);
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ai-file-search-cli-policy-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).expect("fixture should be created");
        Self { path }
    }

    fn write_file(&self, relative_path: &str, contents: &str) {
        let path = self.path.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn command(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_ai-file-search-cli"))
            .current_dir(&self.path)
            .args(args)
            .output()
            .expect("CLI subprocess should run")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).expect("fixture should be removed");
    }
}
