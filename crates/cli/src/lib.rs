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
    let Ok(parsed) = ParsedArgs::parse(&args) else {
        return usage_error();
    };

    match parsed.command.as_deref() {
        Some("search") if parsed.positionals.len() == 2 => search(
            &parsed.positionals[0],
            &parsed.positionals[1],
            parsed.scan_options(),
        ),
        Some("index") if parsed.positionals.len() == 2 => index(
            &parsed.positionals[0],
            &parsed.positionals[1],
            parsed.scan_options(),
        ),
        Some("refresh") if parsed.positionals.len() == 2 => refresh(
            &parsed.positionals[0],
            &parsed.positionals[1],
            parsed.scan_options(),
        ),
        Some("status") if parsed.positionals.len() == 2 => status(
            &parsed.positionals[0],
            &parsed.positionals[1],
            parsed.scan_options(),
        ),
        Some("stats") if parsed.positionals.len() == 1 && parsed.excluded_names.is_empty() => {
            stats(&parsed.positionals[0])
        }
        Some("query") if parsed.positionals.len() == 2 && parsed.excluded_names.is_empty() => {
            query(&parsed.positionals[0], &parsed.positionals[1])
        }
        Some("bench") if parsed.positionals.len() == 2 => bench(
            &parsed.positionals[0],
            &parsed.positionals[1],
            parsed.scan_options(),
        ),
        Some("fixture") if parsed.positionals.len() == 2 && parsed.excluded_names.is_empty() => {
            fixture(&parsed.positionals[0], &parsed.positionals[1])
        }
        _ => usage_error(),
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct ParsedArgs {
    command: Option<String>,
    positionals: Vec<String>,
    excluded_names: Vec<String>,
}

impl ParsedArgs {
    fn parse(args: &[String]) -> Result<Self, ()> {
        let mut parsed = Self {
            command: args.first().cloned(),
            ..Self::default()
        };
        let mut index = 1;

        while index < args.len() {
            match args[index].as_str() {
                "--exclude-name" => {
                    let name = args.get(index + 1).ok_or(())?;
                    if name.is_empty() {
                        return Err(());
                    }
                    parsed.excluded_names.push(name.clone());
                    index += 2;
                }
                argument if argument.starts_with("--") => return Err(()),
                _ => {
                    parsed.positionals.push(args[index].clone());
                    index += 1;
                }
            }
        }

        Ok(parsed)
    }

    fn scan_options(&self) -> ScanOptions {
        self.excluded_names
            .iter()
            .fold(ScanOptions::default(), |options, name| {
                options.exclude_name(name.clone())
            })
    }
}

fn search(root: &str, query: &str, options: ScanOptions) -> CliResult {
    let root = Path::new(root);
    let scanner = Scanner::new(options);
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

fn stats(index_path: &str) -> CliResult {
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

    CliResult {
        exit_code: 0,
        stdout: format!(
            "files={}\ntotal_bytes={}\n",
            store.file_count(),
            store.total_size_bytes()
        ),
        stderr: String::new(),
    }
}

fn index(root: &str, index_path: &str, options: ScanOptions) -> CliResult {
    let root = Path::new(root);
    let index_path = Path::new(index_path);
    let files = match scan_files_for_index(root, index_path, options) {
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

fn status(root: &str, index_path: &str, options: ScanOptions) -> CliResult {
    let root = Path::new(root);
    let index_path = Path::new(index_path);
    let files = match scan_files_for_index(root, index_path, options) {
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

fn refresh(root: &str, index_path: &str, options: ScanOptions) -> CliResult {
    let root = Path::new(root);
    let index_path = Path::new(index_path);
    let files = match scan_files_for_index(root, index_path, options) {
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

fn scan_files_for_index(
    root: &Path,
    index_path: &Path,
    options: ScanOptions,
) -> io::Result<Vec<IndexedFile>> {
    let scanner = Scanner::new(options);
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
        stderr: "usage: ai-file-search <search <root> <query> [--exclude-name <name>...]|index <root> <index-file> [--exclude-name <name>...]|refresh <root> <index-file> [--exclude-name <name>...]|status <root> <index-file> [--exclude-name <name>...]|stats <index-file>|query <index-file> <query>|bench <root> <query> [--exclude-name <name>...]|fixture <root> <count>>\n".to_owned(),
    }
}

fn bench(root: &str, query: &str, options: ScanOptions) -> CliResult {
    let root = Path::new(root);
    let scanner = Scanner::new(options);

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
