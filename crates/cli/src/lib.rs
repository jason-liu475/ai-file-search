use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use ai_file_search_indexer::{
    FileIndexStore, IndexedFile, MemoryIndexStore, RefreshSummary, ScanOptions, Scanner,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CliResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn run(args: impl IntoIterator<Item = impl AsRef<str>>) -> CliResult {
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect::<Vec<_>>();

    match args.first().map(String::as_str) {
        Some("search") if args.len() == 3 => search(&args[1], &args[2]),
        Some("index") if args.len() == 3 => index(&args[1], &args[2]),
        Some("refresh") if args.len() == 3 => refresh(&args[1], &args[2]),
        Some("status") if args.len() == 3 => status(&args[1], &args[2]),
        Some("query") if args.len() == 3 => query(&args[1], &args[2]),
        Some("bench") if args.len() == 3 => bench(&args[1], &args[2]),
        Some("fixture") if args.len() == 3 => fixture(&args[1], &args[2]),
        _ => usage_error(),
    }
}

fn search(root: &str, query: &str) -> CliResult {
    let root = Path::new(root);
    let scanner = Scanner::new(ScanOptions::default());
    let files = match scanner.scan(root) {
        Ok(files) => files,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("scan failed: {error}\n"),
            };
        }
    };

    let mut store = MemoryIndexStore::new();
    for file in files {
        store.upsert_file(file);
    }

    let stdout = store
        .search_by_name(query)
        .iter()
        .map(|file| file.relative_path.as_normalized())
        .collect::<Vec<_>>()
        .join("\n");
    let stdout = if stdout.is_empty() {
        stdout
    } else {
        format!("{stdout}\n")
    };

    CliResult {
        exit_code: 0,
        stdout,
        stderr: String::new(),
    }
}

fn query(index_path: &str, query: &str) -> CliResult {
    let store = match FileIndexStore::open(Path::new(index_path)) {
        Ok(store) => store,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("index open failed: {error}\n"),
            };
        }
    };

    let stdout = store
        .search_by_name(query)
        .iter()
        .map(|file| file.relative_path.as_normalized())
        .collect::<Vec<_>>()
        .join("\n");
    let stdout = if stdout.is_empty() {
        stdout
    } else {
        format!("{stdout}\n")
    };

    CliResult {
        exit_code: 0,
        stdout,
        stderr: String::new(),
    }
}

fn index(root: &str, index_path: &str) -> CliResult {
    let root = Path::new(root);
    let index_path = Path::new(index_path);
    let files = match scan_files_for_index(root, index_path) {
        Ok(files) => files,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("scan failed: {error}\n"),
            };
        }
    };
    let file_count = files.len();

    let mut store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("index open failed: {error}\n"),
            };
        }
    };
    for file in files {
        store.upsert_file(file);
    }
    if let Err(error) = store.save() {
        return CliResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("index save failed: {error}\n"),
        };
    }

    CliResult {
        exit_code: 0,
        stdout: format!("indexed {file_count} files\n"),
        stderr: String::new(),
    }
}

fn status(root: &str, index_path: &str) -> CliResult {
    let root = Path::new(root);
    let index_path = Path::new(index_path);
    let files = match scan_files_for_index(root, index_path) {
        Ok(files) => files,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("scan failed: {error}\n"),
            };
        }
    };
    let file_count = files.len();

    let store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("index open failed: {error}\n"),
            };
        }
    };
    let old_files = store.all_files();
    let summary = RefreshSummary::compare(&old_files, &files);

    CliResult {
        exit_code: 0,
        stdout: format_summary("scanned", file_count, &summary),
        stderr: String::new(),
    }
}

fn refresh(root: &str, index_path: &str) -> CliResult {
    let root = Path::new(root);
    let index_path = Path::new(index_path);
    let files = match scan_files_for_index(root, index_path) {
        Ok(files) => files,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("scan failed: {error}\n"),
            };
        }
    };
    let file_count = files.len();

    let mut store = match FileIndexStore::open(index_path) {
        Ok(store) => store,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("index open failed: {error}\n"),
            };
        }
    };
    let old_files = store.all_files();
    let summary = RefreshSummary::compare(&old_files, &files);
    store.replace_all(files);
    if let Err(error) = store.save() {
        return CliResult {
            exit_code: 1,
            stdout: String::new(),
            stderr: format!("index save failed: {error}\n"),
        };
    }

    CliResult {
        exit_code: 0,
        stdout: format_summary("refreshed", file_count, &summary),
        stderr: String::new(),
    }
}

fn format_summary(action: &str, file_count: usize, summary: &RefreshSummary) -> String {
    format!(
        "{action} {file_count} files\nadded={}\nupdated={}\nremoved={}\nunchanged={}\n",
        summary.added, summary.updated, summary.removed, summary.unchanged
    )
}

fn scan_files_for_index(root: &Path, index_path: &Path) -> io::Result<Vec<IndexedFile>> {
    let scanner = Scanner::new(ScanOptions::default());
    let mut files = scanner.scan(root)?;

    if let Some(index_relative_path) = relative_index_path(root, index_path) {
        files.retain(|file| file.relative_path.as_normalized() != index_relative_path);
    }

    Ok(files)
}

fn relative_index_path(root: &Path, index_path: &Path) -> Option<String> {
    index_path.strip_prefix(root).ok().map(|relative_path| {
        relative_path
            .components()
            .collect::<PathBuf>()
            .to_string_lossy()
            .replace('\\', "/")
    })
}

fn usage_error() -> CliResult {
    CliResult {
        exit_code: 2,
        stdout: String::new(),
        stderr: "usage: ai-file-search <search <root> <query>|index <root> <index-file>|refresh <root> <index-file>|status <root> <index-file>|query <index-file> <query>|bench <root> <query>|fixture <root> <count>>\n".to_owned(),
    }
}

fn bench(root: &str, query: &str) -> CliResult {
    let root = Path::new(root);
    let scanner = Scanner::new(ScanOptions::default());

    let scan_start = Instant::now();
    let files = match scanner.scan(root) {
        Ok(files) => files,
        Err(error) => {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("scan failed: {error}\n"),
            };
        }
    };
    let scan_ms = scan_start.elapsed().as_millis();
    let file_count = files.len();

    let mut store = MemoryIndexStore::new();
    for file in files {
        store.upsert_file(file);
    }

    let search_start = Instant::now();
    let matches = store.search_by_name(query);
    let search_ms = search_start.elapsed().as_millis();

    CliResult {
        exit_code: 0,
        stdout: format!(
            "files={file_count}\nmatches={}\nscan_ms={scan_ms}\nsearch_ms={search_ms}\n",
            matches.len()
        ),
        stderr: String::new(),
    }
}

fn fixture(root: &str, count: &str) -> CliResult {
    let count = match count.parse::<usize>() {
        Ok(count) => count,
        Err(error) => {
            return CliResult {
                exit_code: 2,
                stdout: String::new(),
                stderr: format!("invalid count: {error}\n"),
            };
        }
    };
    let root = Path::new(root);

    for index in 0..count {
        let group = index / 1_000;
        let directory = root.join(format!("group-{group:03}"));
        if let Err(error) = fs::create_dir_all(&directory) {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("fixture directory create failed: {error}\n"),
            };
        }

        let file_path = directory.join(format!("file-{index:06}.txt"));
        let contents = format!("fixture file {index:06}\n");
        if let Err(error) = fs::write(file_path, contents) {
            return CliResult {
                exit_code: 1,
                stdout: String::new(),
                stderr: format!("fixture file write failed: {error}\n"),
            };
        }
    }

    CliResult {
        exit_code: 0,
        stdout: format!("generated {count} files\n"),
        stderr: String::new(),
    }
}
