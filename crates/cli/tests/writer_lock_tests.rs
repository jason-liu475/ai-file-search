use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use ai_file_search_indexer::IndexWriterGuard;

#[test]
fn subprocess_owner_blocks_writers_before_open_or_scan_and_releases_on_exit() {
    let fixture = TestDir::new("normal_release");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("index.txt", "aifs-index-v1\nmeta\tscan_policy\t2\n");
    fs::create_dir(fixture.path.join("alias")).unwrap();
    let index_path = fixture.path.join("index.txt");
    let original_bytes = fs::read(&index_path).unwrap();
    let mut owner = ChildGuard::holding(&fixture, &index_path);

    for index_alias in [
        index_path.to_str().unwrap(),
        "./index.txt",
        "alias/../index.txt",
    ] {
        for command in ["index", "refresh"] {
            for root in ["dataset", "missing-root"] {
                let output = fixture.command(&[command, root, index_alias]);
                assert_busy(&output);
                assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
            }
        }
    }

    owner.release_normally();
    assert!(fixture.path.join("index.txt.lock").exists());
    assert_success(&fixture.command(&["index", "dataset", "alias/../index.txt"]));
    assert_success(&fixture.command(&["refresh", "dataset", "./index.txt"]));
    let stats = fixture.command(&["stats", "index.txt"]);
    assert_success(&stats);
    assert_eq!(stats.stdout, b"files=1\ntotal_bytes=6\n");
}

#[test]
fn subprocess_owner_of_new_relative_index_releases_after_forced_termination() {
    let fixture = TestDir::new("forced_release");
    fixture.write_file("dataset/public.txt", "public");
    let index_path = fixture.path.join("dataset/snapshots/index.txt");
    let mut owner = ChildGuard::holding(&fixture, Path::new("./dataset/snapshots/index.txt"));

    for index_alias in [
        index_path.to_str().unwrap(),
        "./dataset/snapshots/index.txt",
    ] {
        for command in ["index", "refresh"] {
            let output = fixture.command(&[command, "dataset", index_alias]);
            assert_busy(&output);
            assert!(!index_path.exists(), "blocked writer must not publish");
        }
    }

    owner.terminate();
    assert!(
        fixture
            .path
            .join("dataset/snapshots/index.txt.lock")
            .exists()
    );
    let index = fixture.command(&["index", "dataset", "./dataset/snapshots/index.txt"]);
    assert_success(&index);
    assert_eq!(index.stdout, b"indexed 1 files\n");
    fixture.write_file("dataset/added.txt", "added");
    let refresh = fixture.command(&["refresh", "dataset", index_path.to_str().unwrap()]);
    assert_success(&refresh);
    assert_eq!(
        refresh.stdout,
        b"refreshed 2 files\nadded=1\nupdated=0\nremoved=0\nunchanged=1\n"
    );
    let query = fixture.command(&["query", index_path.to_str().unwrap(), ""]);
    assert_success(&query);
    assert_eq!(query.stdout, b"added.txt\npublic.txt\n");
}

#[test]
fn readonly_subprocesses_continue_under_writer_ownership_without_indexing_artifacts() {
    let fixture = TestDir::new("readonly_while_busy");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("dataset/ordinary.tmp", "ordinary");
    fixture.write_file("dataset/private/secret.txt", "secret");
    assert_success(&fixture.command(&[
        "index",
        "dataset",
        "dataset/snapshots/index.txt",
        "--exclude-name",
        "private",
    ]));
    fixture.write_file("dataset/private/new-secret.txt", "new secret");
    fixture.write_file(
        "dataset/snapshots/.index.txt.aifs-tmp-stale",
        "owned namespace",
    );
    let index_path = fixture.path.join("dataset/snapshots/index.txt");
    let original_bytes = fs::read(&index_path).unwrap();
    let mut owner = ChildGuard::holding(&fixture, &index_path);

    for index_alias in [
        index_path.to_str().unwrap(),
        "./dataset/snapshots/index.txt",
    ] {
        let query = fixture.command(&["query", index_alias, "public"]);
        assert_success(&query);
        assert_eq!(query.stdout, b"public.txt\n");
        let totals = fixture.command(&["stats", index_alias, "--json"]);
        assert_success(&totals);
        assert_eq!(totals.stdout, b"{\"files\":2,\"total_bytes\":14}\n");
        let status = fixture.command(&["status", "dataset", index_alias, "--json"]);
        assert_success(&status);
        assert_eq!(
            status.stdout,
            b"{\"scanned_files\":2,\"added\":0,\"updated\":0,\"removed\":0,\"unchanged\":2}\n"
        );
        assert_eq!(fs::read(&index_path).unwrap(), original_bytes);
    }

    assert_artifact_free_snapshot(&fixture, 2);
    owner.release_normally();
    assert_success(&fixture.command(&["refresh", "dataset", "dataset/snapshots/index.txt"]));
    assert_artifact_free_snapshot(&fixture, 2);
}

#[test]
#[ignore = "runs only as an isolated subprocess lock owner"]
fn writer_lock_child() {
    let index_path = PathBuf::from(std::env::var_os("AIFS_CLI_TEST_LOCK_INDEX").unwrap());
    let ready_path = PathBuf::from(std::env::var_os("AIFS_CLI_TEST_LOCK_READY").unwrap());
    let release_path = PathBuf::from(std::env::var_os("AIFS_CLI_TEST_LOCK_RELEASE").unwrap());
    let guard = IndexWriterGuard::acquire(&index_path).unwrap();
    fs::write(&ready_path, "ready").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !release_path.exists() {
        assert!(
            Instant::now() < deadline,
            "parent must release its owned child"
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(guard);
}

#[test]
fn index_under_root_excludes_writer_artifacts_but_not_unrelated_tmp_files() {
    let fixture = TestDir::new("index_artifacts");
    fixture.write_file("dataset/public.txt", "public");
    fixture.write_file("dataset/ordinary.tmp", "ordinary");
    fixture.write_file("dataset/snapshots/index.txt.lock", "persistent lock");
    fixture.write_file(
        "dataset/snapshots/.index.txt.aifs-tmp-stale",
        "owned namespace",
    );
    fixture.write_file("dataset/other/.index.txt.aifs-tmp-kept", "not adjacent");
    fixture.write_file(
        "dataset/snapshots/.other.txt.aifs-tmp-kept",
        "different index",
    );

    for command in ["index", "index"] {
        let output = fixture.command(&[command, "dataset", "dataset/snapshots/index.txt"]);
        assert_success(&output);
        assert_eq!(output.stdout, b"indexed 4 files\n");
        assert_artifact_free_snapshot(&fixture, 4);
    }
    assert_eq!(
        fs::read(fixture.path.join("dataset/snapshots/index.txt.lock")).unwrap(),
        b"persistent lock"
    );
    assert_eq!(
        fs::read(
            fixture
                .path
                .join("dataset/snapshots/.index.txt.aifs-tmp-stale")
        )
        .unwrap(),
        b"owned namespace"
    );
}

#[test]
fn refresh_and_status_under_root_exclude_writer_artifacts() {
    let fixture = TestDir::new("refresh_artifacts");
    fixture.write_file("dataset/public.txt", "public");
    assert_success(&fixture.command(&["index", "dataset", "dataset/snapshots/index.txt"]));
    fixture.write_file(
        "dataset/snapshots/.index.txt.aifs-tmp-stale",
        "owned namespace",
    );
    fixture.write_file("dataset/snapshots/index.txt.lock", "persistent lock");
    fixture.write_file("dataset/ordinary.tmp", "ordinary");
    let index_path = fixture.path.join("dataset/snapshots/index.txt");
    let original_bytes = fs::read(&index_path).unwrap();

    let status = fixture.command(&["status", "dataset", "dataset/snapshots/index.txt", "--json"]);
    assert_success(&status);
    assert_eq!(
        status.stdout,
        b"{\"scanned_files\":2,\"added\":1,\"updated\":0,\"removed\":0,\"unchanged\":1}\n"
    );
    assert_eq!(fs::read(&index_path).unwrap(), original_bytes);

    let refresh = fixture.command(&["refresh", "dataset", "dataset/snapshots/index.txt"]);
    assert_success(&refresh);
    assert_eq!(
        refresh.stdout,
        b"refreshed 2 files\nadded=1\nupdated=0\nremoved=0\nunchanged=1\n"
    );
    assert_artifact_free_snapshot(&fixture, 2);

    let status = fixture.command(&["status", "dataset", "dataset/snapshots/index.txt"]);
    assert_success(&status);
    assert_eq!(
        status.stdout,
        b"scanned 2 files\nadded=0\nupdated=0\nremoved=0\nunchanged=2\n"
    );
}

fn assert_artifact_free_snapshot(fixture: &TestDir, count: usize) {
    let query = fixture.command(&["query", "dataset/snapshots/index.txt", ""]);
    assert_success(&query);
    let stdout = String::from_utf8(query.stdout).unwrap();
    let paths = stdout.lines().collect::<Vec<_>>();
    assert_eq!(paths.len(), count, "{paths:?}");
    for excluded in [
        "snapshots/index.txt",
        "snapshots/index.txt.lock",
        "snapshots/.index.txt.aifs-tmp-stale",
    ] {
        assert!(!paths.contains(&excluded), "{paths:?}");
    }
    assert!(paths.contains(&"ordinary.tmp"), "{paths:?}");
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

fn assert_busy(output: &Output) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("index is busy"),
        "{output:?}"
    );
}

struct ChildGuard {
    child: Child,
    release_path: PathBuf,
}

impl ChildGuard {
    fn holding(fixture: &TestDir, index_path: &Path) -> Self {
        let ready_path = fixture.path.join("lock-ready");
        let release_path = fixture.path.join("lock-release");
        let child = Command::new(std::env::current_exe().unwrap())
            .current_dir(&fixture.path)
            .args(["--exact", "writer_lock_child", "--ignored", "--nocapture"])
            .env("AIFS_CLI_TEST_LOCK_INDEX", index_path)
            .env("AIFS_CLI_TEST_LOCK_READY", &ready_path)
            .env("AIFS_CLI_TEST_LOCK_RELEASE", &release_path)
            .spawn()
            .expect("lock-owner subprocess should start");
        let mut owner = Self {
            child,
            release_path,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready_path.exists() {
            assert!(
                owner.child.try_wait().unwrap().is_none(),
                "lock owner exited before ready"
            );
            assert!(Instant::now() < deadline, "lock owner did not become ready");
            thread::sleep(Duration::from_millis(10));
        }
        owner
    }

    fn release_normally(&mut self) {
        fs::write(&self.release_path, "release").unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status:?}");
                break;
            }
            assert!(Instant::now() < deadline, "lock owner did not exit");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminate(&mut self) {
        self.child
            .kill()
            .expect("only the owned child should be killed");
        assert!(!self.child.wait().unwrap().success());
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new(name: &str) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "ai-file-search-cli-writer-{name}-{}-{}",
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

    fn command(&self, args: &[&str]) -> Output {
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
