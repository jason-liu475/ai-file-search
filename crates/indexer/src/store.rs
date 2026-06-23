use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ai_file_search_core::PathId;

use crate::IndexedFile;

const INDEX_HEADER: &str = "aifs-index-v1";

#[derive(Clone, Debug, Default)]
pub struct MemoryIndexStore {
    files: BTreeMap<String, IndexedFile>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RefreshSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
}

impl RefreshSummary {
    #[must_use]
    pub fn compare(old_files: &[IndexedFile], new_files: &[IndexedFile]) -> Self {
        let old_by_path = files_by_path(old_files);
        let new_by_path = files_by_path(new_files);

        let mut summary = Self::default();

        for (path, new_file) in &new_by_path {
            match old_by_path.get(path) {
                Some(old_file) if same_file_metadata(old_file, new_file) => {
                    summary.unchanged += 1;
                }
                Some(_) => {
                    summary.updated += 1;
                }
                None => {
                    summary.added += 1;
                }
            }
        }

        for path in old_by_path.keys() {
            if !new_by_path.contains_key(path) {
                summary.removed += 1;
            }
        }

        summary
    }
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

    pub fn replace_all(&mut self, files: Vec<IndexedFile>) {
        self.files.clear();
        for file in files {
            self.upsert_file(file);
        }
    }

    pub fn remove_path(&mut self, path: &PathId) {
        self.files.remove(path.as_normalized());
    }

    #[must_use]
    pub fn all_files(&self) -> Vec<IndexedFile> {
        self.files.values().cloned().collect()
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    #[must_use]
    pub fn total_size_bytes(&self) -> u64 {
        self.files.values().map(|file| file.size_bytes).sum()
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

fn files_by_path(files: &[IndexedFile]) -> BTreeMap<&str, &IndexedFile> {
    files
        .iter()
        .map(|file| (file.relative_path.as_normalized(), file))
        .collect()
}

fn same_file_metadata(left: &IndexedFile, right: &IndexedFile) -> bool {
    left.size_bytes == right.size_bytes && left.modified_unix_seconds == right.modified_unix_seconds
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
            let mut lines = contents.lines();
            let has_header = lines.next().is_some_and(|line| line == INDEX_HEADER);
            let records: Box<dyn Iterator<Item = &str> + '_> = if has_header {
                Box::new(lines)
            } else {
                Box::new(contents.lines())
            };

            for line in records.filter(|line| !line.is_empty()) {
                memory.upsert_file(parse_index_record(line, has_header));
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

    pub fn replace_all(&mut self, files: Vec<IndexedFile>) {
        self.memory.replace_all(files);
    }

    pub fn remove_path(&mut self, path: &PathId) {
        self.memory.remove_path(path);
    }

    #[must_use]
    pub fn all_files(&self) -> Vec<IndexedFile> {
        self.memory.all_files()
    }

    #[must_use]
    pub fn file_count(&self) -> usize {
        self.memory.file_count()
    }

    #[must_use]
    pub fn total_size_bytes(&self) -> u64 {
        self.memory.total_size_bytes()
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

        let mut lines = vec![INDEX_HEADER.to_owned()];
        lines.extend(self.memory.all_files().iter().map(format_index_record));

        let mut contents = lines.join("\n");
        contents.push('\n');

        let temporary_path = temporary_index_path(&self.path);
        fs::write(&temporary_path, contents)?;
        fs::rename(temporary_path, &self.path)
    }

    #[must_use]
    pub fn search_by_name(&self, query: &str) -> Vec<IndexedFile> {
        self.memory.search_by_name(query)
    }
}

fn parse_index_record(line: &str, has_header: bool) -> IndexedFile {
    if has_header {
        let mut parts = line.splitn(3, '\t');
        let size_bytes = parts
            .next()
            .and_then(|size| size.parse::<u64>().ok())
            .unwrap_or_default();
        let modified_unix_seconds = parts
            .next()
            .and_then(|size| size.parse::<u64>().ok())
            .unwrap_or_default();
        let path = parts.next().unwrap_or_default();

        IndexedFile {
            relative_path: PathId::from_user_path(path),
            size_bytes,
            modified_unix_seconds,
        }
    } else {
        IndexedFile {
            relative_path: PathId::from_user_path(line),
            size_bytes: 0,
            modified_unix_seconds: 0,
        }
    }
}

fn format_index_record(file: &IndexedFile) -> String {
    [
        file.size_bytes.to_string(),
        file.modified_unix_seconds.to_string(),
        file.relative_path.as_normalized().to_owned(),
    ]
    .join("\t")
}

fn temporary_index_path(path: &Path) -> PathBuf {
    let mut temporary_path = path.to_owned();
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map_or_else(|| "tmp".to_owned(), |extension| format!("{extension}.tmp"));
    temporary_path.set_extension(extension);
    temporary_path
}
