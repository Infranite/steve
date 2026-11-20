#!/usr/bin/env python3
"""Print first-import inventory trailers for Git snapshots recognized by RG4.

RG4 uses the Inventory-Growth trailer key for BOTH imports and budget increases.
This tool emits only its `path: import <40-character blob>: reason` form, never
key/result growth or removal authorization. Existing growth requires a human.
Only committed objects are read; no branch, index, worktree or commit is changed.
"""
import argparse
import importlib.util
from pathlib import Path
import re
import sys

spec = importlib.util.spec_from_file_location(
    "inventory_import_comparator", Path(__file__).with_name("check-inventory-growth.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def single_line_reason(reason):
    if not reason.strip() or any(ord(c) < 32 or ord(c) == 127 or c in "\u2028\u2029" for c in reason):
        raise guard.InventoryError("import reason must be a nonempty single line without control characters")
    return reason.strip()


def head_blob(repo, head, path):
    # Use the same literal pathspec helper as RG4; a path is never shell input.
    entry = guard.git(repo, "ls-tree", "-z", head, "--", path)
    fields = entry.partition("\t")[0].split()
    if len(fields) != 3 or fields[1] != "blob" or not re.fullmatch(r"[0-9a-f]{40}", fields[2]):
        raise guard.InventoryError(f"{path}: RG4 imports require a full 40-character blob id")
    return fields[2]


def generate(repo, base, head="HEAD", reason="initial inventory snapshot"):
    reason = single_line_reason(reason)
    base, head = guard.resolve(repo, base), guard.resolve(repo, head)
    before, after = guard.snapshot(repo, base), guard.snapshot(repo, head)
    _, preliminary = guard.compare_snapshots(before, after)
    existing = guard.growth_markers(repo, base, head) if preliminary else []
    effective, imports, notes = guard.import_baselines(repo, base, head, before, after, existing)
    _, changes = guard.compare_snapshots(effective, after)
    anchored = {item["path"]: item["blob"] for item in imports}
    issues = []
    for path, problems in changes.items():
        if path in before or path in anchored:
            for problem in problems:
                label = "清单被删除，需要人写理由" if problem["kind"] == "removed" else "有增长，需要人写理由"
                issues.append(f"{path}: {label}: {guard.describe(problem)}")
    # Atomic output: even a mixed snapshot (new imports + existing growth) prints NO trailers.
    if issues:
        return {"base": base, "head": head, "markers": [], "issues": issues, "notes": notes}
    markers = []
    for path in sorted(after.keys() - before.keys()):
        if not guard.parse(path, after[path]):  # An empty first snapshot needs no budget import.
            continue
        if not guard.valid_path(path) or not re.fullmatch(r"\S+", path):
            raise guard.InventoryError(f"{path!r}: path cannot be represented by RG4's import syntax")
        # Never replace an earlier reviewed import with a later, potentially larger blob.
        blob = anchored.get(path) or head_blob(repo, head, path)
        if blob not in guard.path_history_blobs(repo, base, head, path):
            raise guard.InventoryError(f"{path}: import blob is not in this path's comparison history")
        markers.append(f"Inventory-Growth: {path}: import {blob}: {reason}")
    return {"base": base, "head": head, "markers": markers, "issues": [], "notes": notes}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base", required=True, help="baseline commit/ref; existing files cannot be imported")
    parser.add_argument("--head", default="HEAD", help="committed snapshot to inspect (default: HEAD)")
    parser.add_argument("--repo", type=Path, default=guard.ROOT)
    parser.add_argument("--reason", default="initial inventory snapshot", help="single-line import description; not a growth approval")
    args = parser.parse_args(argv)
    try:
        report = generate(args.repo, args.base, args.head, args.reason)
    except (guard.InventoryError, OSError) as error:
        print(f"inventory imports: {error}", file=sys.stderr)
        return 2
    for note in report["notes"]:
        print(note, file=sys.stderr)
    for issue in report["issues"]:
        print(issue, file=sys.stderr)
    if report["issues"]:
        return 1
    for marker in report["markers"]:
        print(marker)
    return 0


if __name__ == "__main__":
    sys.exit(main())
