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

Implemented prototype slices include the Rust core/scanner, in-memory and streaming text-file stores, persisted scan exclusions, guarded index writers, safe snapshot publication, borrowed ordered metadata comparison, CLI commands, metadata JSON-RPC over stdio/local IPC, manual refresh/reindex/index status, and user-level service start/status/stop.

The CLI and local daemon are usable for experiments. They are not yet a production desktop app or an authenticated AI data-access boundary.

Automatic refresh is configuration-only at commit `fd2f6a9`: `--auto-refresh-seconds` is parsed and recorded, but `service-run` does not schedule scans yet. Use manual `refresh` until runtime implementation and acceptance tests are complete.

The [reviewed automatic-refresh design](docs/superpowers/specs/2026-07-10-service-auto-refresh-design.md) and [implementation plan](docs/superpowers/plans/2026-07-10-service-auto-refresh.md) separate completed configuration, scan-policy persistence, writer isolation, streaming publication/loading and borrowed comparison from pending bounded connections, scheduling, and platform/performance gates.

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
ai-file-search-daemon service start <index-file> [--endpoint <name>] [--auto-refresh-seconds <seconds>]
ai-file-search-daemon service status [--json]
ai-file-search-daemon service stop
```

Current behavior:

- `search` scans a root directory and searches file names in memory.
- `index` explicitly rebuilds a local index with normalized relative paths, file sizes, modified times, canonical absolute root, and sorted/deduplicated scan exclusions. Rebuilding replaces old entries and is the explicit way to change root or exclusion scope.
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
- `--auto-refresh-seconds` accepts `30..=86400` and appears in service status only when configured. It currently records configuration only; it does not run automatic refresh.
- `ai-file-search-daemon service start` requires an index file with stored root metadata; `index_status`, `refresh`, and `reindex` reject explicit roots that differ from that stored root.
- `--exclude-name <name>` can be repeated on scanning commands to skip directories with an exact file name match, such as `node_modules`, `.git`, or `target`.
- For indices built by this version, `refresh`/`status` and JSON-RPC `refresh`/`reindex`/`index_status` inherit stored exclusions when omitted. Explicit exclusions must match the stored set; a mismatch fails before scanning or rewriting the index. Stop any service before explicitly rebuilding its index.
- Legacy indices without a policy marker keep their previous manual default/explicit-exclusion behavior. Manual refresh does not guess or confirm their original policy. Configured auto-refresh startup requires a known policy and an absolute, resolvable directory root; rebuild legacy indices with the intended exclusions before opting in.
- CLI `index`/`refresh` and direct/stdio RPC writes acquire one nonblocking writer lock before opening or scanning. Manual IPC and managed service processes hold it for their entire lifetime and reuse it for their own refresh requests. External writes fail with `index is busy`; query/stat/status reads do not acquire this lock. Send refresh to the owning service or stop it before using CLI writes.
- `<index-filename>.lock` remains beside the index after exit. Do not delete it to clear contention: the OS releases ownership when the process exits. Scanning excludes this index, its lock and its adjacent `.<index-filename>.aifs-tmp-*` publication namespace, not arbitrary `.tmp` files.
- Snapshot save streams into an exclusively created same-directory temporary file, flushes/syncs/closes it, then replaces the destination without deleting the old index first. A failure before replacement preserves the old bytes and cleans only this attempt's temporary file. A crash can leave a reserved temporary artifact; it is not indexed or reused.
- Managed RPC scans also exclude the actual service-state file, including a relative `AIFS_SERVICE_STATE` resolved at startup and explicitly forwarded to the child. This is an exact path exclusion, not a generic `service-state.json` name filter. Direct/stdio/manual IPC calls have no managed-state context and do not guess which user files to hide.
- Manual `refresh`/`reindex` still publish explicitly even with zero file changes. The shared refresh operation has a tested conditional-publication mode for the future scheduler, but no automatic timer is connected yet. `index_status` uses the same scan/compare path and never publishes.

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
- Automatic refresh scheduling is not implemented yet. `index_status` is a full scan, not a cheap health check; calling it and then `refresh` performs two scans.
- Legacy scan policy is unknown until an explicit rebuild. Older readers can query new metadata, but older writers drop policy records; do not mix old and new writers. Malformed or unsupported policy metadata is rejected without changing the file.
- Full scans and parsed indices still use O(N) metadata memory. Loading reuses one line buffer, save streams borrowed records, and CLI/RPC refresh comparison merges ordered borrowed records without cloning the saved snapshot or constructing extra path maps. Parsed metadata, candidate scan records and the longest input record still contribute to peak memory. Actual process-memory, latency and cross-platform performance budgets have not been validated.
- `stats` avoids rescanning the filesystem root, but currently still loads and parses the entire saved index; it is not a constant-time metadata lookup.
- Writer locking resolves existing index paths and canonical parents of new paths, including supported relative/absolute and parent-symlink aliases. Hard-link index aliases, older/noncooperating writers, network-filesystem guarantees, and hostile directories are unsupported. Keep index/state/endpoint files in a trusted user-controlled directory; a cooperative lock is not authorization.
- New Unix lock/publication files request mode `0600`; Windows files inherit the destination directory ACL. No custom Windows ACL or preservation of an old file-specific ACL is promised. Atomic replacement/power-loss durability and concurrent-client behavior still need native Linux/macOS acceptance; current verification is Windows-only.
- OS service installation, start-on-login, authentication, and multi-user access controls are not implemented yet.
- Managed IPC still needs bounded connections, safe stale-endpoint cleanup, and stronger instance ownership. Local-only transport does not by itself authorize callers or isolate users.
- Content indexing is not implemented yet.
- Desktop UI and a production-safe AI authorization profile are planned after the CLI/core path is stable; the prototype local JSON-RPC API already exists.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## License

Apache-2.0. See [LICENSE](LICENSE).
