#!/usr/bin/env python3
"""The structure ratchet's parsing and comparison, on constructed Clippy JSON (no Clippy run)."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("guard", Path(__file__).with_name("structure_guard.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)

PKG = {"path+file:///w/a#0.1.0": "steve-a"}


def message(lint, text, file="crates/steve-a/src/lib.rs", package="path+file:///w/a#0.1.0", test=False):
    return json.dumps({"reason": "compiler-message", "package_id": package, "profile": {"test": test},
                       "target": {"kind": ["lib"]}, "message": {
        "code": {"code": f"clippy::{lint}"}, "message": text,
        "spans": [{"is_primary": True, "file_name": file}]}})


DONE = json.dumps({"reason": "build-finished", "success": True})


class Parse(unittest.TestCase):
    def test_values_per_file_and_lint_largest_first(self):
        got = guard.parse([message("too_many_lines", "this function has too many lines (120/100)"),
                           message("too_many_lines", "this function has too many lines (300/100)"),
                           message("cognitive_complexity", "the function has a cognitive complexity of (30/25)"),
                           DONE], PKG)
        self.assertEqual(got, {("steve-a", "crates/steve-a/src/lib.rs", "too_many_lines"): [300, 120],
                               ("steve-a", "crates/steve-a/src/lib.rs", "cognitive_complexity"): [30]})

    def test_tests_other_packages_and_other_lints_are_left_out(self):
        got = guard.parse([message("too_many_lines", "too many lines (120/100)", file="crates/steve-a/tests/t.rs"),
                           message("too_many_lines", "too many lines (120/100)", package="registry+x"),
                           message("needless_return", "unneeded return"),
                           message("too_many_lines", "too many lines (120/100)", test=True), DONE], PKG)
        self.assertEqual(got, {})

    def test_an_unfinished_build_or_unknown_message_fails(self):
        with self.assertRaises(ValueError):
            guard.parse([message("too_many_lines", "too many lines (120/100)")], PKG)
        with self.assertRaises(ValueError):
            guard.parse([message("too_many_lines", "something else"), DONE], PKG)

    def test_failed_or_trailing_build_output_is_rejected(self):
        for lines in ([json.dumps({"reason": "build-finished", "success": False})],
                      [DONE, DONE], [DONE, message("too_many_lines", "too many lines (120/100)")]):
            with self.assertRaises(ValueError):
                guard.parse(lines, PKG)

    def test_real_cargo_messages_have_no_profile_and_test_artifacts_reject_a_mixed_stream(self):
        item = json.loads(message("too_many_lines", "too many lines (120/100)"))
        del item["profile"]
        self.assertEqual(next(iter(guard.parse([json.dumps(item), DONE], PKG).values())), [120])
        artifact = json.dumps({"reason": "compiler-artifact", "profile": {"test": True},
                               "target": {"kind": ["lib"]}})
        with self.assertRaisesRegex(ValueError, "test artifacts"):
            guard.parse([json.dumps(item), artifact, DONE], PKG)

    def test_values_form_a_multiset_and_line_positions_do_not_identify_functions(self):
        item = json.loads(message("too_many_lines", "too many lines (120/100)"))
        item["message"]["spans"][0]["line_start"] = 1
        moved = json.loads(json.dumps(item))
        moved["message"]["spans"][0]["line_start"] = 200
        self.assertEqual(guard.parse([json.dumps(item), DONE], PKG),
                         guard.parse([json.dumps(moved), DONE], PKG))
        values = guard.parse([json.dumps(item), json.dumps(moved), DONE], PKG)
        self.assertEqual(next(iter(values.values())), [120, 120])


class Compare(unittest.TestCase):
    K = ("steve-a", "f.rs", "too_many_lines")

    def test_same_is_clean(self):
        self.assertEqual(guard.compare({self.K: [300, 120]}, {self.K: [300, 120]}), ([], []))

    def test_more_entries_higher_values_or_new_keys_grow(self):
        for now in ([300, 120, 110], [301, 120], [300, 121]):
            self.assertTrue(guard.compare({self.K: [300, 120]}, {self.K: now})[0], now)
        self.assertTrue(guard.compare({}, {self.K: [101]})[0])

    def test_a_function_moving_within_a_file_is_not_growth(self):
        # one shorter, one gone: the multiset only shrank
        growth, shrink = guard.compare({self.K: [300, 120]}, {self.K: [250]})
        self.assertEqual(growth, [])
        self.assertTrue(shrink)

    def test_bless_writes_but_keeps_the_head_and_refuses_growth(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d, "structure.txt")
            path.write_text("# head\nsteve-a\tf.rs\ttoo_many_lines\t300,120\n")
            self.assertEqual(guard.read(path), {self.K: [300, 120]})
            for current in ({self.K: [301, 120]}, {self.K: [300, 120, 101]},
                            {("steve-a", "new.rs", "too_many_lines"): [101]}):
                before = path.read_text()
                self.assertTrue(guard.check(path, current, bless=True))
                self.assertEqual(path.read_text(), before)
            self.assertTrue(guard.check(path, {self.K: [250]}))
            self.assertFalse(guard.check(path, {self.K: [250]}, bless=True))
            self.assertEqual(path.read_text(), "# head\nsteve-a\tf.rs\ttoo_many_lines\t250\n")

    def test_missing_inventory_requires_explicit_initialization(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d, "structure.txt")
            with self.assertRaises(ValueError):
                guard.check(path, {self.K: [101]})
            self.assertFalse(path.exists())
            with self.assertRaisesRegex(ValueError, "missing inventory"):
                guard.check(path, {self.K: [999]}, bless=True)
            self.assertFalse(path.exists())
            self.assertFalse(guard.check(path, {self.K: [101]}, init=True))
            self.assertEqual(guard.read(path), {self.K: [101]})
            before = path.read_text()
            with self.assertRaisesRegex(ValueError, "INIT cannot overwrite"):
                guard.check(path, {self.K: [999]}, init=True)
            self.assertEqual(path.read_text(), before)
            path.unlink()
            with self.assertRaisesRegex(ValueError, "missing inventory"):
                guard.check(path, {self.K: [999]}, bless=True)
            self.assertFalse(path.exists())

    def test_init_and_bless_cannot_be_combined(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d, "structure.txt")
            with self.assertRaisesRegex(ValueError, "not both"):
                guard.check(path, {self.K: [101]}, bless=True, init=True)
            self.assertFalse(path.exists())

    def test_invalid_cli_modes_fail_before_starting_cargo(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d, "structure.txt")
            for bless, init, existing, problem in [("1", "0", False, "missing inventory"),
                                                    ("0", "1", True, "INIT cannot overwrite"),
                                                    ("1", "1", False, "not both"),
                                                    ("yes", "0", False, "missing inventory")]:
                if existing:
                    path.write_text("# fixture\n")
                elif path.exists():
                    path.unlink()
                with patch.object(guard, "LIST", path), patch.object(guard.sys, "argv", ["guard"]), \
                        patch.dict(guard.os.environ, {"STEVE_GUARD_BLESS": bless, "STEVE_GUARD_INIT": init}), \
                        patch.object(guard.subprocess, "run") as cargo:
                    with self.assertRaisesRegex(ValueError, problem):
                        guard.main()
                    cargo.assert_not_called()
                self.assertEqual(path.exists(), existing)

    def test_bad_baseline_cannot_be_blessed_away(self):
        with tempfile.TemporaryDirectory() as d:
            path = Path(d, "structure.txt")
            for body in ("300,120\nsteve-a\tf.rs\ttoo_many_lines\t300", "120,300", "0", "x"):
                text = f"steve-a\tf.rs\ttoo_many_lines\t{body}\n"
                path.write_text(text)
                with self.assertRaises(ValueError):
                    guard.check(path, {self.K: [101]}, bless=True)
                self.assertEqual(path.read_text(), text)


if __name__ == "__main__":
    unittest.main()
