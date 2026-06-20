use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ai_file_search_core::PathId;

#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    excluded_names: BTreeSet<String>,
}

impl ScanOptions {
    #[must_use]
    pub fn exclude_name(mut self, name: impl Into<String>) -> Self {
        self.excluded_names.insert(name.into());
        self
    }

    fn excludes(&self, path: &Path) -> bool {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| self.excluded_names.contains(name))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedFile {
    pub relative_path: PathId,
    pub size_bytes: u64,
    pub modified_unix_seconds: u64,
}

#[derive(Clone, Debug)]
pub struct Scanner {
    options: ScanOptions,
}

impl Scanner {
    #[must_use]
    pub fn new(options: ScanOptions) -> Self {
        Self { options }
    }

    /// Scans `root` and returns indexed file metadata.
    ///
    /// # Errors
    ///
    /// Returns an error when the root directory cannot be read or when a
    /// directory entry cannot be inspected.
    pub fn scan(&self, root: &Path) -> io::Result<Vec<IndexedFile>> {
        let mut files = Vec::new();
        self.scan_directory(root, root, &mut files)?;

        files.sort_by(|left, right| {
            left.relative_path
                .as_normalized()
                .cmp(right.relative_path.as_normalized())
        });

        Ok(files)
    }

    fn scan_directory(
        &self,
        root: &Path,
        directory: &Path,
        files: &mut Vec<IndexedFile>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                if !self.options.excludes(&path) {
                    self.scan_directory(root, &path, files)?;
                }
            } else if file_type.is_file() {
                let metadata = entry.metadata()?;
                let relative_path = relative_path(root, &path);
                files.push(IndexedFile {
                    relative_path: PathId::from_user_path(&relative_path),
                    size_bytes: metadata.len(),
                    modified_unix_seconds: modified_unix_seconds(&metadata),
                });
            }
        }

        Ok(())
    }
}

fn modified_unix_seconds(metadata: &fs::Metadata) -> u64 {
    metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_secs())
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .collect::<PathBuf>()
        .to_string_lossy()
        .into_owned()
}
