use std::cmp::Ordering;
use std::iter::Peekable;

use crate::{IndexedFile, RefreshSummary};

impl RefreshSummary {
    /// Compares borrowed records in linear time with constant extra space.
    ///
    /// Both inputs must be sorted in nondecreasing normalized-path order. For
    /// adjacent duplicate paths, the last record wins, matching [`Self::compare`].
    #[must_use]
    pub fn compare_ordered<'old, 'new>(
        old_files: impl IntoIterator<Item = &'old IndexedFile>,
        new_files: impl IntoIterator<Item = &'new IndexedFile>,
    ) -> Self {
        let mut old_files = old_files.into_iter().peekable();
        let mut new_files = new_files.into_iter().peekable();
        let mut old = next_last_record(&mut old_files);
        let mut new = next_last_record(&mut new_files);
        let mut summary = Self::default();

        loop {
            match (old, new) {
                (Some(left), Some(right)) => match left
                    .relative_path
                    .as_normalized()
                    .cmp(right.relative_path.as_normalized())
                {
                    Ordering::Less => {
                        summary.removed += 1;
                        old = next_last_record(&mut old_files);
                    }
                    Ordering::Greater => {
                        summary.added += 1;
                        new = next_last_record(&mut new_files);
                    }
                    Ordering::Equal => {
                        if left.size_bytes == right.size_bytes
                            && left.modified_unix_seconds == right.modified_unix_seconds
                        {
                            summary.unchanged += 1;
                        } else {
                            summary.updated += 1;
                        }
                        old = next_last_record(&mut old_files);
                        new = next_last_record(&mut new_files);
                    }
                },
                (Some(_), None) => {
                    summary.removed += 1;
                    old = next_last_record(&mut old_files);
                }
                (None, Some(_)) => {
                    summary.added += 1;
                    new = next_last_record(&mut new_files);
                }
                (None, None) => break,
            }
        }
        summary
    }

    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.added != 0 || self.updated != 0 || self.removed != 0
    }
}

fn next_last_record<'a>(
    files: &mut Peekable<impl Iterator<Item = &'a IndexedFile>>,
) -> Option<&'a IndexedFile> {
    let mut last = files.next()?;
    while files.peek().is_some_and(|next| {
        next.relative_path.as_normalized() == last.relative_path.as_normalized()
    }) {
        last = files.next()?;
    }
    Some(last)
}
