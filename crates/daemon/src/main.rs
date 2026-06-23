use std::io::{self, BufRead, Write};
use std::path::Path;

use ai_file_search_daemon::handle_json_line;

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let exit_code = match args.first().map(String::as_str) {
        Some("stdio") if args.len() == 2 => serve_stdio(&args[1]),
        Some("handle") if args.len() == 3 => handle_once(&args[1], &args[2]),
        _ => {
            eprintln!(
                "usage: ai-file-search-daemon <stdio <index-file>|handle <index-file> <json-line>>"
            );
            2
        }
    };

    std::process::exit(exit_code);
}

fn serve_stdio(index_path: &str) -> i32 {
    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("stdin read failed: {error}");
                return 1;
            }
        };
        let response = handle_json_line(Path::new(index_path), &line);
        if let Err(error) = stdout.write_all(response.to_json_line().as_bytes()) {
            eprintln!("stdout write failed: {error}");
            return 1;
        }
        if let Err(error) = stdout.flush() {
            eprintln!("stdout flush failed: {error}");
            return 1;
        }
    }

    0
}

fn handle_once(index_path: &str, line: &str) -> i32 {
    let response = handle_json_line(Path::new(index_path), line);
    print!("{}", response.to_json_line());
    0
}
