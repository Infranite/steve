#!/usr/bin/env python3
"""The Rust tests' job summary: a retried pass is listed as flaky, a test that never passed as failed."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("nextest_summary", Path(__file__).with_name("nextest-summary.py"))
summary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(summary)

# What cargo-nextest 0.9.151 writes with `--retries 2` (outputs trimmed): one first-try pass, one
# pass on the second try, one failure on all three.
REPORT = """<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" skipped="0" failures="1" errors="0">
    <testsuite name="demo" tests="3" skipped="0" errors="0" failures="1">
        <testcase name="t::ok" classname="demo" time="0.017"/>
        <testcase name="t::flaky" classname="demo" time="0.017">
            <flakyFailure time="0.017" message="thread &apos;t::flaky&apos; panicked at src/lib.rs:10:59" type="test failure with exit code 101">thread &apos;t::flaky&apos; panicked at src/lib.rs:10:59:
first try fails
                <system-out>running 1 test</system-out>
            </flakyFailure>
        </testcase>
        <testcase name="t::broken" classname="demo" time="0.017">
            <failure message="thread &apos;t::broken&apos; panicked at src/lib.rs:13:27" type="test failure with exit code 101">always</failure>
            <rerunFailure time="0.023" message="thread &apos;t::broken&apos; panicked at src/lib.rs:13:27 | again" type="test failure with exit code 101">always</rerunFailure>
            <rerunFailure time="0.014" message="thread &apos;t::broken&apos; panicked at src/lib.rs:13:27" type="test failure with exit code 101">always</rerunFailure>
        </testcase>
    </testsuite>
</testsuites>
"""

CLEAN = """<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="1"><testsuite name="demo" tests="1">
<testcase name="t::ok" classname="demo" time="0.017"/></testsuite></testsuites>
"""


class SummaryTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)

    def write(self, text):
        p = self.dir / "junit.xml"
        p.write_text(text)
        return str(p)

    def run_main(self, path, env):
        out = io.StringIO()
        with mock.patch.dict(os.environ, env, clear=True), contextlib.redirect_stdout(out):
            code = summary.main(["nextest-summary.py", path])
        return code, out.getvalue()

    def test_flaky_and_failed_are_told_apart(self):
        flaky, failed = summary.read(self.write(REPORT))
        self.assertEqual([c.name for c in flaky], ["demo t::flaky"])
        self.assertEqual(len(flaky[0].failures), 1)
        self.assertEqual([c.name for c in failed], ["demo t::broken"])
        self.assertEqual(len(failed[0].failures), 3)

    def test_the_step_summary_lists_every_retried_pass_and_annotates_it(self):
        sink = self.dir / "summary.md"
        code, out = self.run_main(self.write(REPORT), {
            "GITHUB_ACTIONS": "true", "RUNNER_OS": "Linux", "GITHUB_STEP_SUMMARY": str(sink)})
        self.assertEqual(code, 0)
        text = sink.read_text()
        self.assertIn("### Rust tests (Linux)", text)
        self.assertIn("1 passed only on retry", text)
        self.assertIn("| `demo t::flaky` | 1 |", text)
        self.assertIn("1 failed every try", text)
        self.assertIn("| `demo t::broken` | 3 |", text)
        self.assertIn("::warning title=Flaky test::demo t::flaky passed only after 1 failed try", out)
        self.assertNotIn("::warning title=Flaky test::demo t::broken", out)

    def test_a_clean_run_says_so(self):
        code, out = self.run_main(self.write(CLEAN), {})
        self.assertEqual(code, 0)
        self.assertIn("Every test passed on its first try.", out)
        self.assertNotIn("::warning", out)

    def test_a_missing_report_is_an_error(self):
        sink = self.dir / "summary.md"
        code, _ = self.run_main(str(self.dir / "absent.xml"), {"GITHUB_STEP_SUMMARY": str(sink)})
        self.assertEqual(code, 2)
        self.assertIn("No nextest report", sink.read_text())


if __name__ == "__main__":
    unittest.main()
