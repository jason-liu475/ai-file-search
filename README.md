# AI File Search

AI File Search is a fast, safe, memory-conscious cross-platform local file search engine designed to become a reliable low-cost data entry point for AI tools.

The project starts with a Rust core and a CLI prototype before adding the desktop UI and AI-facing local API.

## Goals

- Fast local file-name search.
- Safe path handling and explicit permission boundaries.
- Low idle memory and CPU usage.
- Cross-platform architecture for Windows, macOS, and Linux.
- A local API that AI tools can use without bypassing user control.

## Current Status

Milestone 1 is being built: Rust workspace, core path model, recursive scanner, in-memory index store, file-backed index store, CLI commands, and benchmark fixtures.

The current CLI is usable for local experiments. It is not yet a production desktop app.

## Quick Start

Generate a deterministic fixture dataset:

```bash
cargo run -p ai-file-search-cli -- fixture ./tmp-fixture 100
```

Run a scan/search benchmark over that dataset:

```bash
cargo run -p ai-file-search-cli -- bench ./tmp-fixture file-000042
```

Build a persistent index file:

```bash
cargo run -p ai-file-search-cli -- index ./tmp-fixture ./tmp-index.txt
```

Skip noisy directories by exact directory name while scanning:

```bash
cargo run -p ai-file-search-cli -- index ./my-repo ./repo-index.txt --exclude-name node_modules --exclude-name .git
```

Check pending index changes without rewriting the saved index:

```bash
cargo run -p ai-file-search-cli -- status ./tmp-fixture ./tmp-index.txt
```

Check pending index changes as JSON:

```bash
cargo run -p ai-file-search-cli -- status ./tmp-fixture ./tmp-index.txt --json
```

Read lightweight totals from the saved index without scanning the root:

```bash
cargo run -p ai-file-search-cli -- stats ./tmp-index.txt
```

Read lightweight totals as JSON:

```bash
cargo run -p ai-file-search-cli -- stats ./tmp-index.txt --json
```

Refresh a saved index after files change:

```bash
cargo run -p ai-file-search-cli -- refresh ./tmp-fixture ./tmp-index.txt
```

Query the saved index:

```bash
cargo run -p ai-file-search-cli -- query ./tmp-index.txt file-000042
```

Query with metadata as JSON for AI tools:

```bash
cargo run -p ai-file-search-cli -- query ./tmp-index.txt file-000042 --json
```

Run the lightweight JSON-RPC daemon over stdio:

```bash
cargo run -p ai-file-search-daemon -- stdio ./tmp-index.txt
```

Run the local IPC daemon for long-lived clients:

```bash
cargo run -p ai-file-search-daemon -- ipc ./tmp-index.txt aifs-search
```

Send one JSON-RPC request for local testing:

```bash
echo '{"id":1,"method":"stats","params":{}}' | cargo run -p ai-file-search-daemon -- stdio ./tmp-index.txt
```

Send the same request through the platform IPC transport:

```bash
echo '{"id":1,"method":"stats","params":{}}' | cargo run -p ai-file-search-daemon -- ipc-request aifs-search
```

Discover daemon JSON-RPC capabilities:

```bash
echo '{"id":1,"method":"methods","params":{}}' | cargo run -p ai-file-search-daemon -- ipc-request aifs-search
```

Run the daemon as a user-level background service:

```bash
cargo run -p ai-file-search-daemon -- service start ./tmp-index.txt
cargo run -p ai-file-search-daemon -- service status --json
echo '{"id":1,"method":"stats","params":{}}' | cargo run -p ai-file-search-daemon -- ipc-request aifs-service
cargo run -p ai-file-search-daemon -- service stop
```

For one-shot search without saving an index:

```bash
cargo run -p ai-file-search-cli -- search ./tmp-fixture file-000042
```

## CLI Commands

```text
ai-file-search search <root> <query> [--exclude-name <name>...]
ai-file-search index <root> <index-file> [--exclude-name <name>...]
ai-file-search refresh <root> <index-file> [--exclude-name <name>...]
ai-file-search status <root> <index-file> [--exclude-name <name>...] [--json]
ai-file-search stats <index-file> [--json]
ai-file-search query <index-file> <query> [--json]
ai-file-search bench <root> <query> [--exclude-name <name>...]
ai-file-search fixture <root> <count>
ai-file-search-daemon stdio <index-file>
ai-file-search-daemon ipc <index-file> <endpoint>
ai-file-search-daemon ipc-request <endpoint> [json-line]
ai-file-search-daemon service start <index-file> [--endpoint <name>]
ai-file-search-daemon service status [--json]
ai-file-search-daemon service stop
```

Current behavior:

- `search` scans a root directory and searches file names in memory.
- `index` scans a root directory and saves a lightweight local index file with normalized relative paths, file sizes, modified times, and the indexed root path.
- `refresh` rescans a root directory, replaces the saved index, and reports added, updated, removed, and unchanged counts.
- `status` rescans a root directory and reports added, updated, removed, and unchanged counts without rewriting the saved index, with optional JSON output.
- `stats` reads a saved index and reports file count and total indexed bytes without scanning the root directory, with optional JSON output.
- `query` searches a previously saved index file, with optional JSON output that includes path, file size, and modified time metadata.
- `bench` reports file count, match count, scan time, and search time.
- `fixture` creates deterministic files for repeatable local benchmarks.
- `ai-file-search-daemon stdio` keeps a process alive and serves newline-delimited JSON-RPC over stdin/stdout for lightweight AI-tool integration.
- `ai-file-search-daemon ipc` serves the same JSON-RPC protocol over Windows Named Pipe or Unix Domain Socket for local long-lived clients.
- `ai-file-search-daemon ipc-request` sends one newline-delimited JSON-RPC request to a local IPC endpoint, either from stdin or the optional command argument.
- `ai-file-search-daemon service start/status/stop` manages a user-level background daemon over the platform IPC transport.
- `ai-file-search-daemon service start` requires an index file with stored root metadata; `index_status`, `refresh`, and `reindex` reject explicit roots that differ from that stored root.
- `--exclude-name <name>` can be repeated on scanning commands to skip directories with an exact file name match, such as `node_modules`, `.git`, or `target`.

## JSON-RPC Methods

The daemon serves newline-delimited JSON-RPC-like requests over stdio and platform IPC:

```text
methods  -> returns protocol version and available method names
ping     -> returns {"status":"ok"}
index_status -> params {"root":"optional with stored root metadata (if supplied, must match); otherwise required","exclude_names":["optional"]}; returns needs_refresh and change counts without saving
refresh  -> params {"root":"optional; must match stored root","exclude_names":["optional"]}
reindex  -> alias of refresh
stats    -> returns saved-index file and byte totals
search   -> params {"query":"string","limit":20}
shutdown -> asks the daemon to stop
```

## MVP Limitations

- The persistent store is a simple versioned text file, not SQLite, Tantivy, or an external database.
- Search is file-name substring search only.
- File watching and true incremental updates are not implemented yet; `index_status` and `refresh` currently perform full rescans.
- OS service installation, start-on-login, authentication, and multi-user access controls are not implemented yet.
- Content indexing is not implemented yet.
- Desktop UI and AI-facing local API are planned after the CLI/core path is stable.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

Apache-2.0. See [LICENSE](LICENSE).
