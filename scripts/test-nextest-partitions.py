#!/usr/bin/env python3
"""Regression checks for incomplete, overlapping or accidentally widened CI shards."""
import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPT = Path(__file__).with_name("nextest-partitions.py")
spec = importlib.util.spec_from_file_location("partitions", SCRIPT)
partitions = importlib.util.module_from_spec(spec)
spec.loader.exec_module(partitions)


def case(ignored=False, selected=True):
    match = {"status": "matches"} if selected and not ignored else {
        "status": "mismatch", "reason": "ignored" if ignored else "partition"}
    return {"kind": "test", "ignored": ignored, "filter-match": match}


def listing():
    suites = {}
    for binary in ("pkg::first", "pkg::second", "pkg::empty"):
        suites[binary] = {"binary-id": binary, "status": "listed", "package-name": "pkg",
                          "binary-name": binary.split("::")[1], "kind": "test",
                          "build-platform": "target", "testcases": {}}
    suites["pkg::first"]["testcases"] = {"same_name": case(), "opt_in": case(ignored=True)}
    suites["pkg::second"]["testcases"] = {"same_name": case()}
    return {"test-count": 3, "rust-suites": suites}


class Tests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.full = listing()
        self.parts = [copy.deepcopy(self.full), copy.deepcopy(self.full)]
        self.parts[0]["rust-suites"]["pkg::second"]["testcases"]["same_name"] = case(selected=False)
        self.parts[1]["rust-suites"]["pkg::first"]["testcases"]["same_name"] = case(selected=False)

    def files(self):
        files = []
        for index, data in enumerate([self.full, *self.parts]):
            path = Path(self.temp.name) / f"{index}.json"
            path.write_text(json.dumps(data), encoding="utf-8")
            files.append(path)
        return files

    def verify(self):
        full, *parts = self.files()
        return partitions.verify(full, parts)

    def test_same_test_name_in_different_binaries_is_not_a_duplicate(self):
        self.assertEqual(self.verify(), dict(coverage="exactly-once", binaries=3, total=3,
                                            ignored=1, selected=2, partitions=[1, 1]))

    def test_duplicate_and_missing_cancelled_counts_are_rejected(self):
        self.parts[1] = copy.deepcopy(self.parts[0])
        with self.assertRaisesRegex(partitions.Invalid, r"missing=1.*duplicated=1"):
            self.verify()

    def test_missing_test_inventory_is_rejected_even_when_unselected(self):
        del self.parts[0]["rust-suites"]["pkg::second"]["testcases"]["same_name"]
        self.parts[0]["test-count"] = 2
        with self.assertRaisesRegex(partitions.Invalid, "test inventory"):
            self.verify()

    def test_missing_empty_binary_is_rejected(self):
        del self.parts[0]["rust-suites"]["pkg::empty"]
        with self.assertRaisesRegex(partitions.Invalid, "binary inventory"):
            self.verify()

    def test_filtered_full_list_cannot_hide_missing_coverage(self):
        self.full["rust-suites"]["pkg::first"]["testcases"]["same_name"] = case(selected=False)
        with self.assertRaisesRegex(partitions.Invalid, "unexpected filter"):
            self.verify()

    def test_ignored_test_cannot_be_enabled_or_reclassified(self):
        for altered in [case(ignored=False), {**case(ignored=True), "filter-match": {"status": "matches"}}]:
            with self.subTest(altered=altered):
                self.parts[0]["rust-suites"]["pkg::first"]["testcases"]["opt_in"] = altered
                with self.assertRaises(partitions.Invalid):
                    self.verify()

    def test_malformed_or_incomplete_list_fails_closed(self):
        for change in [lambda data: data.update({"test-count": 99}),
                       lambda data: data["rust-suites"]["pkg::first"].update({"status": "skipped"}),
                       lambda data: data["rust-suites"]["pkg::first"]["testcases"]["same_name"].pop("filter-match")]:
            with self.subTest(change=change):
                original = copy.deepcopy(self.parts[0])
                change(self.parts[0])
                with self.assertRaises(partitions.Invalid):
                    self.verify()
                self.parts[0] = original

    def test_duplicate_json_keys_are_not_silently_overwritten(self):
        full, *parts = self.files()
        full.write_text('{"rust-suites":{},"rust-suites":{}}', encoding="utf-8")
        with self.assertRaisesRegex(partitions.Invalid, "duplicate JSON key"):
            partitions.verify(full, parts)

    def test_command_failure_propagates_and_success_reports_counts(self):
        full, *parts = self.files()
        result = subprocess.run([sys.executable, str(SCRIPT), str(full), *map(str, parts)],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["partitions"], [1, 1])
        result = subprocess.run([sys.executable, str(SCRIPT), str(full), str(parts[0])],
                                capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing=1", result.stderr)

    def report(self, index, cases, issue=None):
        root = ET.Element("testsuites", tests=str(len(cases)), failures="0", errors="0", skipped="0")
        for binary, name in cases:
            child = ET.SubElement(root, "testcase", classname=binary, name=name)
            if issue:
                ET.SubElement(child, issue)
        path = Path(self.temp.name) / f"junit-{index}.xml"
        ET.ElementTree(root).write(path)
        return path

    def test_junit_proves_actual_execution_and_rejects_incomplete_or_extra_cases(self):
        full, *parts = self.files()
        first = ("pkg::first", "same_name")
        second = ("pkg::second", "same_name")
        reports = [self.report(0, [first]), self.report(1, [second])]
        self.assertEqual(partitions.verify(full, parts, reports)["execution"],
                         "all-selected-passed-first-try")
        for actual in [[], [first, first], [second], [first, second]]:
            with self.subTest(actual=actual):
                reports[0] = self.report(0, actual)
                with self.assertRaises(partitions.Invalid):
                    partitions.verify(full, parts, reports)
        with self.assertRaisesRegex(partitions.Invalid, "one --junit"):
            partitions.verify(full, parts, reports[:1])

    def test_one_unpartitioned_run_is_checked_against_its_own_full_list(self):
        full = self.files()[0]
        first = ("pkg::first", "same_name")
        second = ("pkg::second", "same_name")
        result = partitions.verify(full, [full], [self.report(0, [first, second])])
        self.assertEqual((result["partitions"], result["execution"]),
                         ([2], "all-selected-passed-first-try"))
        for actual in [[first], [first, second, ("pkg::first", "opt_in")]]:
            with self.subTest(actual=actual):
                with self.assertRaisesRegex(partitions.Invalid, "differs from selection"):
                    partitions.verify(full, [full], [self.report(0, actual)])

    def test_junit_rejects_failure_skip_and_retry_history(self):
        for issue in ["failure", "error", "skipped", "flakyFailure", "flakyError", "rerunFailure", "rerunError"]:
            with self.subTest(issue=issue):
                key = ("pkg::first", "same_name")
                report = self.report(0, [key], issue)
                with self.assertRaisesRegex(partitions.Invalid, "first try"):
                    partitions.verify_junit(report, {key})


if __name__ == "__main__":
    unittest.main()
