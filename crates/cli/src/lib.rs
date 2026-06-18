use std::path::Path;

use ai_file_search_indexer::{MemoryIndexStore, ScanOptions, Scanner};

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

    if args.len() != 3 || args[0] != "search" {
        return usage_error();
    }

    let root = Path::new(&args[1]);
    let query = &args[2];
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

fn usage_error() -> CliResult {
    CliResult {
        exit_code: 2,
        stdout: String::new(),
        stderr: "usage: ai-file-search search <root> <query>\n".to_owned(),
    }
}
