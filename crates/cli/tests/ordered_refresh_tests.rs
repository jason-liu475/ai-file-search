use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ai_file_search_cli::{CliResult, run};
use ai_file_search_indexer::{FileIndexStore, ScanOptions};

#[test]
fn ordered_refresh_preserves_mixed_summary_scope_and_snapshot_order() {
    let fixture = TestDir::new("mixed");
    fixture.write_file("dataset/x-removed.txt", "removed");
    fixture.write_file("dataset/m-updated.txt", "old");
    fixture.write_file("dataset/a/kept.txt", "nested");
    fixture.write_file("dataset/Z-kept.txt", "uppercase");
    fixture.write_file("dataset/private/secret.txt", "private");
    let root = fixture.path.join("dataset");
    let index_path = root.join("snapshots/index.txt");
    assert_success(&invoke(
        "index",
        &root,
        &index_path,
        &["--exclude-name", "private"],
    ));

    // Disk record order is not the comparison order; the final duplicate wins.
    let contents = fs::read_to_string(&index_path).unwrap();
    let mut reordered = String::new();
    for line in contents
        .lines()
        .filter(|line| *line == "aifs-index-v1" || line.starts_with("meta\t"))
    {
        reordered.push_str(line);
        reordered.push('\n');
    }
    reordered.push_str("0\t0\tZ-kept.txt\n0\t0\tm-updated.txt\n");
    for line in contents
        .lines()
        .rev()
        .filter(|line| *line != "aifs-index-v1" && !line.starts_with("meta\t"))
    {
        reordered.push_str(line);
        reordered.push('\n');
    }
    fs::write(&index_path, reordered).unwrap();
    let original_bytes = fs::read(&index_path).unwrap();

    fs::remove_file(root.join("x-removed.txt")).unwrap();
    fixture.write_file("dataset/m-updated.txt", "changed size without a clock wait");
    fixture.write_file("dataset/a-added.txt", "added");
    fixture.write_file("dataset/private/new-secret.txt", "excluded");

    let status = invoke("status", &root, &index_path, &[]);
    assert_success(&status);
    assert_eq!(
        status.stdout,
        "scanned 4 files\nadded=1\nupdated=1\nremoved=1\nunchanged=2\n"
    );
    let json = invoke("status", &root, &index_path, &["--json"]);
    assert_success(&json);
    assert_eq!(
        json.stdout,
        "{\"scanned_files\":4,\"added\":1,\"updated\":1,\"removed\":1,\"unchanged\":2}\n"
    );
    assert_eq!(fs::read(&index_path).unwrap(), original_bytes);

    let refresh = invoke("refresh", &root, &index_path, &[]);
    assert_success(&refresh);
    assert_eq!(
        refresh.stdout,
        "refreshed 4 files\nadded=1\nupdated=1\nremoved=1\nunchanged=2\n"
    );
    let store = FileIndexStore::open(&index_path).unwrap();
    assert_eq!(
        store
            .iter_files()
            .map(|file| file.relative_path.as_normalized())
            .collect::<Vec<_>>(),
        ["Z-kept.txt", "a-added.txt", "a/kept.txt", "m-updated.txt"]
    );
    let canonical_root = fs::canonicalize(&root).unwrap();
    assert_eq!(store.root_path(), Some(canonical_root.as_path()));
    assert_eq!(
        store.scan_policy(),
        Some(&ScanOptions::default().exclude_name("private"))
    );
    let query = run(["query", index_path.to_str().unwrap(), ""]);
    assert_success(&query);
    assert_eq!(
        query.stdout,
        "Z-kept.txt\na-added.txt\na/kept.txt\nm-updated.txt\n"
    );
    let status = invoke("status", &root, &index_path, &[]);
    assert_success(&status);
    assert_eq!(
        status.stdout,
        "scanned 4 files\nadded=0\nupdated=0\nremoved=0\nunchanged=4\n"
    );
}

#[cfg(windows)]
#[test]
fn unchanged_manual_refresh_still_publishes_while_status_is_readonly() {
    use std::os::windows::fs::OpenOptionsExt;

    let fixture = TestDir::new("explicit_publication");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("dataset/private/secret.txt", "private");
    let root = fixture.path.join("dataset");
    let index_path = root.join("snapshots/index.txt");
    assert_success(&invoke(
        "index",
        &root,
        &index_path,
        &["--exclude-name", "private"],
    ));
    let original_bytes = fs::read(&index_path).unwrap();

    // FILE_SHARE_READ denies both in-place writes and replacement, without timing.
    let publication_gate = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&index_path)
        .unwrap();
    let status = invoke("status", &root, &index_path, &["--json"]);
    assert_success(&status);
    assert_eq!(
        status.stdout,
        "{\"scanned_files\":1,\"added\":0,\"updated\":0,\"removed\":0,\"unchanged\":1}\n"
    );

    let refresh = invoke("refresh", &root, &index_path, &[]);
    assert_eq!(refresh.exit_code, 1, "{refresh:?}");
    assert_eq!(refresh.stdout, "");
    assert!(
        refresh.stderr.starts_with("index save failed:"),
        "{refresh:?}"
    );
    assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
    let mut adjacent_files = fs::read_dir(index_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    adjacent_files.sort();
    assert_eq!(adjacent_files, ["index.txt", "index.txt.lock"]);

    fixture.write_file("dataset/added.txt", "added");
    let changed_status = invoke("status", &root, &index_path, &[]);
    assert_success(&changed_status);
    assert_eq!(
        changed_status.stdout,
        "scanned 2 files\nadded=1\nupdated=0\nremoved=0\nunchanged=1\n"
    );
    assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
    fs::remove_file(root.join("added.txt")).unwrap();
    drop(publication_gate);

    // The failed attempt releases its writer guard; unchanged explicit retry saves.
    let retry = invoke("refresh", &root, &index_path, &[]);
    assert_success(&retry);
    assert_eq!(
        retry.stdout,
        "refreshed 1 files\nadded=0\nupdated=0\nremoved=0\nunchanged=1\n"
    );
    assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
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

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ai-file-search-cli-ordered-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn write_file(&self, relative_path: &str, contents: &str) {
        let path = self.path.join(relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
