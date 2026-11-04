# Reliability and extension boundaries

Steve remains a modular monolith with SQLite. The first durable lifecycle path is item completion to Automation. It does not make all existing lifecycle hooks durable.

```text
                  steve-work-api
                (DTO/topic/version)
                   ^          ^
                   |          |
                Work       Automation
                   |          |
                   v          v
                  Store / outbox
                    SQLite

Completion data flow:
  state + timeline + decision expiry + ItemDone
                       |
                  SQL commit
                       |
                  durable event
                  /           \
         Automation          Work tidy
    event/run/wait + receipt  idempotent effect
          SQL commit          then receipt
               |
        drive / boot recovery
```

`steve-work-api` contains only published DTOs and topic versions. Infrastructure does not own the Work protocol. Work emits a snapshot of the completed item. Automation writes only through its own ingest path. The shared database transaction provides atomicity without transferring ownership of domain rules.

A repeated completion of an already completed item emits no new fact. Reopening and completing again is a new transition. The default graph executor persists both newly triggered runs and event-wait resumption before acknowledging delivery. Unsupported transactional wait executors must fail explicitly on the durable path.

Consumers commit their database effects and receipt together. External effects happen outside that transaction and must tolerate repetition after a crash. A lease coordinates execution; it does not provide exactly-once external delivery. The worker starts after Flow recovery succeeds, and failed boot recovery retries. Consumer IDs and topic bindings are stable. Pausing a consumer retains its unconsumed events; it does not unsubscribe it. Dead deliveries are retained for investigation.

Remaining dependency-item advancement and unmigrated lifecycle hooks still run after completion commits. They are outside this first durable path. Git cleanup still uses the Core state lock to coordinate with item reopening; moving it out of the SQL transaction does not eliminate all lock-held subprocess work.

## Mail submission

```text
 approved -> sending -> sent
                |
                +----> unknown -> human reconciliation
                |
                +----> failed  -> retry / discard
```

A known SMTP rejection is a failure; lost confirmation, cancellation and local persistence failure after acceptance are unknown. Unknown sends are never automatically resubmitted. A workspace user must explicitly confirm that no message was sent before retrying. Stable Message-ID and intent audit records aid reconciliation; SMTP is not assumed to deduplicate them.

Confirmed submission is stored before the optional sent-folder copy. The copy has its own deadline and cannot turn a confirmed send into an unknown result. Missing unknown-result decision cards are repaired idempotently; explicitly discarded drafts are not revived. This implementation assumes one Hub owns sending for a database.

## Other outside effects

```text
 GitHub write you approved           channel send
 (open PR, merge, agent comment)     (reply, approved or by a rule)

 pending -> running -> done          pending -> sent
                    -> failed                -> failed  (refused)
                    -> unknown               -> unknown (no answer)
 restart: running -> unknown         unknown: same uuid again within the
 unknown: reconcile, then decide              provider's dedupe window, else ask
```

The connection is checked when a write goes out, not when it was approved: paused or to be authorized again, the write waits; revoked, or its capability turned off (`mail.send`, `repo.write`, `msg.send_as_user`), it is refused. A connection in `error` may write.

An approved GitHub write is a `gh_effects` row from the answer's transaction. A run that sent its write and heard nothing reconciles before writing again, without comparing clocks: the PR GitHub has from the approved commit (written down before the first send; a branch that moved is refused), a merged PR of the approved head, the one identical comment no other effect claimed. No evidence either way (a comment not found, several candidates) stops it: it stays unknown and you are told. Items finish through the outbox (`github.after`).

A channel message keeps its authorization, first send time, text and mentions (`chan_send_state`). A direct reply cut off before its send is sent by the queue under that authorization; one older than an hour is asked about instead. A transient adapter error is unknown, not failed: within the provider's dedupe window (Feishu: one hour from the first try) it is sent again as it was; after it, you are asked, once. A message tried before is never sent again past its window, whatever path brings it back. Results are fenced by the claim.

Incoming data is recorded before it counts as handled. A GitHub delivery is `received` with its source and payload until processed; a failure is retried after a back-off and kept when it fails for good; a delivery processed late does not apply a view older than the stored PR. A stored mail's event goes to the outbox with the mail (`mail.event.ready`). A poll that cannot store an event keeps its cursor; a malformed event is skipped.

Not covered yet: the reply text and its send task in one transaction (conversation redesign, stage four), Core completion hooks beyond `item.done`, a total time budget per GitHub operation, and Git subprocess timeouts.

## Event storage and compatibility

Schema 22 normalizes large tool-event strings into session-owned unique payloads in the same SQLite database. References are stored in a separate table, not interpreted from user-supplied JSON objects. Events, payloads and references commit together. Full reads hydrate transparently; searches include normalized text, and deletion removes unreferenced payloads. A single consistent database backup retains the full transcript.

New UI clients opt into bounded pages and continue by sequence number until an empty page. Older clients retain their 5000-row page contract. The bounded reader uses a 500-row / 2 MiB budget; one oversized event is returned alone to ensure progress. This is not a hard per-event memory limit. Internal full-context reads still materialize complete history.

Once schema 22 contains references, an older binary only understands the stored previews. Do not directly downgrade it. A supported older-facing reader or an independently verified reconstruction of inline data is required. Existing production history is not rewritten by the migration. New writes receive the normalization benefit.

## Extension and operational checks

New SSE event types must carry a workspace; missing or malformed private scope is not broadcast. Only explicitly retained legacy global notices may omit it. SSE lag requests client resynchronization, including lag encountered while batching frames. UI delivery remains separate from durable business events.

Statistics use a read-only pending-work check before acquiring the writer lock. This removes idle write transactions but does not remove the timer or prove a particular physical I/O rate.

Public distribution rules, known cleanup debt and build-time identity configuration are documented in [public-boundary.md](public-boundary.md). A passing debt-baseline check is not public-release approval.

Run the Rust workspace tests, web tests/build and public-boundary checks before integrating changes to these contracts. Network and runtime tests should use isolated databases and fake providers; do not enable live-provider or production-copy tests as routine validation.
