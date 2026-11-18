#!/usr/bin/env python3
"""Prepare event constructors with ast-grep; dry runs modify only scratch/ copies."""
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
    parser = argparse.ArgumentParser(description=__doc__)
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
                reason = "typed kb event" if rel.endswith("wire/modules.rs") else "split gh/mail notifier" if rel.endswith("wire/signals.rs") else "remove Value forwarding adapter"
                print(f"  {rel}:{line}: {reason}")
        print("Warning: no Hub/store production source was compiled by this preparation step.")


if __name__ == "__main__":
    main()
