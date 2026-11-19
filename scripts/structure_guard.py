#!/usr/bin/env python3
"""Structure ratchet: per (crate, file, lint), the Clippy values of over-limit production functions.

A key's entry count must not grow and its values, sorted from the largest, must not rise; a new key
fails. Shrinking fails too until hub-rs/structure.txt is rewritten with --bless, which refuses any
growth (allowing growth means editing the file by hand, in review). STEVE_GUARD_BLESS=1 is the
environment equivalent of --bless. Each run checks all workspace targets before measuring production
lib/bin targets. --json replays a production-only Cargo JSON stream.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
WORKSPACE = ROOT / "hub-rs"
LIST = WORKSPACE / "structure.txt"
HEADER = "# Production Clippy structure inventory; only shrinks.\n# crate<TAB>workspace-relative file<TAB>lint<TAB>values, sorted largest first\n# Thresholds: too_many_lines 100, cognitive_complexity 25.\n"
VALUE = {
    "too_many_lines": re.compile(r"too many lines \((\d+)/\d+\)"),
    "cognitive_complexity": re.compile(r"cognitive complexity of \((\d+)/\d+\)"),
}
# compiler-message has no profile.test in real Cargo JSON. Check all targets separately, then
# collect only production lib/bin diagnostics; mixing lib and lib-test messages cannot be disambiguated.
CHECK = ["cargo", "check", "--workspace", "--all-targets", "--locked"]
# clippy::all includes deny-by-default lints too; force-warn keeps only these two metrics active.
CLIPPY = ["cargo", "clippy", "--workspace", "--lib", "--bins", "--no-deps", "--locked", "--message-format=json",
          "--", "-A", "warnings", "-A", "clippy::all",
          "--force-warn", "clippy::too_many_lines", "--force-warn", "clippy::cognitive_complexity"]


def parse(lines, packages):
    """{(crate, file, lint): [values, largest first]} from a successful Cargo JSON stream."""
    found, finished = {}, []
    for line in lines:
        item = json.loads(line)
        if finished:
            raise ValueError("unexpected message after build-finished")
        if item.get("reason") == "build-finished":
            finished.append(item["success"])
        if item.get("reason") == "compiler-artifact" and item.get("profile", {}).get("test"):
            raise ValueError("test artifacts in metric stream; collect Clippy with --lib --bins")
        if item.get("reason") != "compiler-message" or item.get("package_id") not in packages:
            continue
        if item.get("profile", {}).get("test") or set(item.get("target", {}).get("kind", [])) & {"test", "bench"}:
            continue
        message = item["message"]
        lint = ((message.get("code") or {}).get("code") or "").removeprefix("clippy::")
        if lint not in VALUE:
            continue
        span = next((s for s in message["spans"] if s["is_primary"]), None)
        if span is None:
            raise ValueError(f"missing primary span for {lint}")
        if {"tests", "benches"} & set(Path(span["file_name"]).parts):
            continue
        file = Path(span["file_name"])
        if file.is_absolute():
            file = file.relative_to(WORKSPACE)
        if ".." in file.parts:
            raise ValueError(f"source outside workspace: {file}")
        value = VALUE[lint].search(message["message"])
        if not value:
            raise ValueError(f"unknown {lint} message: {message['message']}")
        key = (packages[item["package_id"]], file.as_posix(), lint)
        found.setdefault(key, []).append(int(value[1]))
    if finished != [True]:
        raise ValueError("Clippy did not finish successfully")
    return {key: sorted(values, reverse=True) for key, values in found.items()}


def read(path):
    listed = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        if line and not line.startswith("#"):
            crate, file, lint, values = line.split("\t")
            key = (crate, file, lint)
            numbers = [int(v) for v in values.split(",")]
            if (key in listed or lint not in VALUE or not crate or not file
                    or numbers != sorted(numbers, reverse=True) or any(v <= 0 for v in numbers)):
                raise ValueError(f"invalid inventory row: {line}")
            listed[key] = numbers
    return listed


def compare(listed, current):
    """(growth, shrink) messages."""
    growth, shrink = [], []
    for key in sorted(set(listed) | set(current)):
        was, now = listed.get(key, []), current.get(key, [])
        where = " ".join(key)
        if len(now) > len(was) or any(n > w for n, w in zip(now, was)):
            growth.append(f"{where}: {now} grew past {was}")
        elif now != was:
            shrink.append(f"{where}: {now} is below {was}; rewrite the list with --bless")
    return growth, shrink


def write(path, current):
    head = [line for line in (path.read_text(encoding="utf-8") if path.exists() else HEADER).splitlines()
            if line.startswith("#")]
    body = [f"{c}\t{f}\t{l}\t{','.join(map(str, v))}" for (c, f, l), v in sorted(current.items())]
    path.write_text("\n".join(head + body) + "\n", encoding="utf-8")


def check(path, current, bless=False):
    if not path.exists():
        if not bless:
            raise ValueError(f"missing inventory {path}; initialize explicitly with STEVE_GUARD_BLESS=1")
        write(path, current)
        return []
    growth, shrink = compare(read(path), current)
    if bless and not growth:
        write(path, current)
        return []
    return growth + shrink


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bless", action="store_true")
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()
    meta = json.loads(subprocess.run(["cargo", "metadata", "--no-deps", "--offline", "--format-version", "1"],
                                     cwd=WORKSPACE, check=True, capture_output=True, text=True).stdout)
    packages = {p["id"]: p["name"] for p in meta["packages"] if p["id"] in meta["workspace_members"]}
    if args.json:
        current = parse(args.json.open(encoding="utf-8"), packages)
    else:
        subprocess.run(CHECK, cwd=WORKSPACE, check=True)
        with tempfile.TemporaryFile("w+", encoding="utf-8") as out:
            result = subprocess.run(CLIPPY, cwd=WORKSPACE, stdout=out)
            out.seek(0)
            if result.returncode:
                for line in out:
                    item = json.loads(line)
                    message = item.get("message", {})
                    if item.get("reason") == "compiler-message" and message.get("level") == "error":
                        print(message.get("rendered") or message["message"], file=sys.stderr)
                result.check_returncode()
            out.seek(0)
            current = parse(out, packages)
    for lint in VALUE:
        values = [v for (_, _, l), vs in current.items() if l == lint for v in vs]
        print(f"{lint}: {len(values)} functions over the limit")
    problems = check(LIST, current, args.bless or os.environ.get("STEVE_GUARD_BLESS") == "1")
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"structure: {error}")
