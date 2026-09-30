# Service Auto Refresh Implementation Plan

Reviewed: 2026-09-30. Implement in the existing main checkout. This revision replaces the original Task 2/3 sequence because unattended writes require scope, storage, and connection safeguards first.

## Status And Rules

- [x] Task 1: interval configuration only, committed as fd2f6a9.
- [x] Task 2: persist and enforce scan scope.
- [x] Task 3: single-writer ownership and safe snapshot publication (Windows verification; native Linux/macOS gates remain open).
- [x] Task 4: shared scan/compare and controlled allocations (Windows structural/functional verification; measured memory budgets remain Task 7).
- [ ] Task 5: bounded managed IPC and safe lifecycle.
- [ ] Task 6: fixed-delay scheduler and last-attempt status.
- [ ] Task 7: platform, performance, and documentation acceptance.

The current `service_run` uses `auto_refresh_seconds` only to validate root/policy prerequisites, not to schedule scans. Tasks 1-4 are implemented; Tasks 5-7 remain planned, with state-path forwarding already delivered in Task 4. The Task 3 native Linux/macOS acceptance item below remains unverified and is required before claiming cross-platform completion. Do not enable or advertise automatic refresh before their safety gates pass.

**Stack:** Rust edition 2024, MSRV 1.96, existing Tokio IPC/time support, serde/serde_json, and repository temporary-directory test helpers. There is no existing `tempfile` dependency; reuse local helpers rather than adding one implicitly.

**Reference:** [Reviewed design](../specs/2026-07-10-service-auto-refresh-design.md).

Work on `main` without a new branch/worktree. For each slice, add focused failing tests, implement, run focused tests, then run one workspace suite before committing. Inspect the complete diff and stage only intended files. Push each verified commit with ordinary `git push origin main`. Do not force-push or include unrelated user changes.

Do not require a skill through this document. Follow the current user's skill permissions. Do not add duplicate plans for these follow-up slices.

## Task 1: Configuration Slice Already Implemented

Files: `crates/daemon/src/lib.rs`, `main.rs`, `service.rs`, `tests/service_cli_tests.rs`, `tests/service_state_tests.rs`.

- [x] Parse one optional `--auto-refresh-seconds` and `--endpoint` in either order.
- [x] Accept `30..=86400`; reject missing/duplicate/invalid values with exit code `2`.
- [x] Forward the optional value to hidden `service-run`.
- [x] Store `ServiceState.auto_refresh_seconds: Option<u64>` with `#[serde(default)]`.
- [x] Conditionally render JSON interval and text `auto refresh: <seconds>s`.
- [x] Cover legacy state and configuration parsing, plus service lifecycle tests.
- [x] Commit and push fd2f6a9.

Existing APIs to reuse: `parse_auto_refresh_seconds(&str) -> Option<u64>`, `read_state`, `write_state`, `render_status_text`, and `render_status_json`. Do not copy the previous plan's nonexistent `write_service_state` or alternate parser signature.

These checks describe the committed configuration scope, not fresh scheduler acceptance or proof that the timer runs.

## Task 2: Persist And Enforce Scan Scope

Files: `crates/indexer/src/store.rs` and store tests; `crates/cli/src/lib.rs` and CLI tests; `crates/daemon/src/lib.rs` and handler/service tests.

- [x] Add failing policy serialization tests: sorted/deduplicated exclusion names, escaped values, known empty policy, absent legacy policy, malformed/unsupported policy version, and preservation across save/open.
- [x] Add failing tests proving automatic-start validation rejects unknown policy and ambiguous relative roots before spawning. An excluded directory stays excluded through refresh and index status.
- [x] Add policy metadata to `FileIndexStore` using existing `meta` records and escaping. Preserve `aifs-index-v1` reads; no marker is `None`/unknown rather than an empty set.
- [x] Explicit CLI `index` records canonical absolute root and the requested policy. Unknown legacy policy requires explicit rebuild; ordinary `refresh` does not guess it.
- [x] For a known policy, omitted exclusions in CLI/daemon refresh and index status inherit stored scope; matching explicit sets are allowed and mismatches fail before scanning. Changing policy requires an explicit `index` rebuild.
- [x] Preserve legacy manual behavior and existing daemon root mismatch errors. The upgraded writer preserves policy metadata.
- [x] Test metadata-only additions, empty exclusion sets, root alias equality, index-inside-root exclusions, and older file reads. Document old-writer incompatibility.
- [x] Run focused tests and full workspace verification: 123 Windows tests pass (46 added), `cargo fmt --check` and workspace Clippy pass. Deliver in `feat: persist index scan policy`.

No timer is connected in this task. Startup policy/root checks now run for configured auto refresh in both parent and hidden child; no-auto legacy lifecycle behavior is preserved. Native Linux/macOS and performance gates remain unverified.

Shared indexer APIs: `FileIndexStore::new/open` now create read-only snapshots; Task 3 moved mutation/rebuild to guarded `FileIndexWriter::new/open`. `scan_policy`/`set_scan_policy` distinguish unknown and known-empty scope; `resolve_scan_options(Option<ScanOptions>)` enforces inheritance/matching; `ScanOptions::excluded_names` exposes sorted, deduplicated names. No dependency was added.

## Task 3: Single Writer And Safe Publication

Files: `crates/indexer/src/store.rs`, `writer_lock.rs`, `scanner.rs`, storage/lock tests, and all CLI/daemon write call sites and subprocess tests.

- [x] Add real subprocess contention tests for external guard owner/CLI, daemon manual write/service, and second-service ownership of the same canonical index. Include release after normal exit and forced test-child termination. An additional Windows smoke uses the actual CLI and service-run binaries together.
- [x] Implement an indexer-owned RAII guard using a stable adjacent lock file and `std::fs::File::try_lock`. Hold it over old-index open, scan, comparison, and publication; managed and manual IPC children hold it for their lifetime.
- [x] Make mutation/publication require a mutable guard borrow through `FileIndexWriter`. Preserve existing read-only APIs; update every supported write entry point and its tests. Three compile-fail doctests enforce reader/write-capability separation.
- [x] Resolve relative/absolute and supported filesystem aliases to one lock identity. State limitations for hard-link aliases, mixed old writers, untrusted directories, and network filesystems. Never unlink the lock file on release or truncate an existing lock path. Windows case/parent aliases are tested; Unix symlink cases are conditional and unrun here.
- [x] Add injected failure tests for temporary creation, write, flush, sync, and replacement. Assert old index bytes remain unchanged for every pre-publication failure.
- [x] Test pre-existing temp-name collisions and links without requiring Windows symlink privileges: hard-link/file/directory collision tests pass natively; conditional symlink coverage is added but unrun here. Unrelated files remain untouched.
- [x] Replace fixed `<index>.tmp` reuse with unique same-directory exclusive creation and 32 bounded collision retries. Remove only files created by the current attempt. No unlink-old-index fallback.
- [x] Stream borrowed records through `BufWriter`, explicitly flush/sync, request Unix mode `0600` (Windows inherits trusted-directory ACL), close temporary handles before rename/cleanup, and replace on supported local filesystems. No fallible step follows successful replacement, and no power-loss durability guarantee is made.
- [ ] Verify new and existing index publication on native Windows, Linux, and macOS, including read-only concurrent clients and rename/share-mode failure.
- [x] Run focused storage/subprocess tests and one workspace suite: 162 Windows tests pass (39 added, including 3 compile-fail doctests). Three helper cases are ignored by the top-level runner and invoked as owned test children. Workspace fmt, Clippy and diff checks pass. Deliver as `fix: isolate index writers and publish snapshots safely`.

Implemented with three disjoint code workers (storage, CLI, daemon), plus main-thread shared APIs, review and integration. The Windows smoke confirms service-held ownership rejects real CLI index/refresh, query/stats/status remain readable, service RPC refresh publishes added files, and normal child exit permits CLI refresh. Process audit found no remaining test/service child. Startup readiness failure and state-write failure now reap only the newly spawned `Child`; endpoint/state identity, unbounded health I/O and readiness provenance remain Task 5 gaps.

Shared API: `IndexWriterGuard::acquire(&Path)`, `index_path()`, `lock_path()`, `FileIndexWriter::new/open(&mut guard)`, and `Scanner::scan_for_index(root, index_path)`. Guard/writer are not cloneable; only immutable snapshots may be cloned. Index, adjacent lock and reserved publication prefix are centrally excluded; ordinary `.tmp` and similarly named files elsewhere remain visible.

Keep directory and endpoint access control distinct from cooperative locks. A lock is not protection against arbitrary same-user code or external processes ignoring the contract.

## Task 4: Share Scan/Compare And Control Memory Copies

Files: `crates/daemon/src/lib.rs` and shared-refresh tests; `crates/indexer/src/store.rs`, `comparison.rs`, `scanner.rs` and loading/comparison/artifact tests; CLI comparison call sites and tests.

- [x] Add failing internal tests for unchanged, added, updated, and removed files; exact no-write behavior; root/policy preservation; self-artifacts; missing root; and save failure.
- [x] Build private `scan_and_compare`, borrowing the actual `FileIndexStore` (or writer's immutable dereference) and returning resolved root, candidate `Vec<IndexedFile>`, and `RefreshSummary`, with explicit runtime artifact paths and held writer ownership for mutations. Borrowing keeps the existing writer capability intact and avoids copying/rewrapping the saved snapshot.
- [x] Use existing `replace_all(files)`, `set_root_path(...)`, and `save()` APIs through guarded `FileIndexWriter` and private `publish_comparison`. No alternate store/save API is introduced.
- [x] Define changed as `added != 0 || updated != 0 || removed != 0`. `RefreshSummary::has_changes` is shared by status and conditional publication.
- [x] Prepare conditional publication for future automatic operations: one shared scan, save only on change. The production helper is tested with `save_only_if_changed = true`; no scheduler invokes it yet. Manual `refresh`/`reindex` use false and keep their summary fields/current explicit-save behavior; `index_status` scans once and never mutates.
- [x] Add borrowed ordered iteration over saved records and compare sorted candidate entries without `all_files()` clones or additional path maps. Adjacent duplicates keep the last record, preserving the old map behavior. Five comparison tests cover 4096 metadata-state pairs, empty sets, metadata changes, normalized/Unicode paths, duplicate runs and single-pass iterator consumption.
- [x] Stream index loading rather than keeping complete text alongside parsed records. Reuse one line buffer, distinguish NotFound from actual open/read errors, preserve str::lines/legacy/policy behavior, and reject partial invalid-UTF-8 snapshots. Scoped tests include 20000 records, long records and tiny split read buffers. No parsed map or scan vector is cached at service idle.
- [x] Exclude exact index, lock, resolved managed state, and index-publication namespace, including relative/absolute paths. `Scanner::scan_for_index_with_artifacts` accepts explicit runtime paths. A child-specific absolute `AIFS_SERVICE_STATE` override is built from the actual startup path; no process-global environment mutation or generic state-name/.tmp suffix filter is used.
- [x] Prove one scan/one write for changed results and one scan/zero writes for unchanged conditional results with spies. Prove unchanged manual refresh still attempts replacement via a Windows share-mode gate. Read-only status has no publication path; real replacement failure preserves published bytes.
- [x] Run focused tests and one workspace suite: 204 Windows tests pass, 42 added (17 loading/iteration, 9 comparison/artifact, 2 CLI, 14 daemon). Three top-level ignored subprocess helpers are exercised by parent tests. Workspace fmt, Clippy and diff checks pass. Deliver as `refactor: share bounded-allocation index refresh`.

Memory remains O(N) while scanning. Existing modification timestamps have second resolution; tests should change size or controlled metadata rather than relying on tiny wall-clock sleeps.

Three workers delivered storage loading, CLI call sites and daemon shared operations; the main thread delivered ordered comparison, runtime identity resolution, integration review and docs. Real child-process tests cover managed absolute/relative custom state inside the scanned root, status/refresh/reindex/query scope preservation, and keeping similarly named user files visible. Command construction separately proves an explicit custom startup state overrides inherited environment and is absolute. Process audit found no remaining test/service child.

Loading's extra buffer is O(read buffer + longest record), and ordered comparison's extra storage is O(1); this is source/test evidence, not a measured process-memory reduction. Parsed saved metadata and candidates remain O(N). The 20000-record loading case is a functional test, not large-scale latency or RSS acceptance. Only x86_64-pc-windows-msvc is installed/verified here; native Linux/macOS and Task 7 performance gates remain open. Task 5 endpoint/state ownership, bounded IPC/client health I/O and Task 6 timer/status are unchanged and unimplemented.

## Task 5: Bounded Managed IPC And Safe Lifecycle

Files: `crates/daemon/src/lib.rs`, `service.rs`, `tests/transport_tests.rs`, `tests/service_cli_tests.rs`; add narrowly scoped private helpers when necessary.

- [ ] Test the actual managed Windows Named Pipe/Unix Socket loop with an idle connection, slow trickle, oversized/no-newline frame, EOF, disconnected response reader, and a client keeping a connection open after one response.
- [ ] Use one request/response per managed connection, a 64-KiB capped incoming frame, total 5-second read deadline, and 5-second response-write deadline. Connection-local failure must not terminate the server.
- [ ] Keep `handle_json_stream` for stdio's multi-request use and preserve manually started `ipc` compatibility separately. Test no-auto managed mode through the new bounded path too.
- [ ] Preserve a pending listener/Named Pipe server across timer waits. Do not drop/recreate an unconnected pipe on every timer event; it should remain available while the serial owner is busy.
- [x] Delivered in Task 4: pass the absolute resolved state path to the child runtime context, respecting `AIFS_SERVICE_STATE`, and avoid process-global environment mutation in parallel tests. Task 5 still owns the state/endpoint lifecycle safety work.
- [ ] Bound client-side health/shutdown request I/O. A timeout or malformed state means unknown/busy/error, not permission to spawn over a live child or delete its endpoint.
- [ ] Add managed-instance ownership covering the state/endpoint as well as the per-index guard. Check requested index/endpoint/config when reporting already running; do not silently say a different requested index was started.
- [ ] Resolve a user-private absolute Unix endpoint under the runtime directory; enforce private directory/socket permissions. Use Windows first-pipe-instance protection and a current-user access policy before treating the service as a production boundary.
- [ ] Never unconditionally delete a pre-existing endpoint path. With verified instance ownership, inspect socket type and liveness before stale cleanup; reject a regular file or foreign live endpoint. Remove only owned artifacts at shutdown.
- [ ] Keep the startup `Child` handle. On readiness or state-write failure, stop and reap that owned child, and clean only owned state/endpoint files. A saved PID alone is not authority to kill a process.
- [ ] Atomically write advisory state. Missing state is stopped only when ownership is absent; active startup is starting, and an active child without usable state is unresponsive. Malformed/unreadable state is an explicit error, not silently stopped. Use the background design's exact JSON status names; preserve existing healthy/stopped output.
- [ ] Test simultaneous start, busy scan during start/status/stop, stale endpoint file, malformed state, timeout cleanup, index contention, custom endpoint, and failed state write.
- [ ] Run focused native transport/lifecycle tests and workspace tests; commit/push `fix: bound managed IPC and protect service ownership`.

Do not wrap a synchronous full scan in an async timeout and claim it is canceled. Scan latency still bounds responsiveness. ACL/peer authorization for arbitrary AI clients remains a separate production gate; local IPC alone provides no such guarantee.

## Task 6: Fixed-Delay Scheduler And Refresh Status

Files: `crates/daemon/src/lib.rs`, `crates/daemon/Cargo.toml` for Tokio test support only if needed, scheduler/handler/transport tests.

- [ ] Introduce a private deadline state: optional period plus optional next deadline. No configured period means no timer and no automatic scans.
- [ ] Add deterministic paused-time tests BEFORE wiring production: first run after readiness plus one period, slow success/failure then full idle interval, machine suspension, successful manual reset, failed manual/read-only no reset, due-timer fairness, and no overlap.
- [ ] Enable Tokio `test-util` in dev-only dependencies if paused-time tests require it. Reuse the existing Tokio version/features, not an unrelated test library.
- [ ] Select `sleep_until(next_due)` against a cancel-safe accept/connect operation. Keep that transport state alive when the timer wins. Use explicit due-work priority so a flood of connections cannot postpone the scan forever.
- [ ] Run one synchronous shared refresh attempt, then set the next deadline from COMPLETION, for both success and failure. Do not use `Interval`/`MissedTickBehavior::Skip` to approximate the fixed-delay requirement.
- [ ] A successful manual `refresh`/`reindex` also resets the deadline. Shutdown is processed after the active operation completes; do not add a scan worker merely to mask that limitation.
- [ ] Maintain a bounded in-memory last-attempt record and implement additive read-only `refresh_status`: enabled flag, configured period when present, never/unchanged/updated/failed outcome, last attempt/success times, summary, and sanitized error capped at 512 UTF-8 bytes.
- [ ] Add catalog and handler tests for `refresh_status`; prove it performs no root scan, content read, or disk-state write, and failure/recovery records survive between requests but reset on restart.
- [ ] Exercise actual managed platform loops with a due scan plus idle/malformed/oversized client. Verify subsequent requests and scheduled attempts still complete.
- [ ] Remove the current ignored-interval placeholder only after Tasks 2-5 pass. Existing no-flag state rendering and manual RPC result fields remain compatible.
- [ ] Run focused scheduler/handler/transport tests and workspace tests; commit/push `feat: run fixed-delay service auto refresh`.

Detached stderr is not the diagnostic channel. `index_status` is not a substitute for refresh outcome: it performs another full scan and knows nothing about the last automatic attempt.

## Task 7: Functional, Platform, And Performance Acceptance

Files: daemon functional tests, benchmark/test scripts where needed, native CI configuration, `README.md` and these documents.

- [ ] Add a functional child-process test using an injected internal period/clock seam rather than accepting a sub-30-second production flag. Cover add/update/delete, unchanged no publication, policy preservation, failure/recovery, `refresh_status`, and real child exit on shutdown.
- [ ] Keep one optional smoke with the real `--auto-refresh-seconds 30`. A status field alone is not acceptance: mutate files and observe a scheduled write. Cleanup in `finally` must stop/reap owned children and remove isolated endpoints/state.
- [ ] Establish native Windows/Linux/macOS CI for formatting, workspace tests, clippy, managed transport, lock contention, and safe replacement. No existing `.github` workflow should be assumed. Report any unrun platform as unverified.
- [ ] Run release-mode 10k/100k metadata fixtures, and a separately labeled 1M scale test. Include shallow/deep trees, long paths, exclusions, no-change scans, changed scans, and repeated refresh cycles.
- [ ] Record OS/filesystem, CPU/RAM/storage, toolchain/commit, dataset, cold/warm conditions, process idle/peak/retained memory, idle CPU, scan/compare/save timings, write counts, and RPC P50/P95/max idle versus during scans.
- [ ] Establish a measured baseline and platform budgets BEFORE claiming low-memory/high-performance acceptance. On the same pinned runner, gate unexplained peak-memory or latency regressions above 20%; investigate before adjusting a budget. Never compare unrelated hardware samples as a regression.
- [ ] Report serial scan blocking explicitly. If it exceeds the product's accepted response-latency budget, keep automatic scanning experimental and open a separate cooperative-scan design; do not hide it with a generous socket timeout.
- [ ] Update README to show auto refresh as implemented only after these gates. Document fixed delay, legacy rebuild, stored exclusions, writer contention, full-scan blocking, bounded service connections, `refresh_status`, restart configuration, and security limitations.
- [ ] Run final verification once:

```powershell
cargo fmt --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
git diff --check
git status --short
```

- [ ] Confirm expected tests actually ran with zero failures, no leaked child/endpoint/state, and no unrelated staged changes.
- [ ] Commit/push acceptance artifacts with `git push origin main`; verify local and remote branch heads match. Do not mark unrun native/performance gates complete.

## Review Exit Criteria

- [ ] Automatic refresh cannot broaden a saved known scan policy.
- [ ] All supported writers share ownership; private temporary paths cannot overwrite a pre-existing link.
- [ ] Pre-publication failures preserve the old snapshot; unchanged scans produce zero writes.
- [ ] A slow completed attempt is followed by a full configured idle interval.
- [ ] Idle/trickling clients and sustained read traffic cannot indefinitely prevent due work.
- [ ] Shutdown, busy health checks, and stale recovery do not spawn duplicate services or kill unrelated processes.
- [ ] No unbounded diagnostic history, full-snapshot formatting buffer, or idle retained scan snapshot is added.
- [ ] Native platform and measured performance evidence are labeled separately from configuration-only and design-only progress.
