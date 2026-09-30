# Service Auto Refresh Design

Reviewed: 2026-09-30. This revision supersedes the original timer, storage, and connection assumptions.

## Implementation Status

Implemented: interval parsing, child argument forwarding, startup-state rendering, scan-policy persistence, manual policy inheritance/mismatch rejection, explicit rebuilds, configured-start root/policy validation, cross-process writer ownership, unique streaming snapshot publication/loading, borrowed ordered comparison, a shared scan/publication operation, and managed-state path forwarding/self-exclusion. Task 5a adds bounded single-request managed connections, bounded health/shutdown clients, startup/instance locks, structured service identity, atomic capped state publication and explicit lifecycle statuses. Task 5b adds current-user Windows pipe access restrictions and Unix private runtime directories, endpoint ownership and verified stale recovery; acceptance evidence is recorded in the implementation plan. The current `service_run` still uses the interval only for prerequisite validation, without scheduling scans. Conditional publication is a tested common operation, not an active timer. Scheduled refresh and `refresh_status` below are NOT implemented. Native platform and performance gates must be evaluated independently of source implementation.

The configuration flag is not evidence that an automatic scan has run. Do not document this as an active feature until the implementation and platform acceptance gates pass.

## Goal And Scope

Provide opt-in metadata refresh in the managed user-level service using one serial operation owner and no HTTP, watcher, extra process, or dedicated scanning worker.

```text
ai-file-search-daemon service start <index-file> [--endpoint <name>] [--auto-refresh-seconds <seconds>]
ai-file-search-daemon service-run <index-file> <endpoint> [--auto-refresh-seconds <seconds>]
```

- Disabled by default; accept decimal integers in `30..=86400`.
- Duplicate, unknown, missing, nonnumeric, and out-of-range flags return usage exit code `2`.
- Starting an already-running instance requires matching index, endpoint and interval; mismatches fail and require stop/start.
- Changing configuration requires a stop/start.
- Legacy service-state JSON omitting `auto_refresh_seconds` loads as `None`.
- JSON status includes the configured interval only for `Some`; text appends `auto refresh: <seconds>s`.
- State JSON describes startup configuration, not refresh progress, and is not rewritten per scan.

Non-goals: file watching, incremental indexing, content reads, runtime reconfiguration, persistent refresh history, concurrent scan workers, and OS-service installation. Low latency during a full scan and constant-memory indexing are NOT guarantees of this phase.

## Enablement Prerequisites

Before connecting the timer to production writes, implement and test:

1. Persisted scan scope, with unknown legacy policy handled explicitly.
2. A cross-process single-writer contract used by every supported write entry point.
3. Safe, failure-preserving snapshot publication.
4. Bounded service connections and safe instance ownership.

These safeguards also benefit manual refresh. Introducing a timer must not turn existing prototype limitations into unattended data corruption or unintended scope expansion.

## Scan Scope And Compatibility

The index now persists `--exclude-name` options. Using `ScanOptions::default()` during automatic refresh would still silently add excluded directories; the future scheduler must use the persisted policy.

The existing `aifs-index-v1` metadata records now include a policy marker and repeated excluded directory names:

```text
meta<TAB>scan_policy<TAB>1
meta<TAB>exclude_name<TAB>.git
meta<TAB>exclude_name<TAB>node_modules
```

`<TAB>` denotes the existing tab delimiter, not literal text. Reuse metadata escaping for values. Normalize exclusions as a sorted, deduplicated set; preserve the scanner's exact-name comparison behavior.

- Marker with no exclusions means explicitly empty exclusions.
- No marker means UNKNOWN policy, never an implicit empty policy.
- `FileIndexStore::open` rejects unsupported versions, malformed policy records, duplicate markers, and exclusions without a marker as `InvalidData`. Explicit CLI `index` builds a fresh snapshot without reading the old one and can recover from invalid policy metadata.
- New explicit `index` builds persist the chosen policy and an absolute canonical root.
- Enabling automatic refresh on a legacy index requires an explicit rebuild using `index <root> <index-file> [--exclude-name ...]` with the intended scope. Do not guess the original exclusions or silently rewrite a relative root.
- CLI `refresh`/`status` and daemon `refresh`/`reindex`/`index_status` inherit a known stored policy when exclusions are omitted. Explicit exclusions must match it; a mismatch returns a scope error without scanning or writing. Changing policy requires an explicit CLI rebuild while no service owns the index.
- For legacy manual operations, preserve the existing explicit/default exclusion behavior and root safety errors. Ordinary refresh does not mark an unknown policy as confirmed.
- Older readers currently skip unknown metadata and can still query these files. Older writers drop the new metadata and do not honor writer locks; mixing versions as writers is unsupported. Missing policy on the next open must fail closed for automatic refresh.

Root equality uses filesystem-aware resolution, not raw string comparison. Reject relative or unresolvable stored roots for automatic startup; read-only legacy use remains available. A full scan is not a transactional filesystem snapshot: concurrent user file changes may appear in the next scan.

## Single Writer And Safe Publication

The serial service loop only prevents overlap WITHIN that process. CLI `index`/`refresh`, daemon `handle`/`stdio`/`ipc` writes, and another managed instance otherwise remain separate writers.

The indexer/storage boundary now implements an RAII writer guard:

- Resolve one stable absolute index identity, canonicalizing existing paths and the parent of a new file. Document supported aliases; do not claim a path-based lock serializes hard-link aliases or hostile external writers.
- Use an adjacent `<index>.lock` file with an OS exclusive lock. Open it read/write without truncation; use a nonblocking acquisition and report a busy index rather than waiting indefinitely.
- Managed service and manually started IPC hold the guard for their lifetime. CLI, direct-handler and stdio writes acquire it BEFORE opening the old snapshot, scanning, comparing, and saving.
- Those servers reuse their held guard instead of recursively acquiring an OS lock. `FileIndexStore` is a cloneable read-only snapshot. `FileIndexWriter::new/open` borrows `&mut IndexWriterGuard` and exposes mutation/save only for that guard's destination; it has neither `Clone` nor `DerefMut`. Queries, stats and index status still use read-only snapshots.
- Keep the lock file in place after release. Unlinking a locked file permits a second lock identity on Unix.
- The lock is cooperative coordination, not an access-control boundary. Index and lock files must live in an owner-controlled directory; reject unsafe file types. A process that bypasses the contract is unsupported.

The workspace MSRV is 1.96. `std::fs::File::try_lock` is available without adding a lock dependency; verify platform behavior with subprocess tests. See [Rust file locking](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock).

Snapshot publication must:

1. Finish scanning and comparing before mutating the published snapshot.
2. Create a unique temporary file in the destination directory using exclusive `create_new`; retry a name collision without opening, truncating, or deleting an existing path.
3. Stream borrowed entries through `BufWriter`, explicitly flush, then sync the file. New Unix lock/temp files request mode `0600` (umask may further restrict); Windows inherits the trusted directory's ACL. This does not preserve an old index's custom file-specific ACL or establish caller authorization.
4. Close handles as required by the platform and replace atomically on supported local filesystems. Never remove the old index first to work around a rename failure.
5. On a pre-publication failure, preserve old index bytes and clean up only the temporary file created by this attempt.
6. After publication, do not report a rollback that did not happen. Power-loss durability and network-filesystem lock/rename semantics are not guaranteed in this MVP.

A fixed `<index>.tmp` is unacceptable: it permits write races and can follow a pre-existing link. Exclusive creation prevents that particular path-reuse problem; it is not a substitute for a private directory or writer isolation.

Publication uses `.<index-filename>.aifs-tmp-<pid>-<counter>` with bounded exclusive-creation retries. Existing collisions/links and legacy `<index>.tmp` files are untouched. Owned temporaries are removed on ordinary pre-publication failure; process termination can leave a reserved artifact. `Scanner::scan_for_index` centrally excludes the resolved index, adjacent lock and same-directory reserved prefix without per-entry canonicalization or a blanket `.tmp` filter. `scan_for_index_with_artifacts` also excludes explicit runtime file identities and their adjacent `.<filename>.aifs-tmp-*` namespaces, resolved once per scan without creating missing directories. Managed service supplies its actual state path and startup/instance locks; atomic state temporaries use the same reserved naming convention. Direct/stdio/manual IPC does not infer that context.

Relative/absolute paths and resolvable parent aliases share ownership. An existing final symlink resolves to its target, and the writer publishes to that target. Path locks do not unify different hard-link names. Lock-path symlinks/nonregular types are rejected; protection against hostile directory replacement, writers ignoring locks and network-filesystem semantics is outside this contract. Existing readers may finish using the old snapshot while newly opened readers see the replacement on supported local filesystems.

Exclude the exact index path, lock path, resolved service-state path, and owned temporary-file namespace when they lie under the scanned root. Do not exclude every file ending in `.tmp`. Pass the actual absolute state path from startup to the child context, including `AIFS_SERVICE_STATE` overrides, so self-exclusion cannot depend on a different working directory.

## Scheduling Contract

Use a fixed-delay monotonic deadline, not a periodic interval.

1. After startup validation, binding, and readiness, set `next_due = now + period`. Do not scan during startup.
2. Wait for either one service connection or `sleep_until(next_due)`.
3. When due, perform one complete scan/compare/publication attempt. After success OR failure, set `next_due = completion_time + period`.
4. After a successful manual `refresh` or `reindex`, also move the deadline to completion plus one period. Failed manual writes and read-only requests do not reset it.
5. If a deadline became due during request handling, perform at most one automatic attempt before accepting more ordinary requests. Repeated requests cannot starve the timer.
6. After a slow scan or machine suspension, never run a backlog of scans. A full idle interval follows the completed attempt.
7. Automatic and manual scans never overlap. Shutdown during a scan is handled when the synchronous operation completes; it is not scan cancellation.

Tokio `MissedTickBehavior::Skip` skips scheduled instants but an overdue `tick` can resolve immediately, with another deadline less than a full period later. It does not establish this completion-relative contract. See [Tokio missed-tick behavior](https://docs.rs/tokio/latest/tokio/time/enum.MissedTickBehavior.html).

A minimum interval alone cannot bound scan cost. Slow storage may block service replies, including `ping` and `shutdown`. This limitation must remain visible until a separately reviewed cooperative/chunked scan design exists.

## Connection Contract

Selecting a timer only while accepting a connection is insufficient: a connected client that sends no newline or holds a Unix stream open can otherwise stop the scheduler forever.

For the MANAGED service on both platforms:

- One newline-delimited request and one response per connection, then close. Clients reconnect for subsequent requests.
- Maximum incoming frame: 64 KiB including newline. Read into a capped buffer and reject over-limit or unterminated frames; do not allocate an unbounded `String` first.
- A 5-second total frame-read deadline from connection acceptance, not a fresh timeout per byte.
- A 5-second response-write deadline; slow/nonreading clients are disconnected.
- EOF, framing errors, malformed client requests, and read/write timeouts affect only that connection. Listener creation/binding failures remain service errors.
- Keep stdio's existing multi-request stream semantics. Keep manually started `ipc` compatibility separate; do not pretend its old unbounded Unix stream handler meets the managed scheduler contract.
- Bound CLI health/shutdown request I/O too. Timeout means unreachable or busy, not proof of process death.

Task 5a implements these connection rules for both managed platform loops; Task 5b's observed native CI verifies their regressions on Windows/Linux/macOS (see implementation-plan evidence). Frames use 8 KiB block reads with a capped accumulation buffer, not one syscall per byte. Service-management requests share a 5-second connect/write/read deadline, a 64 KiB outgoing request cap and a separate 1 MiB incoming response cap. Startup readiness shares a 2-second deadline and must match the owned child PID, index, endpoint and interval while instance ownership is held; it publishes the child's immutable startup timestamp and generation identifier. Health and targeted shutdown compare that persisted identity, including `instance_id`, so PID reuse alone cannot match a previous target. The identifier is correlation, not authentication. Windows keeps the next unconnected pipe alive during handling/scanning; Unix retains its listener. Transient accept/early-disconnect errors are retried without dropping the listener. Future timer integration must preserve the same pending transport state.

The scan itself remains synchronous and serial. Wrapping it in `tokio::time::timeout` does not enforce a deadline on non-yielding work; see [Tokio timeout](https://docs.rs/tokio/latest/tokio/time/fn.timeout.html). Frame limits bound transport buffering, not all search response allocations.

## Refresh Operation And Memory Budget

Share one private scan/compare operation with manual refresh and read-only index status, preserving their existing summary fields and root error messages. It borrows the actual read-only snapshot (including a writer's immutable dereference), returning the resolved root, candidate records and summary without cloning the saved records. Mutation still requires the held writer guard.

- Open `FileIndexStore`, resolve scope, and scan exactly once.
- Compare saved and candidate metadata (`size_bytes` and second-resolution modification time); same-size edits within that timestamp resolution may be missed. Do not claim content-change detection. `iter_files` yields borrowed normalized-path order and `RefreshSummary::compare_ordered` performs one merge with constant extra space. Inputs must be nondecreasing by normalized path; adjacent duplicate paths retain their final record, matching the legacy map comparison.
- On an automatic attempt with zero added/updated/removed counts, do not save or touch index modification time.
- On change, apply `replace_all` and the failure-safe `save()` contract while preserving root and policy.
- `index_status` never calls mutation or save, and no polling call precedes an automatic scan.

Manual refresh keeps explicit publication even when file metadata is unchanged. A shared publication helper's conditional mode is tested with scan/write spies before scheduler integration: one scan and one save on change, one scan and no save on unchanged metadata. This does not enable an automatic timer or introduce a new RPC method.

The refresh/status path now keeps the parsed `BTreeMap` and candidate `Vec`, with one longest-line read buffer during loading. Borrowed ordered comparison removes the cloned `all_files()` snapshot and two auxiliary path maps; stream loading removes the full input text buffer, and stream save avoids a whole-file formatting buffer. Legacy `all_files()` and `RefreshSummary::compare` APIs remain available for compatibility but are not used by these production comparison paths. Search result allocation and broader performance optimization are separate from this slice.

Before claiming memory-conscious scheduled refresh, introduce borrowed ordered iteration, compare sorted entries without additional path maps/cloned snapshots, and stream load/save rather than buffering complete text files. Stream loading reuses one line buffer while building the saved metadata map; its maximum record buffer, the parsed map and the candidate vector still contribute to memory. Retain the old snapshot and candidate metadata required for correctness: scan peak remains O(N), while unchanged idle state has no loaded scan snapshot or watcher. Do not retain capacities of full scan buffers after returning to idle without measuring retained memory. These structural reductions are not a measured process-memory budget or Everything-scale performance acceptance.

## Failure And Observability

- Automatic open/scan/save errors preserve the last published index and leave the service able to handle the next request.
- Retry after a complete interval from failure completion; do not retry in a tight loop.
- Maintain only a small in-memory last-attempt record: outcome (`never`/`unchanged`/`updated`/`failed`), last attempt and last success Unix seconds, summary when available, and a sanitized error capped at 512 bytes on a UTF-8 boundary.
- Expose it through a planned additive read-only `refresh_status` RPC, and advertise the method only when implemented. When disabled, return `enabled: false` without scanning; when configured, also return the period and last-attempt record. Reset records on restart and make the absence of history explicit.
- Do not rewrite the service-state file on each attempt or add unbounded logs/history.
- Detached service stderr is currently discarded. A stderr message alone is not an observable failure signal.
- `index_status` is a full scan and cannot report the last refresh error or prove that the timer ran. Clients must not use it as a cheap health heartbeat.

## Lifecycle And Local Security

The revised [background service design](2026-06-24-background-service-mvp-design.md) defines instance ownership and endpoint cleanup.

Before enabling unattended refresh, bound health I/O and prevent stale-state recovery from spawning over a live/busy owner or unlinking its endpoint. Use a private user endpoint namespace and first-instance protection. Local IPC is not authentication; broader AI-tool access requires an explicit read-only authorization profile and OS peer/ACL restrictions before production use.

Task 5a uses separate persistent state-adjacent startup and child-lifetime locks. A busy or unverifiable owner is `starting`/`unresponsive`, not permission to spawn or clean up. Invalid state is `error`. Stop verifies structured identity, optionally targets shutdown to that identity, and waits for lifetime/index release before removing matching advisory state. Atomic state writes are capped at 64 KiB including newline, use unique exclusive temporaries, and retain the owned startup `Child` for failure cleanup; no saved PID is kill authority.

Task 5b applies a protected current-process-user DACL during every Windows pipe instance's creation, retains first-instance/remote-client protection, and maps the default label to a user-SID name. Existing-instance reuse verifies the actual descriptor; an older/insecure descriptor requires stop/start, without rewriting a live ACL or disabling stop for the older owned instance. Complete local pipe prefixes are recognized case-insensitively. A small native boundary crate contains documented Windows FFI; existing application crates retain `unsafe_code = forbid`. Unix validates private owned runtime directories, requires absolute custom endpoints under private parents, and retains an endpoint-specific persistent lock. Its bounded record ties recovery to socket device/inode and canonical state owner: only an unchanged recorded socket belonging to that state with definitive connection refusal can be removed. A gracefully removed endpoint can be reused by another state without replacing the permanent lock. Timeout, foreign/unrecorded sockets, ordinary files, links and replaced identities are never cleanup authority. Managed scans exclude the endpoint lock as another explicit runtime artifact. These controls separate ordinary OS users, not arbitrary same-user code or privileged administrators. Native platform tests and broader AI authorization remain separate gates; do not infer acceptance from a configured CI job.

## Acceptance Gates

Unit tests: parser boundaries, legacy state, policy round trips/unknown versions, ordered comparison parity, no-change no-write, and bounded diagnostic state.

Storage/subprocess tests: all supported writers contend on one lock; process exit releases it; pre-existing temp link/collision is untouched; injected write/flush/sync/replace failures preserve old bytes; self-artifacts and excluded directories stay excluded.

Scheduler tests with paused time and an injected refresh hook: delayed first run, disabled mode, completion-relative delay after slow success/failure, suspend/resume, manual success reset, read traffic fairness, and at most one operation active. Do not rely on a `Skip` constructor test or 30-second sleeps.

Real managed transport tests on Windows and Unix: idle client, slow trickle, oversized line, open connection after response, broken writer, concurrent request/timer, and shutdown after scan. Test the actual platform loop, not only an in-memory stream handler.

Functional process tests: start, mutate added/updated/deleted fixtures, observe one scheduled publication, retain root/policy, inspect `refresh_status`, force one failure then recovery, stop, and confirm child exit plus endpoint/state cleanup in `finally`.

Performance gate in release mode: 10k/100k metadata fixtures (1M as a separate scale gate), shallow/deep trees, long paths, exclusions, and same hardware/cold-warm conditions. Record idle/peak process memory, retained memory after repeated scans, scan/compare/save time, unchanged write count, idle CPU, and RPC P50/P95/max both idle and during scans. Report full-scan blocking honestly. No claim of Everything-scale speed or a universal memory limit until measured budgets exist.

Before implementation completion run `cargo fmt --check`, focused tests, one `cargo test --workspace`, and `cargo clippy --workspace --all-targets -- -D warnings`. Native Windows, Linux, and macOS test/transport gates are required before claiming cross-platform completion; a Windows-only result must be labeled as such.
