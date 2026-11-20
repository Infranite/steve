#!/usr/bin/env python3
"""Reject new or growing inventory keys against a Git baseline.

Inventory-Growth trailers bind reviews to an exact key result or an initial file blob.
The trailer is an audit marker, not proof of review: reviewers must assess the printed growth.
Snapshots come from Git objects, so deleting and reinitializing a working file is no escape.
"""
import argparse
import json
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys

from structure_guard import VALUE, compare

ROOT = Path(__file__).resolve().parents[1]
HUB_TESTS = "hub-rs/crates/steve-hub/tests"
SQLITE_TESTS = "hub-rs/crates/steve-store-sqlite/tests"
SCHEMA = "hub-rs/crates/steve-store-sqlite/schema"
STRUCTURE = "hub-rs/structure.txt"
DIGEST_WRITES = f"{SQLITE_TESTS}/digest_write_sites.txt"
RUN_OWNERSHIP = "hub-rs/crates/steve-flow/tests/run_write_ownership.txt"
DUE_WAKE = f"{HUB_TESTS}/due_wake_sites.txt"
I18N_REMAINING = f"{HUB_TESTS}/i18n_remaining.txt"
WEB = {"web/test/guard-zh-compare.txt", "web/test/harness-parity.txt"}


class InventoryError(ValueError):
    pass


def is_inventory(path):
    file = PurePosixPath(path)
    parent = file.parent.as_posix()
    return (path in (STRUCTURE, DIGEST_WRITES, RUN_OWNERSHIP) or path in WEB
            or (parent == HUB_TESTS and file.suffix == ".txt" and file.name != "layering.txt")
            or (parent == SQLITE_TESTS and file.name.startswith("inventory_") and file.suffix == ".txt")
            or (parent == SCHEMA and (file.name == "triggers.tsv"
                                     or (file.name.startswith("debt_") and file.suffix == ".txt"))))


def positive(text):
    if not re.fullmatch(r"[0-9]+", text) or int(text) <= 0:
        raise InventoryError(f"expected a positive decimal integer, found {text!r}")
    return int(text)


def nonnegative(text):
    if not re.fullmatch(r"[0-9]+", text):
        raise InventoryError(f"expected a nonnegative decimal integer, found {text!r}")
    return int(text)


def run_columns(text):
    """A missing legacy bare column is unknown, never an implicit zero budget."""
    return next((len(line.removesuffix("\r").split("\t")) for line in text.split("\n")
                 if line and not line.startswith("#")), None)


def parse(path, text):
    """Every inventory projects to the structure guard's key -> descending numeric multiset."""
    entries, arity = {}, None
    lines = text.split("\n")  # Unicode separators inside JSON literals are not inventory newlines.
    for index, line in enumerate(lines):
        if index < len(lines) - 1:
            line = line.removesuffix("\r")
        if not line or line.startswith("#"):
            continue
        try:
            fields = line.split("\t")
            if any(not field for field in fields):
                raise InventoryError("empty inventory field")
            if arity is not None and len(fields) != arity:
                raise InventoryError("inconsistent number of key fields")
            arity = len(fields)
            if path == RUN_OWNERSHIP:
                if len(fields) not in (2, 3):
                    raise InventoryError("expected path, total and optional explicit bare column")
                total = positive(fields[1])
                columns = [("total", total)]
                if len(fields) == 3:
                    bare = nonnegative(fields[2])
                    if bare > total:
                        raise InventoryError("bare count exceeds total")
                    columns.append(("bare", bare))
                for column, value in columns:
                    key = (fields[0], column)
                    if key in entries:
                        raise InventoryError(f"duplicate key: {' '.join(key)}")
                    entries[key] = [value]
                continue
            elif path == DIGEST_WRITES:
                if len(fields) != 3:
                    raise InventoryError("expected kind, path and count")
                key, values = tuple(fields[:2]), [positive(fields[2])]
            elif path == DUE_WAKE:
                if len(fields) != 2:
                    raise InventoryError("expected file and count")
                key, values = (fields[0],), [positive(fields[1])]
            elif path == STRUCTURE:
                if len(fields) != 4 or fields[2] not in VALUE:
                    raise InventoryError("expected crate, file, known lint and comma-separated values")
                key = tuple(fields[:3])
                values = [positive(value) for value in fields[3].split(",")]
                if values != sorted(values, reverse=True):
                    raise InventoryError("structure values must be sorted largest first")
            elif str(PurePosixPath(path).parent) == SCHEMA:
                if PurePosixPath(path).name == "triggers.tsv":
                    if len(fields) != 4:
                        raise InventoryError("expected trigger, table, domain and replacement operation")
                    key = (fields[0],)
                else:
                    if len(fields) != 3:
                        raise InventoryError("expected debt kind, stable id and description")
                    key = tuple(fields[:2])
                values = [1]  # Metadata can change without creating another debt/trigger identity.
            elif path == I18N_REMAINING and len(fields) == 2:
                # Published Git history used a set here before counts were added. Compare that
                # historical membership as 1, rather than failing the first reform -> main PR.
                if not (fields[1].startswith('"') and fields[1].endswith('"')):
                    raise InventoryError("expected a quoted legacy i18n literal")
                key, values = tuple(fields), [1]
            else:
                if len(fields) < 2:
                    raise InventoryError("expected tab-separated key fields followed by a count")
                key, values = tuple(fields[:-1]), [positive(fields[-1])]
            if key in entries:
                raise InventoryError(f"duplicate key: {' '.join(key)}")
            entries[key] = values
        except ValueError as error:
            raise InventoryError(f"{path}:{index + 1}: {error}") from error
    return entries


def key_text(key):
    return "\t".join(key)


def value_text(values):
    return ",".join(map(str, values))


def describe(change):
    if change["kind"] == "removed":
        return "inventory file removed; retain an empty list or name its removal"
    if change["kind"] == "format":
        return (f"inventory format changed {change['before_format']}->{change['head_format']}; "
                "explicit human format review required; a missing bare column has no baseline")
    return (f"{key_text(change['key'])} = {value_text(change['head'])} "
            f"(baseline {value_text(change['before']) or 'absent'})")


def compare_snapshots(before, after):
    paths = sorted(path for path in before.keys() | after.keys() if is_inventory(path))
    changes = {}
    for path in paths:
        listed, current = parse(path, before.get(path, "")), parse(path, after.get(path, ""))
        problems = []
        if path == RUN_OWNERSHIP and path in before and path in after:
            old_format, new_format = run_columns(before[path]), run_columns(after[path])
            if old_format is not None and new_format is not None and old_format != new_format:
                problems.append({"kind": "format", "before_format": old_format, "head_format": new_format})
        for key in sorted(listed.keys() | current.keys()):
            was = {key: listed[key]} if key in listed else {}
            now = {key: current[key]} if key in current else {}
            growth, _ = compare(was, now)
            if growth:
                problems.append({"kind": "budget", "key": key, "before": listed.get(key, []),
                                 "head": current[key]})
        if path in before and path not in after:
            problems.append({"kind": "removed"})
        if problems:
            changes[path] = problems
    return paths, changes


def git(repo, *args, input_text=None):
    result = subprocess.run(["git", "-C", str(repo), "--literal-pathspecs", *args], capture_output=True,
                            input=None if input_text is None else input_text.encode("utf-8"))
    if result.returncode:
        raise InventoryError(f"git {args[0]} failed: {result.stderr.decode('utf-8', errors='replace').strip()}")
    try:
        return result.stdout.decode("utf-8")
    except UnicodeDecodeError as error:
        raise InventoryError(f"git {args[0]} returned non-UTF-8 inventory data") from error


def resolve(repo, revision):
    try:
        return git(repo, "rev-parse", "--verify", "--end-of-options", f"{revision}^{{commit}}").strip()
    except InventoryError as error:
        raise InventoryError(f"cannot resolve comparison commit {revision!r}; fetch its history (checkout fetch-depth: 0): {error}") from error


def snapshot(repo, revision):
    paths = git(repo, "ls-tree", "-r", "--name-only", "-z", revision).split("\0")
    return {path: git(repo, "show", f"{revision}:{path}") for path in paths if is_inventory(path)}


def growth_markers(repo, base, head):
    markers = []
    for commit in git(repo, "rev-list", "--reverse", f"{base}..{head}").splitlines():
        message = git(repo, "show", "-s", "--format=%B", commit)
        trailers = git(repo, "interpret-trailers", "--parse", input_text=message)
        for line in trailers.split("\n"):
            key, separator, value = line.partition(":")
            if not separator or key.casefold() != "inventory-growth":
                continue
            markers.append({"commit": commit, "text": value.strip()})
    return markers


def valid_path(path):
    return (is_inventory(path) and not PurePosixPath(path).is_absolute()
            and ".." not in PurePosixPath(path).parts and str(PurePosixPath(path)) == path)


def path_history_blobs(repo, base, head, path):
    blobs = set()
    commits = git(repo, "log", "--full-history", "--format=%H", f"{base}..{head}", "--", path).splitlines()
    for commit in commits:
        entry = git(repo, "ls-tree", commit, "--", path)
        if entry:
            _, kind, blob = entry.split("\t", 1)[0].split()
            if kind == "blob":
                blobs.add(blob)
    return blobs


def import_baselines(repo, base, head, before, after, markers):
    effective, imports, notes, history = dict(before), {}, [], {}
    pattern = re.compile(r"^(\S+):\s*import ([0-9a-fA-F]{40}):\s*(\S.*)$")
    for marker in markers:
        match = pattern.fullmatch(marker["text"])
        if not match:
            continue
        path, blob, reason = match.groups()
        blob = blob.lower()
        if not valid_path(path):
            continue
        if path in before or path not in after:
            notes.append(f"ignored import {path}: only a newly introduced, retained inventory can use an import anchor")
            continue
        if path not in history:
            history[path] = path_history_blobs(repo, base, head, path)
        if blob not in history[path]:
            notes.append(f"ignored import {path}: blob {blob} is not in this path's history within the comparison range")
            continue
        if path in imports and imports[path]["blob"] != blob:
            raise InventoryError(f"conflicting import anchors for {path}; keep one reviewed baseline blob")
        effective[path] = git(repo, "cat-file", "blob", blob)
        imports[path] = {"kind": "import", "commit": marker["commit"], "path": path,
                         "blob": blob, "reason": reason}
    return effective, list(imports.values()), notes


def authorize(changes, markers):
    applied, covered = [], set()
    required = [(path, index, change) for path, items in changes.items() for index, change in enumerate(items)]
    for marker in markers:
        matches = []
        for path, index, change in required:
            if change["kind"] == "removed":
                prefix = f"{path}:"
            elif change["kind"] == "format":
                prefix = f"{path}: format {change['before_format']}->{change['head_format']}:"
            else:
                prefix = f"{path} {key_text(change['key'])} = {value_text(change['head'])}:"
            if marker["text"].startswith(prefix):
                reason = marker["text"][len(prefix):].strip()
                structured_import_or_format = reason.startswith("import ") or re.match(r"format [0-9]+->[0-9]+:", reason)
                if reason and not (change["kind"] == "removed" and structured_import_or_format):
                    matches.append((len(prefix), path, index, change, reason))
        if not matches:
            continue
        longest = max(item[0] for item in matches)
        matches = [item for item in matches if item[0] == longest]
        if len(matches) != 1:
            raise InventoryError("ambiguous inventory review key; use the exact result printed by the comparator")
        _, path, index, change, reason = matches[0]
        covered.add((path, index))
        review = {"kind": change["kind"], "commit": marker["commit"], "path": path,
                  "reason": reason, "key": change.get("key"), "head": change.get("head")}
        if change["kind"] == "format":
            review.update(before_format=change["before_format"], head_format=change["head_format"])
        applied.append(review)
    missing = [(path, change) for path, index, change in required if (path, index) not in covered]
    return applied, missing


def event_sha(name, event, keys):
    value = event
    for key in keys:
        value = value.get(key) if isinstance(value, dict) else None
    if not isinstance(value, str) or not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", value):
        raise InventoryError(f"{name} has no valid {'.'.join(keys)} SHA")
    return value


def is_ancestor(repo, before, head):
    result = subprocess.run(["git", "-C", str(repo), "merge-base", "--is-ancestor", before, head], capture_output=True)
    if result.returncode not in (0, 1):
        raise InventoryError("cannot establish push ancestry; fetch the complete before/head history")
    return result.returncode == 0


def event_range(repo, name, event, head="HEAD"):
    if not isinstance(event, dict):
        raise InventoryError("GitHub event payload must be an object")
    if name not in ("push", "pull_request", "merge_group"):
        raise InventoryError(f"unsupported GitHub event {name!r}; use --base for local checks")
    head = resolve(repo, head)
    if name == "push":
        before = event_sha(name, event, ("before",))
        if set(before) == {"0"}:
            return {"base": f"{head}^", "head": head,
                    "reason": "push before is all-zero (new branch); falling back to HEAD^"}
        try:
            before = resolve(repo, before)
        except InventoryError:
            if event.get("forced") is True:
                return {"base": f"{head}^", "head": head,
                        "reason": "payload confirms force push and before is unavailable; falling back to HEAD^"}
            raise
        if is_ancestor(repo, before, head):
            return {"base": before, "head": head, "reason": "fast-forward push: before..HEAD"}
        return {"base": f"{head}^", "head": head,
                "reason": "push before is not an ancestor (force push); falling back to HEAD^"}
    if name == "pull_request":
        base = resolve(repo, event_sha(name, event, ("pull_request", "base", "sha")))
        source = resolve(repo, event_sha(name, event, ("pull_request", "head", "sha")))
        parents = git(repo, "show", "-s", "--format=%P", head).split()
        if parents != [base, source]:
            raise InventoryError(f"PR merge parents do not match payload: expected base {base} and head {source}; found {parents}")
        return {"base": base, "head": head, "reason": "PR target snapshot; merge parents match base.sha/head.sha"}
    base = event_sha(name, event, ("merge_group", "base_sha"))
    return {"base": base, "head": head, "reason": "merge_group target base..HEAD"}


def check(repo, base, head="HEAD"):
    base, head = resolve(repo, base), resolve(repo, head)
    before, after = snapshot(repo, base), snapshot(repo, head)
    _, preliminary = compare_snapshots(before, after)
    candidates = growth_markers(repo, base, head) if preliminary else []
    effective, imports, notes = import_baselines(repo, base, head, before, after, candidates)
    paths, changes = compare_snapshots(effective, after)
    markers, missing = authorize(changes, candidates)
    totals = {}
    for path in paths:
        was, now = parse(path, effective.get(path, "")), parse(path, after.get(path, ""))
        total = (lambda rows: sum(map(len, rows.values()))) if path == STRUCTURE else (lambda rows: sum(v[0] for v in rows.values()))
        if path == RUN_OWNERSHIP:
            axis = lambda rows, name: sum(v[0] for key, v in rows.items() if key[-1] == name)
            totals[path] = {"baseline": axis(was, "total"), "head": axis(now, "total"),
                            "bare_baseline": axis(was, "bare") if run_columns(effective.get(path, "")) == 3 else None,
                            "bare_head": axis(now, "bare") if run_columns(after.get(path, "")) == 3 else None}
        else:
            totals[path] = {"baseline": total(was), "head": total(now)}
    return {"base": base, "head": head, "paths": paths, "changes": changes,
            "growth": [f"{path}: {describe(change)}" for path, items in changes.items() for change in items],
            "markers": imports + markers, "notes": notes, "totals": totals,
            "unreviewed": sorted({path for path, _ in missing}),
            "unreviewed_changes": [f"{path}: {describe(change)}" for path, change in missing]}


def main(argv=None):
    import os
    parser = argparse.ArgumentParser(description=__doc__)
    baseline = parser.add_mutually_exclusive_group(required=True)
    baseline.add_argument("--base", help="explicit reviewed baseline, normally git merge-base origin/main HEAD")
    baseline.add_argument("--event-file", type=Path, help="GitHub payload; reads GITHUB_EVENT_NAME")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--repo", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    selection = {"base": args.base, "head": args.head, "reason": "explicit baseline"}
    if args.event_file:
        selection = event_range(args.repo, os.environ.get("GITHUB_EVENT_NAME", ""),
                                json.loads(args.event_file.read_text(encoding="utf-8")), args.head)
    print(f"baseline selection: {selection['reason']}", flush=True)
    report = check(args.repo, selection["base"], selection["head"])
    print(f"inventory comparison: {report['base']}..{report['head']}; {len(report['paths'])} inventories")
    for path in report["paths"]:
        problems = report["changes"].get(path, [])
        print(f"inventory {path}: " + ("review required" if problems else "no growth"))
        for problem in problems:
            print(f"  {describe(problem)}")
    for note in report["notes"]:
        print(note)
    for marker in report["markers"]:
        if marker["kind"] == "import":
            detail = f"{marker['path']}: import {marker['blob']}"
        elif marker["kind"] == "removed":
            detail = f"{marker['path']}: removal"
        elif marker["kind"] == "format":
            detail = f"{marker['path']}: format {marker['before_format']}->{marker['head_format']}"
        else:
            detail = f"{marker['path']} {key_text(marker['key'])} = {value_text(marker['head'])}"
        print(f"explicit Inventory-Growth marker {marker['commit']}: {detail}: {marker['reason']}")
    for path, counts in report["totals"].items():
        if path == RUN_OWNERSHIP:
            old_bare = "unknown" if counts["bare_baseline"] is None else counts["bare_baseline"]
            new_bare = "unknown" if counts["bare_head"] is None else counts["bare_head"]
            print(f"reference columns {path}: total {counts['baseline']} -> {counts['head']}; "
                  f"bare {old_bare} -> {new_bare}; columns never offset one another")
        else:
            print(f"reference totals {path}: {counts['baseline']} -> {counts['head']}; counts can cancel, key budgets decide")
    if report["unreviewed"]:
        print("inventory changes require a reviewed Inventory-Growth key/result (or explicit format review / named removal):\n"
              + "\n".join(report["unreviewed_changes"]), file=sys.stderr)
        return 1
    print("inventory comparison passed" + (" with exact-result review markers" if report["changes"] else "; no growth"))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (InventoryError, OSError, json.JSONDecodeError) as error:
        sys.exit(f"inventory comparison: {error}")
