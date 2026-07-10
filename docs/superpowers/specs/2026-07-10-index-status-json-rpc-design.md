# Index Status JSON-RPC Design

## Goal

Add a read-only JSON-RPC method that lets local AI clients determine whether the saved file index is stale before requesting a write operation.

The method must preserve the daemon's low-memory, local-only architecture and reuse the safety boundary already enforced by `refresh` and `reindex`.

## Scope

Add one daemon method:

```text
index_status
```

The method performs a full scan of the stored index root, compares the scan with the saved index, and returns a machine-readable change summary. It never rewrites the index file.

Also update:

- The JSON-RPC method catalog
- Daemon handler tests
- User-facing JSON-RPC documentation in `README.md`

## Non-Goals

This slice does not add:

- File-system watching
- Scheduled or automatic refresh
- Incremental event processing
- Content indexing
- A new transport or binary protocol
- Changes to service lifecycle commands

## Chosen Approach

Use a dedicated `index_status` JSON-RPC method.

This is preferred over a `dry_run` flag on `refresh` because the operation is explicitly read-only and cannot be confused with a write request. It is preferred over file watching at this stage because it adds no persistent watcher state, platform-specific dependencies, or idle memory cost.

The method runs over the daemon's existing local IPC transport:

- Windows Named Pipe on Windows
- Unix Domain Socket on Linux and macOS
- JSON-RPC request and response framing already used by the daemon

HTTP is not involved.

## Request Contract

Example using the root stored in index metadata:

```json
{"id":11,"method":"index_status","params":{}}
```

Optional parameters match `refresh` and `reindex`:

```json
{
  "id": 11,
  "method": "index_status",
  "params": {
    "root": "C:\\Users\\example\\Documents",
    "exclude_names": ["node_modules", ".git"]
  }
}
```

Parameter rules:

- `root` is optional when the index contains stored root metadata.
- An explicitly supplied `root` must resolve to the same root stored in the index.
- When neither stored root metadata nor an explicit `root` exists, return `missing string param: root`.
- `exclude_names` is optional and must be an array of strings.

## Response Contract

When changes exist:

```json
{
  "id": 11,
  "result": {
    "added": 2,
    "needs_refresh": true,
    "removed": 1,
    "scanned_files": 42,
    "unchanged": 39,
    "updated": 0
  }
}
```

When the index is current:

```json
{
  "id": 11,
  "result": {
    "added": 0,
    "needs_refresh": false,
    "removed": 0,
    "scanned_files": 42,
    "unchanged": 42,
    "updated": 0
  }
}
```

`needs_refresh` is `true` when any of `added`, `updated`, or `removed` is greater than zero. The `unchanged` count does not affect it.

JSON object key order is controlled by the existing serialization behavior and is asserted by handler tests where the repository already treats serialized lines as a stable interface.

## Data Flow

1. Parse the JSON-RPC request through the existing protocol layer.
2. Parse `exclude_names` with the existing scan option helper.
3. Open the saved `FileIndexStore`.
4. Resolve the root through the same root safety helper used by `refresh`.
5. Scan the root with the existing scanner, excluding the index file itself when it resides below the root.
6. Compare saved and scanned files with `RefreshSummary::compare`.
7. Compute `needs_refresh` from added, updated, and removed counts.
8. Return the summary without calling `replace_all`, `set_root_path`, or `save`.

## Internal Boundaries

The daemon handler remains the owner of the operation. No new crate or dependency is required.

The implementation should share small internal helpers with `refresh` only where this removes duplicated response construction or comparison logic. It must not broaden public APIs merely for this feature.

The existing `refresh_root` helper may be renamed to reflect its use by both read-only status and refresh operations, provided behavior and error messages remain stable.

## Error Handling

Errors follow current daemon conventions:

- Index open failure: `index open failed: <reason>`
- Missing root: `missing string param: root`
- Root mismatch: `root does not match stored index root`
- Invalid exclusions: `exclude_names must be an array of strings`
- Scan failure: `scan failed: <reason>`

An error response must not alter the saved index.

## Safety Properties

- The operation is read-only with respect to the index file.
- A service-bound index cannot be used to scan an unrelated explicit root.
- The index file itself remains excluded when it is located inside the scanned root.
- No network listener or HTTP endpoint is introduced.
- The method performs no content reads beyond file metadata already required by the scanner.

## Testing Strategy

Use test-driven development for every behavior.

Handler tests must cover:

1. `methods` advertises `index_status` and its optional parameters.
2. A current index returns `needs_refresh: false` and zero changed counts.
3. A stale index returns `needs_refresh: true` with correct added, updated, removed, unchanged, and scanned counts.
4. Omitting `root` uses stored root metadata.
5. Supplying a mismatched root returns the existing safety error.
6. A successful status request leaves the index file byte-for-byte unchanged.
7. Invalid `exclude_names` returns the existing validation error.

The focused daemon tests must pass before the complete workspace verification.

## Required Verification

Before implementation commit:

```bash
cargo fmt --check
cargo test -p ai-file-search-daemon --test handler_tests
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

## Follow-Up

After this method is stable, clients can poll it on demand or at a conservative interval and call `refresh` only when `needs_refresh` is true. Native file watching remains a separate future design because its correctness, resource use, and cross-platform behavior need independent evaluation.
