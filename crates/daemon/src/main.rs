use std::io::{self, BufRead, Read, Write};
use std::path::Path;

use ai_file_search_daemon::handle_json_line;

#[tokio::main]
async fn main() {
    let exit_code = async_main().await;
    std::process::exit(exit_code);
}

async fn async_main() -> i32 {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match args.first().map(String::as_str) {
        Some("stdio") if args.len() == 2 => serve_stdio(&args[1]),
        Some("handle") if args.len() == 3 => handle_once(&args[1], &args[2]),
        Some("ipc") if args.len() == 3 => serve_ipc(&args[1], &args[2]).await,
        Some("ipc-request") if args.len() == 2 => ipc_request_stdin(&args[1]).await,
        Some("ipc-request") if args.len() == 3 => ipc_request(&args[1], &args[2]).await,
        _ => {
            eprintln!(
                "usage: ai-file-search-daemon <stdio <index-file>|handle <index-file> <json-line>|ipc <index-file> <endpoint>|ipc-request <endpoint> [json-line]>"
            );
            2
        }
    }
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

async fn serve_ipc(index_path: &str, endpoint: &str) -> i32 {
    match ai_file_search_daemon::serve_ipc(Path::new(index_path), endpoint).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("ipc serve failed: {error}");
            1
        }
    }
}

async fn ipc_request(endpoint: &str, line: &str) -> i32 {
    match ai_file_search_daemon::send_ipc_request(endpoint, line).await {
        Ok(response) => {
            print!("{response}");
            0
        }
        Err(error) => {
            eprintln!("ipc request failed: {error}");
            1
        }
    }
}

async fn ipc_request_stdin(endpoint: &str) -> i32 {
    let mut line = String::new();
    if let Err(error) = io::stdin().read_to_string(&mut line) {
        eprintln!("stdin read failed: {error}");
        return 1;
    }

    ipc_request(endpoint, line.trim_end()).await
}
