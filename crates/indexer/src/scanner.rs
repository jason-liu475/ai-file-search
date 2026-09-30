use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ai_file_search_core::PathId;

use crate::writer_lock::{adjacent_lock_path, publication_prefix, resolve_index_path};

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ScanOptions {
    excluded_names: BTreeSet<String>,
}

impl ScanOptions {
    #[must_use]
    pub fn exclude_name(mut self, name: impl Into<String>) -> Self {
        self.excluded_names.insert(name.into());
        self
    }

    pub fn excluded_names(&self) -> impl Iterator<Item = &str> {
        self.excluded_names.iter().map(String::as_str)
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
        self.scan_with_artifacts(root, None)
    }

    /// Scans while excluding this index, its lock and its publication namespace.
    ///
    /// # Errors
    /// Returns an error when the root, index identity or an entry cannot be read.
    pub fn scan_for_index(&self, root: &Path, index_path: &Path) -> io::Result<Vec<IndexedFile>> {
        self.scan_for_index_with_artifacts(root, index_path, &[])
    }

    /// Also excludes the exact resolved runtime files supplied by the owner.
    ///
    /// Missing artifacts are resolved without creating directories. Their names
    /// do not exclude similarly named user files elsewhere in the scanned tree.
    ///
    /// # Errors
    /// Returns an error for inaccessible roots/entries or invalid file identities.
    pub fn scan_for_index_with_artifacts(
        &self,
        root: &Path,
        index_path: &Path,
        runtime_artifacts: &[PathBuf],
    ) -> io::Result<Vec<IndexedFile>> {
        let root = fs::canonicalize(root)?;
        let index = resolve_index_path(index_path, false)?;
        let runtime_artifacts = runtime_artifacts
            .iter()
            .map(|path| resolve_index_path(path, false))
            .collect::<io::Result<BTreeSet<_>>>()?;
        let artifacts = IndexArtifacts::new(&index, runtime_artifacts);
        self.scan_with_artifacts(&root, Some(&artifacts))
    }

    fn scan_with_artifacts(
        &self,
        root: &Path,
        artifacts: Option<&IndexArtifacts>,
    ) -> io::Result<Vec<IndexedFile>> {
        let mut files = Vec::new();
        self.scan_directory(root, root, artifacts, &mut files)?;

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
        artifacts: Option<&IndexArtifacts>,
        files: &mut Vec<IndexedFile>,
    ) -> io::Result<()> {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                if !self.options.excludes(&path) {
                    self.scan_directory(root, &path, artifacts, files)?;
                }
            } else if file_type.is_file() {
                if artifacts.is_some_and(|artifacts| artifacts.excludes(&path)) {
                    continue;
                }
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

struct IndexArtifacts {
    index: PathBuf,
    lock: PathBuf,
    publication_prefix: std::ffi::OsString,
    runtime_artifacts: BTreeSet<PathBuf>,
}

impl IndexArtifacts {
    fn new(index: &Path, runtime_artifacts: BTreeSet<PathBuf>) -> Self {
        let lock = adjacent_lock_path(index);
        let lock =
            if fs::symlink_metadata(&lock).is_ok_and(|metadata| metadata.file_type().is_file()) {
                fs::canonicalize(&lock).unwrap_or(lock)
            } else {
                lock
            };
        Self {
            index: index.to_path_buf(),
            lock,
            publication_prefix: publication_prefix(index),
            runtime_artifacts,
        }
    }

    fn excludes(&self, path: &Path) -> bool {
        if path == self.index || path == self.lock || self.runtime_artifacts.contains(path) {
            return true;
        }
        if path.parent() != self.index.parent() {
            return false;
        }
        let Some(name) = path.file_name() else {
            return false;
        };
        let name = name.as_encoded_bytes();
        let prefix = self.publication_prefix.as_encoded_bytes();
        #[cfg(windows)]
        {
            name.get(..prefix.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        }
        #[cfg(not(windows))]
        {
            name.starts_with(prefix)
        }
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
