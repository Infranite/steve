#!/usr/bin/env python3
r"""Typed event migration preparation

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
explicit `DomainEvent::from(...)` / `LiveEvent::from(...)` for the aggregate. It anchors the old expression's type so `None`, nested
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
fixtures already contain it), compares bytes, and checks child/aggregate/envelope scope and
serialization. Bus events and envelopes are Serialize-only: the web client reads SSE JSON. JSON-only publisher fixtures follow their
existing object-map key order; enum fixtures preserve enum serialization order. A separate test
preserves noncanonical opaque payload bytes through actual event envelopes.

Every enum derives `EnumCount`; a missing construction/scope case changes its count and fails the
test. This was verified by temporarily adding a StatsEvent variant **and its scope arm**, leaving the
test table unchanged: the per-variant assertion failed. The temporary change was restored.

DurableFact must be readable from storage in Wave 2, so it retains Deserialize. A direct
adjacent-tag derive passes for topic-before-payload JSON, but fails with
"invalid type: newtype struct, expected any valid JSON value" when a RawJson-containing payload
precedes topic: serde buffers it into Content, which cannot retain RawJson. Only DurableFact keeps
original-text deserialization for this reason. Its tests cover both key orders and preserve the raw
snapshot bytes. Bus events have no Deserialize implementations, dispatch table or wrapper macro.

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
"""
import argparse
from collections import Counter, defaultdict
from functools import lru_cache
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
# These are adapters/reconstruction code, not producer sites (T1.b scanner's exclusions).
ADAPTERS = {
    "steve-knowledge/src/sync.rs", "steve-mail/src/threads.rs",
    "steve-hub/src/wire/modules.rs", "steve-hub/src/wire/signals.rs",
    # Value taps and the knowledge subscriber remain legacy until W1.3c (card §4).
    "steve-hub/src/wire/install/domain/knowledge.rs",
}
RAW = re.compile(r'r(#{0,})"')
CHAR = re.compile(r"'(?:\\.|[^'\\\n])'")


@lru_cache(maxsize=256)
def mask(s):
    """Keep offsets/newlines, mask Rust comments and literals (including nested/raw ones)."""
    out = list(s)
    i = 0
    while i < len(s):
        a = i
        if s.startswith("//", i):
            end = s.find("\n", i)
            i = len(s) if end < 0 else end
        elif s.startswith("/*", i):
            depth = 1
            i += 2
            while i < len(s) and depth:
                if s.startswith("/*", i):
                    depth += 1
                    i += 2
                elif s.startswith("*/", i):
                    depth -= 1
                    i += 2
                else:
                    i += 1
        elif match := RAW.match(s, i):
            end = s.find('"' + match[1], match.end())
            i = len(s) if end < 0 else end + 1 + len(match[1])
        elif s[i] == '"':
            i += 1
            while i < len(s):
                if s[i] == "\\":
                    i += 2
                elif s[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
        elif s[i] == "'" and (match := CHAR.match(s, i)):
            i = match.end()
        else:
            i += 1
            continue
        for k in range(a, min(i, len(s))):
            if out[k] != "\n":
                out[k] = " "
    return "".join(out)


@lru_cache(maxsize=256)
def test_ranges(s):
    m = mask(s)
    ranges = []
    for t in re.finditer(r'#\[cfg\(test\)\]|#\[(?:tokio::)?test(?:\([^\]]*\))?\]', s):
        start = m.find("{", t.end())
        if start < 0:
            continue
        depth = 0
        for i in range(start, len(m)):
            depth += (m[i] == "{") - (m[i] == "}")
            if depth == 0:
                ranges.append((t.start(), i + 1))
                break
    return ranges


def run_json(command, cwd):
    result = subprocess.run(command, cwd=cwd, text=True, capture_output=True)
    if result.returncode not in (0, 1):
        raise RuntimeError(result.stderr)
    return json.loads(result.stdout or "[]")


def classify(hit, sources):
    rel = hit["file"]
    s = sources[rel]
    # ast-grep offsets are bytes; cfg scanner works in characters.
    offset = len(s.encode()[:hit["range"]["byteOffset"]["start"]].decode())
    if "/tests/" in rel or "test" in Path(rel).stem or any(a <= offset < b for a, b in test_ranges(s)):
        return "test (not a production constructor)"
    if rel.endswith(("/bus.rs", "/sse.rs")) or any(rel.endswith(p) for p in ADAPTERS):
        return "adapter/reconstruction (W1.3a, excluded by T1.b producer inventory)"
    return None


def convert(expr, old, new):
    if old == new:
        return expr
    if new == "Seq":
        return f"steve_types::Seq({expr})"
    if new == "Option<Millis>":
        return f"({expr}).map(steve_types::Millis)"
    # Anchor the old type so expressions such as id.into(), None and Some(None) stay inferable.
    old_rust = old.replace("Value", "serde_json::Value")
    value = f"{{ let value: {old_rust} = {expr}; "
    if "RawJson" in new:
        raw = 'steve_types::RawJson::from_str(&serde_json::to_string(&value).expect("JSON value")).expect("validated JSON")'
        return value + (f"value.map(|value| {raw})" if old.startswith("Option") else raw) + " }"
    if old == "String":
        return value + f"steve_types::{new}::from(value) }}"
    if old == "Option<String>":
        return value + "value.map(Into::into) }"
    if old == "Option<Option<String>>":
        return value + "value.map(|value| value.map(Into::into)) }"
    raise ValueError(f"unsupported field conversion: {old} -> {new}")


def replacement(hit, rule):
    fields = hit["metaVariables"]["multi"]["FIELDS"]
    expected = dict(rule["fields"])
    types = dict(rule["new_fields"])
    output = hit["replacement"]
    cursor = output.index("{") + 1
    seen = set()
    for field in fields:
        text = field["text"]
        if text == ",":
            continue
        # Captures are AST field nodes; split only the field's first colon, never expression commas.
        m = re.match(r"^(\w+)\s*:(?!:)([\s\S]*)$", text)
        if m:
            name, expr = m[1], m[2].strip()
        elif re.fullmatch(r"\w+", text):
            name = expr = text
        else:
            raise ValueError("field attributes/struct update need manual migration")
        if name not in expected or name in seen:
            raise ValueError(f"unknown or repeated field {name}")
        seen.add(name)
        if rule["variant"] == "SteveThinking" and name == "ws":
            expr = (f"{{ let value: Option<String> = {expr}; "
                    "value.map(Into::into).unwrap_or_else(|| steve_types::ws_todo!()) }")
        else:
            expr = convert(expr, expected[name], types[name])
        transformed = f"{name}: {expr}"
        start = output.index(text, cursor)
        output = output[:start] + transformed + output[start + len(text):]
        cursor = start + len(transformed)
    if seen != set(expected):
        raise ValueError("incomplete field list (pattern or struct update)")
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--dry-run", action="store_true")
    mode.add_argument("--apply", action="store_true")
    mode.add_argument("--rollback", type=Path, help="restore an --apply backup in scratch/")
    parser.add_argument("--crate", help="only this crate, e.g. steve-store")
    parser.add_argument("--ast-grep", default=os.environ.get("AST_GREP") or shutil.which("ast-grep") or "/opt/homebrew/bin/ast-grep")
    args = parser.parse_args()
    scratch = ROOT / "scratch"
    scratch.mkdir(exist_ok=True)
    if args.rollback:
        backup = args.rollback.resolve()
        if not backup.is_relative_to(scratch.resolve()):
            parser.error("backup must be under this checkout's scratch/")
        entries = json.loads((backup / "backup.json").read_text())
        for entry in entries:
            path = ROOT / entry["path"]
            if not path.resolve().is_relative_to((ROOT / "hub-rs/crates").resolve()):
                parser.error("invalid backup path")
            if hashlib.sha256(path.read_bytes()).hexdigest() != entry["after"]:
                parser.error(f"refusing to overwrite changes made after apply: {entry['path']}")
        for entry in entries:
            shutil.copy2(backup / entry["path"], ROOT / entry["path"])
        print(f"Restored {len(entries)} files")
        return
    files = subprocess.check_output(["git", "ls-files", "hub-rs/crates/*/src/*.rs"], cwd=ROOT, text=True).splitlines()
    if args.crate:
        if not re.fullmatch(r"steve-[a-z0-9-]+", args.crate):
            parser.error("invalid crate name")
        files = [p for p in files if Path(p).parts[2] == args.crate]
        if not files:
            parser.error("crate has no tracked Rust source")
    sources = {p: (ROOT / p).read_text() for p in files}
    manifest = {r["id"]: r for r in json.loads((HERE / "manifest.json").read_text())}
    merge_base = subprocess.run(["git", "merge-base", "HEAD", "g/1"], cwd=ROOT, text=True, capture_output=True)
    baseline = merge_base.stdout.strip() if merge_base.returncode == 0 else subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    with tempfile.TemporaryDirectory(prefix="events-dry-", dir=scratch) as temp:
        temp = Path(temp)
        for rel, body in sources.items():
            dest = temp / rel
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(body)
        hits = run_json([args.ast_grep, "scan", "--config", str(HERE / "sgconfig.yml"), "--json=compact", "hub-rs/crates"], temp)
        candidates = run_json([args.ast_grep, "scan", "--rule", str(HERE / "audit.yml"), "--json=compact", "hub-rs/crates"], temp)
        # Tree-sitter treats Rust macro token trees as opaque. A second, masked-text audit must
        # expose such sites rather than claiming AST rules cover everything they did not parse.
        patterns = run_json([args.ast_grep, "scan", "--rule", str(HERE / "audit-patterns.yml"), "--json=compact", "hub-rs/crates"], temp)
        ast_keys = {(h["file"], h["range"]["byteOffset"]["start"]) for h in candidates + patterns}
        for rel, body in sources.items():
            for match in re.finditer(r"(?<![\w:])(?:[A-Za-z_]\w*::)*(?:ChangeEvent|HubEvent)::([A-Za-z_]\w*)\s*\{", mask(body)):
                offset = len(body[:match.start()].encode())
                if (rel, offset) in ast_keys:
                    continue
                line = body.count("\n", 0, match.start())
                candidates.append({"file": rel, "text": match[0], "ruleId": "opaque-macro-audit",
                    "range": {"byteOffset": {"start": offset}, "start": {"line": line}}})
        counts = Counter()
        crates = Counter()
        edits = defaultdict(list)
        covered = set()
        uncovered = []
        ws = 0
        for hit in sorted(hits, key=lambda h: (h["file"], h["range"]["byteOffset"]["start"], h["ruleId"])):
            if classify(hit, sources):
                continue
            rule = manifest[hit["ruleId"]]
            key = (hit["file"], hit["range"]["byteOffset"]["start"])
            if hit["file"].endswith("steve-hub/src/api/stream.rs") and rule["variant"] == "Workspace":
                uncovered.append((hit, "synthetic resync: replace with StreamControl::Reset in W1.3a"))
                continue
            try:
                new = replacement(hit, rule)
            except ValueError as error:
                uncovered.append((hit, str(error)))
                continue
            if key in covered:
                raise RuntimeError(f"overlapping rules: {key}")
            covered.add(key)
            counts[hit["ruleId"]] += 1
            crates[Path(hit["file"]).parts[2]] += 1
            ws += rule["add_ws"] or rule["variant"] == "SteveThinking"
            byte_range = hit["range"]["byteOffset"]
            edits[hit["file"]].append((byte_range["start"], byte_range["end"], new))
        known = {(h["file"], h["range"]["byteOffset"]["start"]) for h, _ in uncovered}
        excluded = Counter()
        for hit in candidates:
            reason = classify(hit, sources)
            if reason:
                excluded[reason] += 1
                continue
            key = (hit["file"], hit["range"]["byteOffset"]["start"])
            if key not in covered and key not in known:
                reason = "no rule for this legacy constructor"
                if hit["ruleId"] == "opaque-macro-audit":
                    reason = "opaque macro/pattern: inspect manually (AST rules cannot parse token trees)"
                    if hit["file"].endswith("wire/install/domain/knowledge.rs"):
                        reason = "install! macro: relocated knowledge event reconstruction, migrate in W1.3a"
                uncovered.append((hit, reason))
        transformed = {}
        for rel, changes in edits.items():
            data = sources[rel].encode()
            for a, b, new in sorted(changes, reverse=True):
                data = data[:a] + new.encode() + data[b:]
            (temp / rel).write_bytes(data)
            transformed[rel] = data
        # Prove complete rules are idempotent on the transformed copy (uncovered sites stay manual).
        second = run_json([args.ast_grep, "scan", "--config", str(HERE / "sgconfig.yml"), "--json=compact", "hub-rs/crates"], temp)
        remaining = [h for h in second if not classify(h, {**sources, **{p: b.decode() for p, b in transformed.items()}})]
        expected_remaining = Counter((h["file"], h["text"]) for h, _ in uncovered if h["ruleId"] in manifest)
        actual_remaining = Counter((h["file"], h["text"]) for h in remaining)
        if actual_remaining != expected_remaining:
            raise RuntimeError("second pass changed coverage; inspect overlapping or incomplete rules")
        if args.apply and transformed:
            backup = Path(tempfile.mkdtemp(prefix="events-backup-", dir=scratch))
            entries = []
            for rel, data in sorted(transformed.items()):
                dest = backup / rel
                dest.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(ROOT / rel, dest)
                entries.append({"path": rel, "after": hashlib.sha256(data).hexdigest()})
            (backup / "backup.json").write_text(json.dumps(entries, indent=2) + "\n")
            for rel, data in sorted(transformed.items()):
                (ROOT / rel).write_bytes(data)
            print("Backup:", backup.relative_to(ROOT))
        print(f"Baseline: {baseline}")
        print("Mode:", "apply" if args.apply else "dry-run (temporary copy under scratch/)")
        print("Crate:", args.crate or "all")
        print("Per rule:")
        for rid in sorted(manifest):
            print(f"  {rid}: {counts[rid]}")
        print("Per crate:")
        for crate, n in sorted(crates.items()):
            print(f"  {crate}: {n}")
        total = len(covered) + len(uncovered)
        print(f"Producer/reference candidates: {total}; covered: {len(covered)}; manual: {len(uncovered)}")
        print(f"Uncovered: {len(uncovered) / max(total, 1):.2%}")
        for hit, reason in sorted(uncovered, key=lambda pair: (pair[0]["file"], pair[0]["range"]["start"]["line"])):
            print(f"  {hit['file']}:{hit['range']['start']['line'] + 1}: {reason}")
        print(f"Workspace placeholders to resolve: {ws}")
        print(f"Files transformed in copy: {len(transformed)}")
        print("Second pass: no further covered constructors (idempotent)")
        for reason, n in sorted(excluded.items()):
            print(f"Excluded: {n} {reason}")
        print("publish_value production calls (manual W1.3a):")
        for rel, body in sorted(sources.items()):
            for match in re.finditer(r'\bpublish_value\s*\(', mask(body)):
                if re.search(r'\bfn\s*$', body[max(0, match.start() - 8):match.start()]):
                    continue
                if any(a <= match.start() < b for a, b in test_ranges(body)):
                    continue
                line = body.count("\n", 0, match.start()) + 1
                reason = "retained legacy forwarding (W1.3c)" if rel.endswith("bus.rs") else "unexpected production Value publisher"
                print(f"  {rel}:{line}: {reason}")
        print("Warning: no Hub/store production source was compiled by this preparation step.")


if __name__ == "__main__":
    main()
