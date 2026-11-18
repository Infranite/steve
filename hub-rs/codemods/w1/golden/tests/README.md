# SSE golden fixtures

These tools record real HTTP mutations against a fresh isolated Hub, keeping both workspace streams.
The contract reference is `3b69ca887`. They prepare the S1 comparison without modifying producers or the bus.
This guide lives with the fixture tests so it follows the repository's public-document boundary.

From the repository root, build the checkout's own binary and record three runs:

```sh
export CARGO_TARGET_DIR="$PWD/scratch/target"
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
buildslot cargo build --manifest-path hub-rs/Cargo.toml -p steve-hub --bin steve-hub
G=hub-rs/codemods/w1/golden
python3 "$G/scenario.py" "$G/baseline-3b69ca887" --port 18720
python3 "$G/scenario.py" scratch/golden-repeat-2 --port 18730
python3 "$G/scenario.py" scratch/golden-repeat-3 --port 18740
python3 "$G/compare.py" "$G/baseline-3b69ca887" scratch/golden-repeat-2
python3 "$G/compare.py" "$G/baseline-3b69ca887" scratch/golden-repeat-3
python3 "$G/compare.py" scratch/golden-repeat-2 scratch/golden-repeat-3
python3 -m unittest discover -s "$G/tests"
```

`buildslot` is the maintainer's optional build-resource wrapper; an ordinary checkout can run Cargo directly.
Python needs only its standard library (3.11+). An existing output directory is refused: choose a new name
for another recording. The committed `repeat-2` and `repeat-3` directories are the other two reference runs.
Inspect normalized evidence with `python3 "$G/normalize.py" <run>`.

The binary must resolve inside this checkout. Every run gets a newly created empty `scratch/golden-*`
home, loopback ports >=18720, `STEVE_NO_RESUME=1`, disabled adapter updates/backups, file vault storage,
and all three adapters set to `/usr/bin/false`. The web directory is this checkout's `web/dist`;
static assets are unnecessary for HTTP API recording. Only the spawned process group is stopped,
including error exits. Hub logs and databases remain in that run's scratch home for diagnosis.
No installed Hub, existing data home, production port, real model, channel credentials, or database edits
are used. Development login cookies remain in memory and are never written into fixtures.

`record.py` opens two simultaneous connections using `lang=en`, explicit `ws`, and the state response's
`x-steve-cursor` as `after`, matching the frontend's resumable stream. Each JSONL row preserves the
operation name, SSE id, SSE event (default `message`), and unparsed data text. There is no reconnect:
EOF or reader failure fails the run. Each operation is followed by at least 5.6 seconds of settling
(6 for model/knowledge paths), then an HTTP state readback and 400 ms without new frames. This lets
five-second stats coalescing drain before the next operation. Named empty steps remain in the manifest.
The manifest records HTTP status, workspace, entity ownership, and the binary SHA-256; it contains no
HTTP response bodies, filesystem locations, credentials or hostnames.

## Comparison rules

Identifiers become `<item#1>`-style placeholders at first observation. IDs independently observed in
HTTP responses/readbacks are registered first, so cross-connection scheduling cannot rename them.
Other identifiers are registered in frame order. Timestamps, epoch/cursor numbers and revisions are
normalized; stats calendar day becomes `<day>`. Transcript `seq`, payload text, field presence/null,
SSE event type, duplicate counts, step boundaries and cursor presence remain significant.
`normalize.py` retains top-level `ws` as separate evidence, and preserves nested payload fields.

All three reference runs have identical arrival order within every step, so no step uses sorting.
Comparison preserves arrival order and duplicate frames. Transport `event: ping`
is kept in raw JSONL and excluded from mutation comparison because its count depends on wall time.
`stream.gap` and `stream.reset` are compared normally.

For S1, record the same scenario with the new binary and run:

```sh
python3 "$G/compare.py" "$G/baseline-3b69ca887" scratch/golden-after --scoped
```

Without `--scoped`, both complete workspace streams must match (including legacy cross-workspace
broadcasts). With it, the baseline is projected onto independently observed entity/operation ownership:
legacy broadcasts to the other workspace are no longer expected. Every post-S1 workspace frame must
carry the independently expected `ws` and appear only on that workspace's connection. Existing scoped
frames are checked even in baseline comparisons. Hub (`device`, `release.hold`) and User (`node`,
`client`) frames retain their current global/user visibility; both test connections use the same owner.
Stats has no entity ID: its workspace aggregate must be one of the two known workspaces and match
its connection. This cannot prove user-vs-owner access; the S1 per-variant scope tests cover that.

For asynchronously created entities, `/api/state` readback supplies ownership where available;
otherwise the named originating operation supplies it. The second workspace has a distinct item
prefix and separate item/contact/rename steps, so identical entity kinds exist in both workspaces.
A removed/missing/duplicate frame, changed payload, wrong workspace, or cross-connection leak fails.
The comparator's negative tests exercise these cases; scoped comparison has not been run on S1 code.

## Coverage

Coverage and the exact counts below are derived from the committed two-stream recordings, not
inferred from API success. There are 36 ChangeEvent wire types plus `kb`, `gh`, `mail` (39 total).

| Wire type | A frames | B frames | Coverage / reason absent |
|---|---:|---:|---|
| `account` | 1 | 0 | Covered by the named HTTP scenario. |
| `bot` | 3 | 3 | Covered by the named HTTP scenario. |
| `calendar` | 0 | 0 | Requires a synchronized provider calendar; no local create route. |
| `chan_message` | 0 | 0 | Requires an ingested channel message; no channel is connected. |
| `changeset` | 0 | 0 | Configuration drafts require model output, refused by the disabled adapters. |
| `channel` | 0 | 0 | Requires channel/provider configuration and credentials. |
| `client` | 0 | 0 | Development login has no paired client identity; pairing/sign-in lifecycle is not exercised. |
| `connection` | 0 | 0 | Requires a configured provider/connector lifecycle. |
| `decision` | 5 | 5 | Covered by the named HTTP scenario. |
| `device` | 0 | 0 | Device configuration requires a connected Node, disabled by NO_RESUME. |
| `digest` | 0 | 0 | Fresh home has no ingested conversation/profile or digest-job inputs. |
| `event` | 1 | 0 | Covered by the named HTTP scenario. |
| `flow` | 4 | 0 | Covered by the named HTTP scenario. |
| `gh` | 0 | 0 | Requires a GitHub integration notification. |
| `item` | 22 | 22 | Covered by the named HTTP scenario. |
| `kb` | 0 | 0 | No knowledge index/embedding backend is configured; memory writes alone emit no kb notice. |
| `mail` | 0 | 0 | Requires a mail integration notification. |
| `message` | 2 | 2 | Covered by the named HTTP scenario. |
| `mm` | 1 | 0 | Covered by the named HTTP scenario. |
| `node` | 0 | 0 | Requires Node pairing/connection; NO_RESUME disables the local Node. |
| `party` | 10 | 5 | Covered by the named HTTP scenario. |
| `persona` | 2 | 0 | Covered by the named HTTP scenario. |
| `project` | 2 | 2 | Covered by the named HTTP scenario. |
| `release.hold` | 0 | 0 | Requires an actual upgrade/release gate; no installation is upgraded. |
| `run` | 2 | 0 | Covered by the named HTTP scenario. |
| `session` | 2 | 2 | Covered by the named HTTP scenario. |
| `settings` | 1 | 1 | Covered by the named HTTP scenario. |
| `signal` | 0 | 0 | Requires external-event ingestion; the manual trigger opens a run without a signal. |
| `space` | 0 | 0 | Requires an ingested channel conversation; no channel is connected. |
| `stats` | 7 | 3 | Covered by the named HTTP scenario. |
| `steve.draft` | 0 | 0 | No successful model stream exists after the Node/adapter refusal. |
| `steve.thinking` | 5 | 0 | Covered by the named HTTP scenario. |
| `steve.tool` | 0 | 0 | No research/tool model stream exists after the Node/adapter refusal. |
| `tag` | 3 | 3 | Covered by the named HTTP scenario. |
| `thread.run` | 3 | 0 | Covered by the named HTTP scenario. |
| `vault` | 0 | 0 | Vault provisioning/unlock and creation of key material are outside this fixture. |
| `workspace` | 1 | 1 | Covered by the named HTTP scenario. |
| `yolo` | 1 | 0 | Covered by the named HTTP scenario. |
| `yolo_action` | 0 | 0 | Requires a pending autonomous permission/action; execution remains disabled. |

`stream.gap` is covered by the resumable connection handshake. `ping` is also recorded.
`stream.reset` requires a stale cursor/restart or lag and is outside this mutation scenario.

The item supplement returns the explicitly asserted HTTP 500 because `STEVE_NO_RESUME=1` also
turns off the local Node; it still creates session/decision notifications and a refused system
transcript entry. This is failure-path coverage, not a successful comment/adapter turn. Proposed
items produce decisions without a model. The main conversation also fails without a Node, producing
its actual thread events. Account creation only records a synthetic address, never connects it.
The manual flow contains only a manual trigger; it has no actions, external I/O or model calls.
YOLO's project scope is explicitly set to `off`; autonomous execution is never enabled.

## Cache generations

`bus_generation.rs` pins all 39 wire types, typed HubEvent publishing/accumulation, and invalid/missing
workspace behavior using only the Bus public API. Ordinary tests pass on the reference baseline.
Three ignored tests express S1: workspace isolation (including `steve.thinking`), global Hub/User
invalidation except `client`, and the unchanged stateless set: `event`, `steve.draft`, `steve.tool`,
`stats`, `client`. All three pass with `--ignored` on the reference baseline. Workspace fixtures
explicitly inject `ws`, so this does not prove that legacy producers now supply it: none of these
tests currently fails for missing producer metadata. W1.3a must replace the JSON construction with
typed events and remove the ignores. No production behavior is changed by these preparatory tests.
