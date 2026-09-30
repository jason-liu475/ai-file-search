use std::cell::Cell;

use ai_file_search_core::PathId;
use ai_file_search_indexer::{IndexedFile, RefreshSummary};

#[test]
fn ordered_comparison_matches_oracle_for_all_small_metadata_states() {
    let fixtures = (0..64).map(metadata_state).collect::<Vec<_>>();
    for old in &fixtures {
        for new in &fixtures {
            let ordered = RefreshSummary::compare_ordered(old, new);
            assert_eq!(
                ordered,
                RefreshSummary::compare(old, new),
                "old={old:?} new={new:?}"
            );
            assert_eq!(ordered.has_changes(), old != new);
        }
    }
}

#[test]
fn adjacent_duplicate_paths_keep_last_record_without_additional_maps() {
    let old = [
        file("a.txt", 1, 1),
        file("a.txt", 2, 2),
        file("b.txt", 1, 1),
        file("b.txt", 9, 9),
    ];
    let new = [
        file("a.txt", 2, 2),
        file("b.txt", 1, 1),
        file("b.txt", 4, 4),
        file("c.txt", 1, 1),
        file("c.txt", 7, 7),
    ];
    assert_eq!(
        RefreshSummary::compare_ordered(&old, &new),
        RefreshSummary::compare(&old, &new)
    );
    assert_eq!(
        RefreshSummary::compare_ordered(&old, &new),
        RefreshSummary {
            added: 1,
            updated: 1,
            removed: 0,
            unchanged: 1
        }
    );
}

#[test]
fn iterators_are_consumed_once_including_duplicate_runs_and_remaining_tail() {
    let old = [
        file("a.txt", 1, 1),
        file("a.txt", 2, 2),
        file("b.txt", 1, 1),
        file("z.txt", 1, 1),
    ];
    let new = [
        file("b.txt", 1, 1),
        file("c.txt", 1, 1),
        file("c.txt", 2, 2),
    ];
    let old_reads = Cell::new(0);
    let new_reads = Cell::new(0);
    let summary = RefreshSummary::compare_ordered(
        old.iter().inspect(|_| old_reads.set(old_reads.get() + 1)),
        new.iter().inspect(|_| new_reads.set(new_reads.get() + 1)),
    );
    assert_eq!(
        summary,
        RefreshSummary {
            added: 1,
            updated: 0,
            removed: 2,
            unchanged: 1
        }
    );
    assert_eq!(old_reads.get(), old.len());
    assert_eq!(new_reads.get(), new.len());
}

#[test]
fn unchanged_counts_do_not_require_publication() {
    assert!(
        !RefreshSummary {
            unchanged: 12,
            ..RefreshSummary::default()
        }
        .has_changes()
    );
    for summary in [
        RefreshSummary {
            added: 1,
            ..RefreshSummary::default()
        },
        RefreshSummary {
            updated: 1,
            ..RefreshSummary::default()
        },
        RefreshSummary {
            removed: 1,
            ..RefreshSummary::default()
        },
    ] {
        assert!(summary.has_changes());
    }
}

#[test]
fn unicode_and_normalized_path_order_match_existing_comparison() {
    let mut old = vec![
        file("z.txt", 1, 1),
        file("a\\b.txt", 2, 2),
        file("\u{4e2d}.txt", 3, 3),
    ];
    let mut new = vec![file("\u{4e2d}.txt", 3, 4), file("./a/b.txt", 2, 2)];
    old.sort_by(|a, b| {
        a.relative_path
            .as_normalized()
            .cmp(b.relative_path.as_normalized())
    });
    new.sort_by(|a, b| {
        a.relative_path
            .as_normalized()
            .cmp(b.relative_path.as_normalized())
    });
    assert_eq!(
        RefreshSummary::compare_ordered(&old, &new),
        RefreshSummary::compare(&old, &new)
    );
}

fn metadata_state(mut code: u32) -> Vec<IndexedFile> {
    let mut files = Vec::new();
    for path in ["a/report.txt", "b.txt", "c.txt"] {
        match code % 4 {
            1 => files.push(file(path, 1, 1)),
            2 => files.push(file(path, 2, 1)),
            3 => files.push(file(path, 1, 2)),
            _ => {}
        }
        code /= 4;
    }
    files
}

fn file(path: &str, size_bytes: u64, modified_unix_seconds: u64) -> IndexedFile {
    IndexedFile {
        relative_path: PathId::from_user_path(path),
        size_bytes,
        modified_unix_seconds,
    }
}
