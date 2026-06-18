use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ai_file_search_core::PathId;

use crate::IndexedFile;

#[derive(Clone, Debug, Default)]
pub struct MemoryIndexStore {
    files: BTreeMap<String, IndexedFile>,
}

impl MemoryIndexStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn upsert_file(&mut self, file: IndexedFile) {
        self.files
            .insert(file.relative_path.as_normalized().to_owned(), file);
    }

    pub fn remove_path(&mut self, path: &PathId) {
        self.files.remove(path.as_normalized());
    }

    #[must_use]
    pub fn all_files(&self) -> Vec<IndexedFile> {
        self.files.values().cloned().collect()
    }

    #[must_use]
    pub fn search_by_name(&self, query: &str) -> Vec<IndexedFile> {
        let query = query.to_lowercase();

        self.files
            .values()
            .filter(|file| file_name(file).to_lowercase().contains(&query))
            .cloned()
            .collect()
    }
}

fn file_name(file: &IndexedFile) -> &str {
    file.relative_path
        .as_normalized()
        .rsplit('/')
        .next()
        .unwrap_or_default()
}

#[derive(Clone, Debug)]
pub struct FileIndexStore {
    path: PathBuf,
    memory: MemoryIndexStore,
}

impl FileIndexStore {
    /// Opens an index file, creating an empty in-memory store when the file does
    /// not exist yet.
    ///
    /// # Errors
    ///
    /// Returns an error when the index file exists but cannot be read.
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut memory = MemoryIndexStore::new();

        if path.exists() {
            let contents = fs::read_to_string(path)?;
            for line in contents.lines().filter(|line| !line.is_empty()) {
                memory.upsert_file(IndexedFile {
                    relative_path: PathId::from_user_path(line),
                });
            }
        }

        Ok(Self {
            path: path.to_owned(),
            memory,
        })
    }

    pub fn upsert_file(&mut self, file: IndexedFile) {
        self.memory.upsert_file(file);
    }

    pub fn remove_path(&mut self, path: &PathId) {
        self.memory.remove_path(path);
    }

    /// Saves the current index to disk.
    ///
    /// # Errors
    ///
    /// Returns an error when the parent directory cannot be created or the index
    /// file cannot be written.
    pub fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let mut contents = self
            .memory
            .all_files()
            .iter()
            .map(|file| file.relative_path.as_normalized())
            .collect::<Vec<_>>()
            .join("\n");
        if !contents.is_empty() {
            contents.push('\n');
        }

        fs::write(&self.path, contents)
    }

    #[must_use]
    pub fn search_by_name(&self, query: &str) -> Vec<IndexedFile> {
        self.memory.search_by_name(query)
    }
}
