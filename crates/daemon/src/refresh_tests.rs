use super::*;
use std::cell::Cell;
use std::fs;

#[test]
fn custom_state_path_is_an_explicit_absolute_child_env_not_inherited_state() {
    let state_path = PathBuf::from("custom-state/owned.runtime");
    let command = service_child_command(
        Path::new("index.txt"),
        "aifs-custom-state",
        Some(300),
        &state_path,
    )
    .unwrap_or_else(|result| panic!("{}", result.stderr));
    let explicit_env = command.get_envs().collect::<Vec<_>>();
    assert_eq!(explicit_env.len(), 1);
    assert_eq!(explicit_env[0].0, SERVICE_STATE_ENV);
    let resolved = std::path::absolute(&state_path).unwrap();
    assert_eq!(
        explicit_env[0].1,
        Some(resolved.as_os_str()),
        "explicit env must work even when parent has no SERVICE_STATE_ENV or has a different one"
    );
    assert_eq!(
        command.get_args().collect::<Vec<_>>(),
        [
            "service-run",
            "index.txt",
            "aifs-custom-state",
            "--auto-refresh-seconds",
            "300"
        ]
    );
    assert!(
        !resolved.exists(),
        "constructing child context must not create state directories"
    );
}

#[test]
fn automatic_unchanged_scans_once_without_mutating_or_saving() {
    let fixture = Fixture::new("unchanged", false);
    let before = fs::read(&fixture.index).unwrap();
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let mut writer = FileIndexWriter::open(&mut guard).unwrap();
    let scans = Cell::new(0);
    let saves = Cell::new(0);
    let comparison = scan_and_compare(
        &writer,
        &fixture.index,
        &json!({"root": fixture.root}),
        None,
        &[],
        |options, root, index, artifacts| {
            scans.set(scans.get() + 1);
            scan_index(options, root, index, artifacts)
        },
    )
    .unwrap();
    let (count, summary) = publish_comparison(&mut writer, comparison, true, |writer| {
        saves.set(saves.get() + 1);
        writer.save()
    })
    .unwrap();
    assert_eq!(count, 3);
    assert_eq!(
        summary,
        RefreshSummary {
            unchanged: 3,
            ..RefreshSummary::default()
        }
    );
    assert_eq!((scans.get(), saves.get()), (1, 0));
    assert!(
        writer.root_path().is_none(),
        "no-change policy must not add root metadata"
    );
    assert!(writer.scan_policy().is_none());
    assert_eq!(fs::read(&fixture.index).unwrap(), before);
}

#[test]
fn automatic_changed_scans_once_and_publishes_once_preserving_scope() {
    let fixture = Fixture::new("changed", true);
    fixture.change();
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let mut writer = FileIndexWriter::open(&mut guard).unwrap();
    let scans = Cell::new(0);
    let saves = Cell::new(0);
    let comparison = scan_and_compare(
        &writer,
        &fixture.index,
        &json!({}),
        None,
        &[],
        |options, root, index, artifacts| {
            scans.set(scans.get() + 1);
            scan_index(options, root, index, artifacts)
        },
    )
    .unwrap();
    let (count, summary) = publish_comparison(&mut writer, comparison, true, |writer| {
        saves.set(saves.get() + 1);
        assert!(IndexWriterGuard::acquire(&fixture.index).is_err());
        writer.save()
    })
    .unwrap();
    assert_eq!((scans.get(), saves.get()), (1, 1));
    assert_eq!(count, 3);
    assert_eq!(
        summary,
        RefreshSummary {
            added: 1,
            updated: 1,
            removed: 1,
            unchanged: 1
        }
    );
    assert_eq!(writer.root_path(), Some(fixture.root.as_path()));
    assert_eq!(
        writer.scan_policy(),
        Some(&ScanOptions::default().exclude_name("target"))
    );
    let snapshot = FileIndexStore::open(&fixture.index).unwrap();
    assert_eq!(snapshot.file_count(), 3);
    assert!(snapshot.search_by_name("removed").is_empty());
    assert!(snapshot.search_by_name("secret").is_empty());
    assert_eq!(snapshot.search_by_name("added").len(), 1);
    assert_eq!(snapshot.search_by_name("updated")[0].size_bytes, 11);
}

#[test]
fn manual_unchanged_still_saves_once_and_sets_explicit_root() {
    let fixture = Fixture::new("manual", false);
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let mut writer = FileIndexWriter::open(&mut guard).unwrap();
    let comparison = scan_and_compare(
        &writer,
        &fixture.index,
        &json!({"root": fixture.root}),
        None,
        &[],
        scan_index,
    )
    .unwrap();
    let saves = Cell::new(0);
    publish_comparison(&mut writer, comparison, false, |writer| {
        saves.set(saves.get() + 1);
        writer.save()
    })
    .unwrap();
    assert_eq!(saves.get(), 1);
    assert_eq!(writer.root_path(), Some(fixture.root.as_path()));
    assert!(
        writer.scan_policy().is_none(),
        "legacy unknown policy stays unknown"
    );
}

#[test]
fn read_only_comparison_scans_once_and_status_never_needs_write_capability() {
    let fixture = Fixture::new("status", true);
    fixture.change();
    let before = fs::read(&fixture.index).unwrap();
    let _owner = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let reader = FileIndexStore::open(&fixture.index).unwrap();
    let scans = Cell::new(0);
    let comparison = scan_and_compare(
        &reader,
        &fixture.index,
        &json!({}),
        None,
        &[],
        |options, root, index, artifacts| {
            scans.set(scans.get() + 1);
            scan_index(options, root, index, artifacts)
        },
    )
    .unwrap();
    assert_eq!(scans.get(), 1);
    assert_eq!(
        comparison.summary,
        RefreshSummary {
            added: 1,
            updated: 1,
            removed: 1,
            unchanged: 1
        }
    );
    let response = handle_json_line(
        &fixture.index,
        r#"{"id":1,"method":"index_status","params":{}}"#,
    );
    assert_eq!(
        response,
        Response::success(
            1,
            json!({"scanned_files":3, "added":1,
        "updated":1, "removed":1, "unchanged":1, "needs_refresh":true})
        )
    );
    assert_eq!(fs::read(&fixture.index).unwrap(), before);
}

#[test]
fn failed_scan_preserves_bytes_and_never_reaches_publication() {
    let fixture = Fixture::new("missing-root", true);
    fs::remove_dir_all(&fixture.root).unwrap();
    let before = fs::read(&fixture.index).unwrap();
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let mut writer = FileIndexWriter::open(&mut guard).unwrap();
    let scans = Cell::new(0);
    let saves = Cell::new(0);
    let result = scan_and_compare(
        &writer,
        &fixture.index,
        &json!({}),
        None,
        &[],
        |options, root, index, artifacts| {
            scans.set(scans.get() + 1);
            scan_index(options, root, index, artifacts)
        },
    )
    .and_then(|comparison| {
        publish_comparison(&mut writer, comparison, true, |writer| {
            saves.set(saves.get() + 1);
            writer.save()
        })
    });
    assert!(result.unwrap_err().starts_with("scan failed:"));
    assert_eq!((scans.get(), saves.get()), (1, 0));
    assert_eq!(fs::read(&fixture.index).unwrap(), before);
}

#[test]
fn save_failure_is_reported_once_without_replacing_old_bytes() {
    let fixture = Fixture::new("save-failure", true);
    fixture.change();
    let before = fs::read(&fixture.index).unwrap();
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let mut writer = FileIndexWriter::open(&mut guard).unwrap();
    let comparison =
        scan_and_compare(&writer, &fixture.index, &json!({}), None, &[], scan_index).unwrap();
    let saves = Cell::new(0);
    let result = publish_comparison(&mut writer, comparison, true, |_| {
        saves.set(saves.get() + 1);
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "injected save failure",
        ))
    });
    assert_eq!(
        result.unwrap_err(),
        "index save failed: injected save failure"
    );
    assert_eq!(saves.get(), 1);
    assert_eq!(fs::read(&fixture.index).unwrap(), before);
}

#[test]
fn root_and_policy_errors_precede_scan_and_preserve_old_bytes() {
    let fixture = Fixture::new("scope-priority", true);
    let before = fs::read(&fixture.index).unwrap();
    let reader = FileIndexStore::open(&fixture.index).unwrap();
    let mismatch = Some(ScanOptions::default());
    for (params, error) in [
        (
            json!({"root": fixture.path.join("other")}),
            "root does not match stored index root",
        ),
        (json!({}), "exclude_names does not match stored scan policy"),
    ] {
        let result = scan_and_compare(
            &reader,
            &fixture.index,
            &params,
            mismatch.clone(),
            &[],
            |_, _, _, _| panic!("invalid scope must not scan"),
        );
        assert_eq!(result.err().unwrap(), error);
    }
    assert_eq!(fs::read(&fixture.index).unwrap(), before);
}

#[test]
fn shared_scan_inherits_policy_and_excludes_only_exact_runtime_artifact() {
    let fixture = Fixture::new("exact-artifacts", true);
    fs::create_dir_all(fixture.root.join("elsewhere")).unwrap();
    let state = fixture.root.join("owned.runtime");
    fs::write(&state, "managed state").unwrap();
    fs::write(fixture.root.join("elsewhere/owned.runtime"), "user file").unwrap();
    fs::write(fixture.root.join("ordinary.tmp"), "ordinary").unwrap();
    let reader = FileIndexStore::open(&fixture.index).unwrap();
    let artifacts = [state];
    let comparison = scan_and_compare(
        &reader,
        &fixture.index,
        &json!({}),
        None,
        &artifacts,
        |options, root, index, actual_artifacts| {
            assert_eq!(options, ScanOptions::default().exclude_name("target"));
            assert_eq!(root, fixture.root);
            assert_eq!(index, fixture.index);
            assert_eq!(actual_artifacts, artifacts);
            scan_index(options, root, index, actual_artifacts)
        },
    )
    .unwrap();
    assert_eq!(
        comparison.summary,
        RefreshSummary {
            added: 2,
            unchanged: 3,
            ..RefreshSummary::default()
        }
    );
    assert_eq!(comparison.files.len(), 5);
    assert!(
        !comparison
            .files
            .iter()
            .any(|file| file.relative_path.as_normalized() == "owned.runtime")
    );
    assert!(
        comparison
            .files
            .iter()
            .any(|file| file.relative_path.as_normalized() == "elsewhere/owned.runtime")
    );
    assert!(
        comparison
            .files
            .iter()
            .any(|file| file.relative_path.as_normalized() == "ordinary.tmp")
    );
}

#[test]
fn root_type_validation_keeps_status_and_legacy_manual_priority() {
    let fixture = Fixture::new("root-type", true);
    assert_eq!(
        handle_json_line(
            &fixture.index,
            r#"{"id":1,"method":"index_status","params":{"root":1}}"#
        ),
        Response::error(1, "root must be a string")
    );
    let response = handle_json_line(
        &fixture.index,
        r#"{"id":1,"method":"refresh","params":{"root":1}}"#,
    );
    assert_eq!(
        response,
        Response::success(
            1,
            json!({"scanned_files":3, "added":0,
        "updated":0, "removed":0, "unchanged":3})
        )
    );
}

#[cfg(windows)]
#[test]
fn real_save_replacement_failure_keeps_disk_bytes_and_reports_error() {
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new("replacement-failure", true);
    fixture.change();
    let before = fs::read(&fixture.index).unwrap();
    let mut guard = IndexWriterGuard::acquire(&fixture.index).unwrap();
    let mut writer = FileIndexWriter::open(&mut guard).unwrap();
    let comparison =
        scan_and_compare(&writer, &fixture.index, &json!({}), None, &[], scan_index).unwrap();
    let _deny_replacement = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&fixture.index)
        .unwrap();
    let result = publish_comparison(&mut writer, comparison, true, FileIndexWriter::save);
    assert!(result.unwrap_err().starts_with("index save failed:"));
    assert_eq!(fs::read(&fixture.index).unwrap(), before);
    assert_eq!(
        fs::read_dir(&fixture.path).unwrap().count(),
        3,
        "failed publication must clean its temp file"
    );
}

struct Fixture {
    path: PathBuf,
    root: PathBuf,
    index: PathBuf,
}

impl Fixture {
    fn new(name: &str, known_scope: bool) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "aifs-shared-refresh-{name}-{}-{nonce}",
            std::process::id()
        ));
        let root = path.join("root");
        fs::create_dir_all(root.join("target")).unwrap();
        for file in ["unchanged.txt", "updated.txt", "removed.txt"] {
            fs::write(root.join(file), "old").unwrap();
        }
        let index = path.join("index.txt");
        let mut guard = IndexWriterGuard::acquire(&index).unwrap();
        let options = ScanOptions::default().exclude_name("target");
        let files = Scanner::new(options.clone())
            .scan_for_index(&root, guard.index_path())
            .unwrap();
        let mut writer = FileIndexWriter::new(&mut guard);
        if known_scope {
            writer.set_root_path(&root);
            writer.set_scan_policy(options);
        }
        writer.replace_all(files);
        writer.save().unwrap();
        Self { path, root, index }
    }
    fn change(&self) {
        fs::write(self.root.join("updated.txt"), "new content").unwrap();
        fs::remove_file(self.root.join("removed.txt")).unwrap();
        fs::write(self.root.join("added.txt"), "added").unwrap();
        fs::write(self.root.join("target/secret.txt"), "secret").unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
