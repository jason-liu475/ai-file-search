use std::collections::BTreeMap;

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
