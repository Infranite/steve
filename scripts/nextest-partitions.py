#!/usr/bin/env python3
"""Prove that nextest's actual partition lists cover the default suite exactly once.

    python3 scripts/nextest-partitions.py full.json shard-1.json shard-2.json ...

Inputs are `cargo nextest list --message-format json`, with identical build/profile
options and only `--partition hash:M/N` added for each shard. The full list must
select every non-ignored test. Ignored tests remain present but unselected in every
list; this preserves the ordinary CI policy without enabling opt-in integrations.

Test identity is (binary-id, test-name), never the test name alone. We check the
complete inventory as well as selected tests, so a missing binary or an accidentally
filtered baseline cannot make an incomplete partition look complete. No hash algorithm
is duplicated here: nextest remains the authority for assigning tests to partitions.
"""
import argparse
from collections import Counter
import json
from pathlib import Path
import sys
import xml.etree.ElementTree as ET


class Invalid(ValueError):
    pass


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise Invalid(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def read(path, *, full=False):
    data = json.loads(Path(path).read_text(encoding="utf-8"), object_pairs_hook=unique_object)
    if not isinstance(data, dict) or not isinstance(data.get("rust-suites"), dict):
        raise Invalid(f"{path}: expected nextest full JSON list (rust-suites)")
    suites, cases, selected = {}, {}, set()
    for binary, suite in data["rust-suites"].items():
        if (not isinstance(suite, dict) or suite.get("binary-id") != binary
                or suite.get("status") != "listed" or not isinstance(suite.get("testcases"), dict)):
            raise Invalid(f"{path}: incomplete or inconsistent binary {binary}")
        suites[binary] = tuple(suite.get(key) for key in
                               ("package-name", "binary-name", "kind", "build-platform"))
        for name, case in suite["testcases"].items():
            key = (binary, name)
            if not isinstance(case, dict) or type(case.get("ignored")) is not bool:
                raise Invalid(f"{path}: invalid test metadata for {key}")
            match = case.get("filter-match")
            if not isinstance(match, dict):
                raise Invalid(f"{path}: missing filter-match for {key}")
            ignored = case["ignored"]
            status, reason = match.get("status"), match.get("reason")
            if ignored:
                valid = status == "mismatch" and reason == "ignored"
            else:
                valid = status == "matches" or (
                    not full and status == "mismatch" and reason == "partition")
            if not valid:
                raise Invalid(f"{path}: unexpected filter for {key}: {match}")
            cases[key] = (case.get("kind"), ignored)
            if status == "matches":
                selected.add(key)
    if type(data.get("test-count")) is not int or data["test-count"] != len(cases):
        raise Invalid(f"{path}: test-count does not match the complete inventory")
    return suites, cases, selected


def sample(keys):
    return ", ".join(f"{binary} {name}" for binary, name in sorted(keys)[:5])


def verify_junit(path, expected):
    """Nextest 0.9.151 emits only executed tests, keyed by classname=binary-id."""
    root = ET.parse(path).getroot()
    if root.tag != "testsuites":
        raise Invalid(f"{path}: expected nextest testsuites report")
    seen = set()
    for case in root.iter("testcase"):
        key = (case.get("classname"), case.get("name"))
        if not all(key) or key in seen:
            raise Invalid(f"{path}: missing or duplicate JUnit identity {key}")
        seen.add(key)
        # A skipped case, final failure, or retry history is not a first-try pass.
        if any(child.tag in {"failure", "error", "skipped", "flakyFailure", "flakyError",
                             "rerunFailure", "rerunError"} for child in case.iter()):
            raise Invalid(f"{path}: JUnit case did not pass on its first try: {key}")
    if root.get("tests") != str(len(seen)):
        raise Invalid(f"{path}: JUnit test count does not match its cases")
    if any(root.get(key) != "0" for key in ("failures", "errors", "skipped")):
        raise Invalid(f"{path}: JUnit reports failures, errors or skipped tests")
    if seen != expected:
        raise Invalid(f"{path}: JUnit execution differs from selection: "
                      f"missing={len(expected - seen)}; extra={len(seen - expected)}")


def verify(full_path, shard_paths, junit_paths=None):
    if not shard_paths:
        raise Invalid("provide at least one shard list")
    if junit_paths is not None and len(junit_paths) != len(shard_paths):
        raise Invalid("provide one --junit report per shard, in the same order")
    suites, cases, expected = read(full_path, full=True)
    if not expected:
        raise Invalid("the full list selects no tests")
    seen, counts = Counter(), []
    for index, path in enumerate(shard_paths):
        shard_suites, shard_cases, selected = read(path)
        if shard_suites != suites:
            raise Invalid(f"{path}: binary inventory differs from the full list")
        if shard_cases != cases:
            raise Invalid(f"{path}: test inventory or ignored metadata differs from the full list")
        seen.update(selected)
        counts.append(len(selected))
        if junit_paths is not None:
            verify_junit(junit_paths[index], selected)
    missing = expected - seen.keys()
    duplicate = {key for key, count in seen.items() if count != 1}
    if missing or duplicate:
        raise Invalid(f"partition coverage failed: missing={len(missing)} [{sample(missing)}]; "
                      f"duplicated={len(duplicate)} [{sample(duplicate)}]")
    result = dict(coverage="exactly-once", binaries=len(suites), total=len(cases),
                  ignored=len(cases) - len(expected), selected=len(expected), partitions=counts)
    if junit_paths is not None:
        result["execution"] = "all-selected-passed-first-try"
    return result


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("full")
    parser.add_argument("shards", nargs="+")
    parser.add_argument("--junit", action="append", help="one actual report per shard, in shard order")
    args = parser.parse_args(argv)
    try:
        result = verify(args.full, args.shards, args.junit)
    except (OSError, ValueError, ET.ParseError) as error:
        print(f"nextest partition validation failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
