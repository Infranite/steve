#!/usr/bin/env python3
"""Reject new or growing inventory keys against a Git baseline.

An Inventory-Growth: <reason> Git trailer in base..head records an explicit reviewed exception.
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
I18N_REMAINING = f"{HUB_TESTS}/i18n_remaining.txt"
WEB = {"web/test/guard-zh-compare.txt", "web/test/harness-parity.txt"}


class InventoryError(ValueError):
    pass


def is_inventory(path):
    file = PurePosixPath(path)
    parent = file.parent.as_posix()
    return (path == STRUCTURE or path in WEB
            or (parent == HUB_TESTS and file.suffix == ".txt" and file.name != "layering.txt")
            or (parent == SQLITE_TESTS and file.name.startswith("inventory_") and file.suffix == ".txt")
            or (parent == SCHEMA and (file.name == "triggers.tsv"
                                     or (file.name.startswith("debt_") and file.suffix == ".txt"))))


def positive(text):
    if not re.fullmatch(r"[0-9]+", text) or int(text) <= 0:
        raise InventoryError(f"expected a positive decimal integer, found {text!r}")
    return int(text)


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
            if path == STRUCTURE:
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


def compare_snapshots(before, after):
    paths = sorted(path for path in before.keys() | after.keys() if is_inventory(path))
    growth = []
    for path in paths:
        problems, _ = compare(parse(path, before.get(path, "")), parse(path, after.get(path, "")))
        growth.extend(f"{path}: {problem}" for problem in problems)
    return paths, growth


def git(repo, *args, input_text=None):
    result = subprocess.run(["git", "-C", str(repo), *args], capture_output=True,
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
        for line in trailers.splitlines():
            key, separator, reason = line.partition(":")
            if separator and key.casefold() == "inventory-growth" and reason.strip():
                markers.append((commit, reason.strip()))
    return markers


def event_base(name, event):
    if not isinstance(event, dict):
        raise InventoryError("GitHub event payload must be an object")
    if name == "push":
        return "HEAD^"
    if name == "pull_request":
        keys = ("pull_request", "base", "sha")
    elif name == "merge_group":
        keys = ("merge_group", "base_sha")
    else:
        raise InventoryError(f"unsupported GitHub event {name!r}; use --base for local checks")
    base = event
    for key in keys:
        base = base.get(key) if isinstance(base, dict) else None
    if not isinstance(base, str) or not re.fullmatch(r"[0-9a-fA-F]{40}|[0-9a-fA-F]{64}", base):
        raise InventoryError(f"{name} has no valid target-branch base SHA")
    return base


def check(repo, base, head="HEAD"):
    base, head = resolve(repo, base), resolve(repo, head)
    paths, growth = compare_snapshots(snapshot(repo, base), snapshot(repo, head))
    return {"base": base, "head": head, "paths": paths, "growth": growth,
            "markers": growth_markers(repo, base, head) if growth else []}


def main(argv=None):
    import os
    parser = argparse.ArgumentParser(description=__doc__)
    baseline = parser.add_mutually_exclusive_group(required=True)
    baseline.add_argument("--base", help="target branch/commit; push checks use HEAD^")
    baseline.add_argument("--event-file", type=Path, help="GitHub payload; reads GITHUB_EVENT_NAME")
    parser.add_argument("--head", default="HEAD")
    parser.add_argument("--repo", type=Path, default=ROOT)
    args = parser.parse_args(argv)
    base = args.base
    if args.event_file:
        base = event_base(os.environ.get("GITHUB_EVENT_NAME", ""),
                          json.loads(args.event_file.read_text(encoding="utf-8")))
    report = check(args.repo, base, args.head)
    print(f"inventory comparison: {report['base']}..{report['head']}; {len(report['paths'])} inventories")
    for problem in report["growth"]:
        print(problem)
    if report["growth"] and not report["markers"]:
        print("inventory growth requires a reviewed Inventory-Growth: <reason> commit trailer in this comparison range", file=sys.stderr)
        return 1
    for commit, reason in report["markers"]:
        print(f"explicit Inventory-Growth marker {commit}: {reason}")
    print("inventory comparison passed" + (" with an explicit growth exception" if report["growth"] else "; no growth"))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (InventoryError, OSError, json.JSONDecodeError) as error:
        sys.exit(f"inventory comparison: {error}")
