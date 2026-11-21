//! Conformance scenarios and implementation constraints.
//!
//! This inventory was prepared before the storage contract and now accompanies the
//! executable scenarios in suite.rs. DomainStore and its DTOs remain the only public
//! storage contract; this module documents coverage and reference-backend constraints.
//!
//! # Execution model
//!
//! Run the same scenarios against an Arc<dyn DomainStore>, supplied by a Harness.
//! The harness owns an isolated namespace, supplies a controllable clock, and
//! describes reopen, real process-crash, and in-flight interruption capabilities.
//! A scenario requiring an unavailable capability produces Skipped(reason), never
//! Passed. Query/commit failures on a supported capability produce Failed.
//! Open fresh state per scenario so ordering and previously advanced clocks cannot
//! change results. Reopen preserves that scenario's state; import uses a distinct,
//! empty destination. Always close leases/export sessions and join worker threads.
//!
//! Use standard-library threads, barriers/channels, and a waking executor built
//! with std::task::Wake, park, and unpark. Do not assume every store future is
//! immediately ready. Do not add a runtime dependency, busy-poll, or use sleeps.
//! Catch scenario panics at the report boundary, and report worker panics as
//! failures. Ensure a failing writer releases readers rather than stranding them
//! on a barrier. Thread schedules need not be reproducible: generated operations
//! and asserted invariants must be. Use a documented fixed seed and include it in
//! the report; never assert a particular winner of a concurrent race.
//!
//! # Stable report
//!
//! Keep an explicit suite version and deterministic scenario ordering. Each item
//! has a stable name, status (passed/failed/skipped), elapsed duration, and optional
//! error/reason. Include implementation identity, seed, and optional artifact
//! digest at report level. Durations vary; field names, units, status spellings,
//! and scenario names must not. A skip requires a nonempty reason. Expose passed,
//! failed, and skipped totals separately: no failures does not mean all required
//! capabilities passed. Preserve performance measurements without gating them.
//!
//! # Scenario inventory
//!
//! References below identify the transaction-check table rows in DESIGN section
//! 8.3 (source lines 562-573 at the drafting baseline). Additional checks extend
//! those rows under storage-contract sections 1 and 3.5; they are not claimed to
//! be separate rows of the original table.
//!
//! - atomicity [562]: insert a sentinel, append records and persist a fact, then
//!   violate a unique index in a later mutation of the same work unit. Verify no
//!   sentinel, appended record, fact or cursor effect survives. Retrying a fixed
//!   unit after removing the cause must not replay a failed receipt. Verify both
//!   a primary-key collision and a secondary unique-index collision. Include
//!   commit_many with success/failure/success to prove per-unit atomicity.
//! - lost_update [563]: 16 clients each complete 1,000 successful read/CAS
//!   increments on one row. Retry only Conflict, with a bounded failure escape;
//!   other errors fail immediately. Assert final value 16,000, successful commits
//!   16,000, and strictly advancing committed revisions in serialized observation.
//!   Add a forced stale-revision attempt so detecting Conflict is not contingent
//!   on the scheduler. Recompute digest after rebuilding a conflicted request.
//! - write_skew [564]: two clients read the same empty state and synchronize before
//!   submitting units that both require A and B absent but insert different rows.
//!   Exactly one wins and the loser reports Conflict. Run on fresh rows repeatedly
//!   to expose an implementation checking conditions outside its write lock.
//! - transfer_snapshot [565]: fixed-seed transfers update both account rows using
//!   both read revisions as preconditions. A concurrent reader continuously takes
//!   a single-domain batch snapshot of all accounts. Every observed sum equals
//!   the initial total; no balance becomes negative. Coordinate a reader sample
//!   during active writers and assert the sample count is nonzero. Retry conflicts
//!   by rereading both accounts, without counting aborted transfers as successes.
//! - durability [566]: commit rows, a fact, an append, and cursor; release handles,
//!   reopen through the harness, and verify values, revisions, sequence positions,
//!   and replay result. MemStore and its loopback skip: no persistent medium.
//! - crash_atomicity [567]: a harness-owned child begins a large multi-mutation
//!   commit; terminate the actual writer process during commit, then reopen.
//!   Repeat 50 times and accept only all-or-none rows/facts/cursor/append effects.
//!   Require evidence that kill occurred while commit was in flight; a clean
//!   close/reopen or a kill before dispatch is not crash coverage. The harness
//!   owns process-specific readiness/fault hooks; no core dependency on SQLite
//!   or a private service. MemStore and its loopback skip: no crashable process.
//! - idempotency_recorded [568]: replay one work unit three times and compare rev
//!   and every returned append sequence with the first result; replayed alone
//!   changes. Verify rows/facts/stream length reflect only one application.
//! - idempotency_mismatch [568 extension]: same key with changed content and its
//!   correctly recomputed digest returns IdempotencyMismatch with no side effects.
//!   Also reject a forged digest rather than trusting the caller's digest bytes.
//! - idempotency_expiry [568 extension]: controllable time checks the 30-day
//!   boundary and just beyond it. An expired original or unseen key returns
//!   Expired even if its receipt was retained/removed; it must never reexecute.
//!   Test future-issued keys at and beyond the five-minute allowance. Verify
//!   receipt identity includes domain and issued_at according to the final DTO.
//! - idempotency_natural [568 extension]: exercise Natural { entity, index } on
//!   the contract's declared natural unique key. Repeated delivery leaves one
//!   logical result, preserves append/fact effects, and handles conflicting
//!   content according to the published Natural contract. Do not silently model
//!   Natural by ignoring its entity/index or treating every unique error as replay.
//! - idempotency_after_crash [568]: replay a known successful unit after a real
//!   crash/reopen, retaining its original result. Also replay an ambiguous unit
//!   from crash_atomicity and verify exactly one final application. MemStore and
//!   its loopback skip: no real crash/reopen capability.
//! - append_order [569]: concurrent fixed-seed appends with unique record IDs and
//!   varying batch sizes, while a reader pages using seq > last_seen. Assert
//!   strictly increasing seq, no duplicate/missing IDs after final drain, batch
//!   result sequences matching stored rows, and commit-before-dispatch ordering
//!   for explicitly coordinated commits. Never substitute thread return order
//!   for commit order. Exercise two streams to check sequence scope.
//! - snapshot_isolation [570]: begin a lease and read a row, commit a new value
//!   from another client, and read again under that lease. Both lease reads see
//!   the old value/revision; a fresh query sees the new value. Batch snapshots
//!   are separately exercised by transfer_snapshot. Ending a lease invalidates
//!   it and ending it again follows the published error/idempotency contract.
//! - unique_and_c_order [571]: reject duplicate primary/secondary unique keys,
//!   preserve distinct case-sensitive keys, and scan a deliberately unsorted
//!   set of ASCII, accented, CJK, and emoji keys in UTF-8 byte order. Include
//!   equal secondary keys with primary-key tiebreakers, descending traversal,
//!   composite-key ranges, and numeric keys 2 and 10. SQL NULL uniqueness follows
//!   the declared contract, not JSON-text ordering or locale collation.
//! - encoding_precision [572]: round-trip CJK, emoji, non-NUL control characters,
//!   a 1 MiB string, millisecond timestamps, integer extrema, byte arrays, booleans,
//!   nulls, finite reals, and RawJson with significant whitespace/key ordering.
//!   Compare bytes and types, not just parsed JSON equivalence. Reject NaN and
//!   infinities on the direct path as well as the loopback path; never let JSON
//!   conversion silently turn them into null. Reject malformed rows and unknown
//!   enum values with the contract error; unknown operation/query is unsupported.
//! - performance_baseline [573]: measure repeated point reads and 128-row commits
//!   with std::time::Instant; include sample count, p50/p99 with explicit units,
//!   and advisory warnings. Use deterministic unique keys. Timing is not a
//!   correctness threshold and uses real monotonic time, not the fake wall clock.
//! - lease_expiry [570 extension]: advance the harness clock across the hard
//!   two-second deadline without sleeping; reads at/after expiry return stale.
//!   Reject mixed-domain lease queries and forged IDs. Expired snapshots must
//!   release resources; verify subsequent snapshots still open successfully.
//! - lease_interruption [570 extension]: with a harness-supported controllably
//!   blocked read, expire the lease while the query is in flight. Verify stale
//!   and safe resource reuse after cancellation. MemStore skips this subcase if
//!   no interruptible in-flight statement exists, independently of lease_expiry.
//! - snapshot_domain [570 extension]: reject a mixed-domain batch both when the
//!   query envelope explicitly disagrees and when its named entity belongs to a
//!   different domain. Include an empty batch and same-domain successful control.
//! - uow_domain [562 extension]: reject another domain's entity in each of Rev,
//!   Present, Absent, Insert, Upsert, Update, Delete, DeleteWhere, append stream,
//!   named operation, and fact topic. Put valid effects before the invalid one
//!   and prove none survives. Test all three domain directions in fresh state.
//! - cursor_monotonic [569 extension]: initial CursorGet returns None, a cursor
//!   precondition with at=None succeeds once, and a repeat fails as Conflict.
//!   Advance atomically with a row side effect and verify composite/text/integer
//!   ordering, rejection of backward advances, and contract treatment of equality.
//!   Cursor identity is domain-scoped. A failed cursor precondition or backward
//!   mutation must roll back all other mutations in that work unit.
//! - scan_window [571 extension]: sparse matches over a small examined-row
//!   budget yield an exhausted page with a resume token even for zero matches.
//!   Continue to exhaustion without duplicates or missing eligible rows; token
//!   tracks the last examined composite index key, including the primary-key
//!   tiebreaker. Independently exercise result limits, exactly-full windows,
//!   ascending/descending scans, index ranges, and token/query compatibility.
//!   Invalid budgets/limits fail rather than allowing unbounded scans. Count
//!   returns AtLeast when inspection stops early, Exact only at range exhaustion.
//! - transfer_roundtrip [566/572 extension]: export all three domains, paginate
//!   in deterministic entity/key order, retry a cursor and compare batch bytes
//!   and sha256. Concurrent writes cannot alter the export's pinned snapshot.
//!   Import batches into fresh state, replay a batch without duplicating effects,
//!   reject altered content/digest, and verify rows, rev, append positions,
//!   cursors, and replay safety as represented by the final transfer contract.
//!   Reject unknown sessions/cursors and cross-domain batches. Do not claim
//!   export for a domain whose complete entity set is not registered.
//! - retention_report [562 extension]: advance the clock over a declared policy's
//!   age cutoff; retain live rows and delete eligible rows in bounded slices.
//!   Check entity, affected, slices, max_slice_ms, overruns, and more, including
//!   empty input, a partial slice, and repeated calls to completion. Assert counts
//!   and units rather than hardware-specific timing. Protect/before/action hooks
//!   cannot silently succeed without executing; unsupported named hooks fail
//!   explicitly. A row whose before hook fails must remain for a later retry.
//! - named_registry [562/570 extension]: enumerate every named operation and query
//!   in the public registry and require an explicitly registered semantic test
//!   for each name, domain and kind. A contract entry, its implementation and
//!   semantic test must land together in the same task. A placeholder generic
//!   get/commit test is not semantic coverage. Run tests through both stores.
//!
//! # Test-only entity catalogue
//!
//! Declare every entity using entity_desc!, behind conformance, with verify_
//! names. At least one entity belongs to each domain. Candidate shapes: mutable
//! accounts/counters with revisions; a mutable record with a unique secondary
//! text index and encoding fields; a stream with a grouping key and allocated
//! sequence; a comms record with a natural unique key; and an age-retained log.
//! Include multiple records with identical secondary sort keys to expose broken
//! resume tokens. Use only contract-declared fields/indexes. Do not invent an
//! entity registration method until the skeleton defines the backend seam.
//!
//! # Memory reference implementation
//!
//! One Mutex serializes writes and precondition checks; a transaction works on a
//! candidate clone and replaces live state only after every validation succeeds.
//! Store entity descriptions, rows, row revisions, domain-scoped receipts, facts,
//! stream counters, and cursors in that transactional state. A failed transaction
//! changes none of them. Allocate revisions consistently with the contract and
//! never issue one revision for two different successful units. commit_many has
//! per-unit results, preserving earlier successes when a later unit fails.
//!
//! Validate rows against declared types, nullability, keys, finite reals, enum
//! membership, and unique indexes after each mutation as the contract requires.
//! Validate every referenced entity/stream/topic/op against the work-unit domain.
//! Compute request digests using the public canonicalization helper, not a second
//! implementation. Check expiry before receipt lookup. Replay the original full
//! result; it must not allocate another revision or append sequence. Interpret
//! Natural according to its public contract, including content conflicts.
//!
//! Implement typed cell/index comparison, SQL-compatible null treatment, C text
//! order, and deterministic primary-key tiebreakers. Never sort numeric keys by
//! their serialized JSON bytes. Filter only inside the examined scan window.
//! Resume after the last examined key, not the last returned match. Leases and
//! export sessions own immutable cloned snapshots; their IDs and deadlines are
//! checked against the controllable clock and their domain on every access.
//! Export/import checksums use the public canonical transfer encoding. Imports
//! validate before changing state and remember batch identities for safe retry.
//! Retention counts examined work and affected rows separately, and preserves
//! policy hooks or explicitly rejects unsupported ones. Do not advertise missing
//! named behavior in Hello. This fake is a reference, not a relaxed CRUD map.
//!
//! # Loopback boundary
//!
//! Wrap every DomainStore method, including hello metadata, query, snapshot,
//! begin_snapshot, lease_query, lease_end, commit, commit_many, begin_export,
//! export, import, and retention. Convert Payload::Local using to_json before
//! serializing the owning DTO. Round-trip inputs and outputs (success and error)
//! with serde_json without losing RawJson bytes. A cached, round-tripped Hello
//! satisfies the borrowed hello signature. Serialization errors become explicit
//! contract errors; do not panic or turn an error into an empty success. Preserve
//! commit_many result cardinality and each unit's error independently. The
//! adapter must never call the inner store with a Local payload after conversion.
//! A local-payload probe must demonstrate that the wire path actually occurred.
//!
//! # Integration and acceptance
//!
//! tests/conformance_mem.rs runs the full suite against MemStore and separately
//! Loopback<MemStore>, asserting identical scenario inventories and no failures.
//! Assert skip names/reasons explicitly so adding a broad skip cannot hide a
//! regression. Test-only assertion failures/panics must become report failures.
//! A registry-coverage negative control adds a name missing its semantic check and
//! proves that coverage fails. Keep conformance code out of default builds.
//! Reuse only the dependencies declared by the contract crate. Do not edit its
//! manifest, root modules, or lockfile from this implementation task.
