# Typed event migration preparation

This directory prepares the event migration. Nothing here authorizes applying it to frozen sources.
The committed dry run is based on `3b69ca887`; the live bus and all event producers remain unchanged.

## Commands

Run from the repository root. Python 3, ast-grep 0.51.3 (or compatible), and the pinned Rust toolchain
are required. `AST_GREP` or `--ast-grep` selects the executable; PATH is preferred, with the preparation
machine's `/opt/homebrew/bin/ast-grep` as a fallback. No private services or inventory files are needed.

```sh
ast-grep test --config hub-rs/codemods/w1/events/sgconfig.yml
python3 hub-rs/codemods/w1/events/test_run.py
python3 hub-rs/codemods/w1/events/run.py --dry-run
python3 hub-rs/codemods/w1/events/run.py --dry-run --crate steve-store
CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$PWD/hub-rs/target" \
  python3 hub-rs/codemods/w1/events/compile_rules.py
```

`--dry-run` copies the selected tracked Rust sources into a temporary directory under this checkout's
`scratch/`, applies the rules there, checks the second pass, then removes the copy. Production files
are never changed by a dry run. Two runs on the same source tree produce identical reports. The
reported baseline is the merge base with local `g/1` when present, otherwise `HEAD`.

`--apply --crate X` uses exactly the same plan on the real checkout. It backs up every changed file
under `scratch/events-backup-*/` and prints that relative directory. It is idempotent: transformed
constructors no longer match. A later baseline can be rescanned directly. Only tracked source files
are scanned; stage new producer files before running. No imports or dependency manifests are edited.

```sh
# Only after the sweep window opens:
python3 hub-rs/codemods/w1/events/run.py --apply --crate steve-store
# Roll back the exact backup printed by --apply (only if files have not changed since):
python3 hub-rs/codemods/w1/events/run.py --rollback scratch/events-backup-EXAMPLE
```

Do not apply raw ast-grep fixes directly. Rule fixes replace the enum constructor and add missing
workspace fields. The driver also converts each captured AST field to its newtype/RawJson and adds
`.into()` for the domain/live aggregate. It anchors the old expression's type so `None`, nested
options, shorthand fields and existing `.into()` calls remain inferable. This is necessary for
compilable rewrites. The compile probe type-checks 92 constructions for all 73 rules, including
legacy optional-field cases, and verifies all 34 generated placeholder sites emit warnings.

## Rule inventory

`manifest.json` lists every rule, old field type, new field type, target enum and workspace insertion.
Every `changeevent-VARIANT.yml` has a corresponding `hubevent-VARIANT.yml`: steve-core calls its
ChangeEvent alias `HubEvent`, including variants absent from steve-hub's own narrower enum.
`hubevent-thinking.yml` additionally covers that narrower enum's `Thinking` spelling.
Each rule has positive and negative cases plus committed rewrite snapshots in `rule-tests/`.
Patterns, other enums, tests and known reconstruction adapters are not rewritten.

| Target | Variants (wire tags remain unchanged) |
| --- | --- |
| `WorkEvent` | Item, Decision, Session, Message, ThreadRun |
| `WorkLiveEvent` | Event, SteveThinking, SteveDraft, SteveTool |
| `ConfigEvent` | Bot, Project, Tag, Workspace, Settings, Changeset, Mm |
| `DevicesEvent` | Device, ReleaseHold, Node, Client |
| `ChannelsEvent` | Space, ChanMessage, Persona, Connection, Channel, Digest |
| `PeopleEvent` | Party, Account |
| `VaultEvent` | Vault |
| `AutomationEvent` | Signal, Flow, Run, Calendar |
| `AutonomyEvent` | Yolo, YoloAction |
| `StatsEvent` | Stats |

`audit.yml` independently detects legacy struct expressions, including unknown variants.
`audit-patterns.yml` separates parsed match patterns. A masked-text audit additionally lists legacy
references hidden in opaque macro token trees, which tree-sitter cannot rewrite. Unknown/incomplete
fields and struct updates remain manual, with a file/line reason. A dry run does not claim that
Hub/store production code already compiles with the new contracts.

## T1.b reconciliation on 3b69ca887

The 36 ChangeEvent variants and 14 HubEvent variants/serde attributes are unchanged. There are
39 unique wire tags: 35 domain events and all four live events. The original 205 production
construction sites are 201 ChangeEvent sites (including steve-core's alias) plus four HubEvent sites;
they are **not** 201 constructors plus four JSON publishers.

* Rules cover 204 of those 205 sites (99.51%). The remaining `api/stream.rs:254` is the synthetic
  workspace resynchronization frame: W1.3a must replace it with `StreamControl::Reset`.
* A rerun of the original T1.b scanner now reports 207 references: two existing knowledge-event
  reconstructions moved into `wire/install/domain/knowledge.rs:69,73` inside `install!`. These are
  W1.3a consumer migration work, not additional state-change producers. The independent text audit
  explicitly reports both. With them included, total coverage is 204/207; three manual references
  (1.45%), each explained in `dry-run-3b69ca887.txt`.
* There are 121 workspace placeholders in 77 transformed files. `SteveThinking` retains an existing
  `Some(ws)`; its `None` branch gets a placeholder. Hub/User events get no workspace.
* The four production `publish_value` call sites are `bus.rs:540`, `wire/store.rs:67`,
  `wire/modules.rs:220` and `wire/signals.rs:184`. The first two are Value-forwarding adapters to
  remove. The knowledge callback should publish `KnowledgeEvent::Kb`; split the last notifier
  into `IntegrationsEvent::Gh` and `IntegrationsEvent::Mail`. These are explicit manual W1.3a edits,
  not uncovered constructors. Existing typed `kb`/`gh` ids stay strings because they address several
  entity kinds; `mail.id` is `MailThreadId`.

## Workspace warning list

`steve_types::ws_todo!()` calls a deprecated function whose exact note is
`W1.3b: fill the workspace from the write transaction`. Ordinary `cargo check` emits a warning for
every invocation; it does not stop at the first unresolved site. After the bus transition, run:

```sh
cargo check --manifest-path hub-rs/Cargo.toml -p steve-store --message-format=json
```

Read `compiler-message` entries whose code is `deprecated` and message mentions `ws_todo`, following
macro expansion spans to the producer line. Replace each placeholder with the workspace captured
inside the write transaction (before deletion when applicable). The placeholder panics if executed;
never run a Hub with unresolved placeholders. `rg 'ws_todo!' hub-rs/crates --glob '*.rs'` is the final
source cross-check. Projects with `deny(deprecated)` must temporarily use warning severity while
collecting the full list. No global lint setting is changed by this preparation.

## Legacy wire capture and contract checks

The fixture generator is test tooling, not production code. It extracts the actual legacy enum
source and Hub double-option helper using `git show`, compiles only those definitions in a disposable
standalone crate under `scratch/`, and serializes representative values with the pinned serde
versions. It checks all shared Hub/ChangeEvent values serialize identically. It also evaluates the
actual `kb`/`gh`/`mail` JSON payload expressions. No running Hub, database or source edits are needed.

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$PWD/hub-rs/target" \
  python3 hub-rs/crates/steve-types/tests/capture_legacy.py --baseline 3b69ca887
# Explicitly regenerate reviewed goldens only:
python3 hub-rs/crates/steve-types/tests/capture_legacy.py --baseline 3b69ca887 --bless
STEVE_TS_BLESS=1 cargo test --manifest-path hub-rs/Cargo.toml -p steve-types gen
cargo test --manifest-path hub-rs/Cargo.toml -p steve-types
```

All 48 fixtures were recaptured on `3b69ca887` and match the earlier bytes. They cover all 39 tags,
missing/null/present item_id for both Decision and Session, and optional/skipped fields. The wire
test constructs each variant independently. It removes `ws` from **both** sides (legacy module
fixtures already contain it), compares bytes, adds a required workspace when reading older fixtures,
and checks child/aggregate/envelope scope and round trips. JSON-only publisher fixtures follow their
existing object-map key order; enum fixtures preserve enum serialization order. A separate test
preserves noncanonical opaque payload bytes through actual event envelopes.

Every enum derives `EnumCount`; a missing construction/scope case changes its count and fails the
test. This was verified by temporarily adding a StatsEvent variant **and its scope arm**, leaving the
test table unchanged: the per-variant assertion failed. The temporary change was restored.

RawJson cannot pass through serde's buffered Content deserializer used by ordinary tagged/untagged
enum derives. Event deserializers therefore dispatch from original validated JSON text into typed
field structs. This preserves raw bytes without exposing a JSON tree or changing wire serialization.

DurableFact defines all eight topics as explicit topic/payload variants. Alarm topics reuse
`AlarmFired` without repurposing the existing payload's `kind` string. This follows the preparation
card's eight-topic requirement rather than collapsing the three topics into a new kind field. Known
fields use concrete types; item source, mail event and GitHub signal snapshots use RawJson until
Wave 2. Mail message ids, polymorphic knowledge/digest/GitHub ids and alarm subject/owner ids remain
strings where the existing ID labels would misidentify their domain. No outbox runtime is changed.

## Execution after the sweep opens

1. W1.3a first switches the bus, envelopes, cache invalidation and SSE boundaries. Keep a temporary
   deprecated old-event conversion accepting the caller's workspace until W1.3b finishes. Remove the
   Value adapters, handle the three manual references above and type the JSON publishers. Move
   `tests-to-add/visibility.rs` to `steve-hub/tests/visibility.rs`; it calls the real stream handler and
   checks workspace isolation, user/owner audience and Hub audience with an ordered barrier. It has
   **not** been compiled or run during preparation, since its bus API is still frozen. Add the cache
   generation and end-to-end frame recording checks required by S1 acceptance at that stage.
2. W1.3b reruns the dry run on the new baseline, then runs `--apply --crate X` one crate at a time.
   Add that crate's steve-types dependency/imports, route live events through `live()`, resolve every
   workspace from its write transaction and run `cargo check -p X`. Commit each crate separately;
   mechanical changes and workspace decisions should remain reviewable separately.
3. Remove the legacy enums/conversions only after all crates compile and no production old
   constructors/JSON publishers/placeholders remain. Regenerate TypeScript bindings explicitly;
   run the wire/scope tests, the moved SSE visibility test and the S1 scripted SSE comparison.

The preparation's path ownership check must be run against the updated group baseline. This card
requires this README, while the current public-boundary guard rejects Markdown outside its fixed
allowlist or `/tests/`. Its process-document finding for this README is an explicit unresolved gate
conflict; do not weaken the guard or move files outside the card to conceal it.
