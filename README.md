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

Check pending index changes without rewriting the saved index:

```bash
cargo run -p ai-file-search-cli -- status ./tmp-fixture ./tmp-index.txt
```

Read lightweight totals from the saved index without scanning the root:

```bash
cargo run -p ai-file-search-cli -- stats ./tmp-index.txt
```

Refresh a saved index after files change:

```bash
cargo run -p ai-file-search-cli -- refresh ./tmp-fixture ./tmp-index.txt
```

Query the saved index:

```bash
cargo run -p ai-file-search-cli -- query ./tmp-index.txt file-000042
```

For one-shot search without saving an index:

```bash
cargo run -p ai-file-search-cli -- search ./tmp-fixture file-000042
```

## CLI Commands

```text
ai-file-search search <root> <query>
ai-file-search index <root> <index-file>
ai-file-search refresh <root> <index-file>
ai-file-search status <root> <index-file>
ai-file-search stats <index-file>
ai-file-search query <index-file> <query>
ai-file-search bench <root> <query>
ai-file-search fixture <root> <count>
```

Current behavior:

- `search` scans a root directory and searches file names in memory.
- `index` scans a root directory and saves a lightweight local index file with normalized relative paths, file sizes, and modified times.
- `refresh` rescans a root directory, replaces the saved index, and reports added, updated, removed, and unchanged counts.
- `status` rescans a root directory and reports added, updated, removed, and unchanged counts without rewriting the saved index.
- `stats` reads a saved index and reports file count and total indexed bytes without scanning the root directory.
- `query` searches a previously saved index file.
- `bench` reports file count, match count, scan time, and search time.
- `fixture` creates deterministic files for repeatable local benchmarks.

## MVP Limitations

- The persistent store is a simple versioned text file, not SQLite, Tantivy, or an external database.
- Search is file-name substring search only.
- File watching and true incremental updates are not implemented yet; `refresh` currently does a full rescan.
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
