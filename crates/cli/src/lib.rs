use std::path::Path;
use std::time::Instant;

use ai_file_search_indexer::{FileIndexStore, MemoryIndexStore, ScanOptions, Scanner};

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
        Some("query") if args.len() == 3 => query(&args[1], &args[2]),
        Some("bench") if args.len() == 3 => bench(&args[1], &args[2]),
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
    let file_count = files.len();

    let mut store = match FileIndexStore::open(Path::new(index_path)) {
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

fn usage_error() -> CliResult {
    CliResult {
        exit_code: 2,
        stdout: String::new(),
        stderr: "usage: ai-file-search <search <root> <query>|index <root> <index-file>|query <index-file> <query>|bench <root> <query>>\n".to_owned(),
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
