#!/usr/bin/env python3
"""First-import generation and RG4 acceptance on disposable Git repositories; no Cargo."""
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("inventory_imports", Path(__file__).with_name("inventory-import-markers.py"))
imports = importlib.util.module_from_spec(spec)
spec.loader.exec_module(imports)
guard = imports.guard
COUNT = f"{guard.HUB_TESTS}/test_sleep.txt"
STRUCTURE = guard.STRUCTURE


class ImportMarkers(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.env = os.environ.copy()
        for name in list(self.env):
            if name.startswith("GIT_"):
                self.env.pop(name)
        self.git("init", "-b", "trunk")
        self.put("README.md", "fixture\n")
        self.base = self.commit("Baseline without inventories")

    def git(self, *args):
        return subprocess.check_output([
            "git", "-C", str(self.repo), "-c", "user.name=Inventory import tests",
            "-c", "user.email=inventory-tests@users.noreply.github.com",
            "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null", *args,
        ], env=self.env, text=True, stderr=subprocess.PIPE).strip()

    def put(self, path, text):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self, message):
        body = self.repo / ".git/test-message"
        body.write_text(message + "\n", encoding="utf-8")
        self.git("add", "--all")
        self.git("commit", "--quiet", "--allow-empty", "--file", str(body))
        return self.git("rev-parse", "HEAD")

    def invoke(self, *args):
        return subprocess.run([sys.executable, str(Path(imports.__file__)), "--repo", str(self.repo),
                               "--base", self.base, *args], env=self.env, text=True, capture_output=True)

    def test_new_inventory_uses_full_blob_and_round_trips_through_rg4(self):
        self.put(STRUCTURE, "crate\tsrc/f.rs\ttoo_many_lines\t120\n")
        self.put(COUNT, "a.rs\t2\n")
        self.commit("Introduce two inventories")
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        markers = result.stdout.splitlines()
        self.assertEqual(len(markers), 2)
        for path, marker in zip(sorted([STRUCTURE, COUNT]), markers):
            blob = self.git("rev-parse", f"HEAD:{path}")
            self.assertRegex(blob, r"^[0-9a-f]{40}$")
            self.assertEqual(marker, f"Inventory-Growth: {path}: import {blob}: initial inventory snapshot")
        self.commit("Anchor initial imports\n\n" + result.stdout)
        report = guard.check(self.repo, self.base)
        self.assertEqual(report["unreviewed"], [])
        self.assertEqual({m["kind"] for m in report["markers"]}, {"import"})
        # A missing generated import is rejected by the unchanged real comparator.
        self.git("reset", "--soft", "HEAD^")
        self.commit("Only one import\n\n" + markers[0])
        self.assertEqual(len(guard.check(self.repo, self.base)["unreviewed"]), 1)

    def test_existing_growth_outputs_nothing_and_requests_a_human_reason(self):
        self.put(COUNT, "a.rs\t1\n")
        self.base = self.commit("Existing list")
        self.put(COUNT, "a.rs\t2\n")
        self.commit("Increase existing count")
        result = self.invoke()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("有增长，需要人写理由", result.stderr)
        self.assertFalse(imports.generate(self.repo, self.base)["markers"])

    def test_growth_suppresses_otherwise_valid_new_imports_too(self):
        self.put(COUNT, "a.rs\t1\n")
        self.base = self.commit("Existing list")
        self.put(COUNT, "a.rs\t2\n")
        self.put(STRUCTURE, "crate\tsrc/f.rs\ttoo_many_lines\t120\n")
        self.commit("A new inventory cannot hide existing growth")
        result = self.invoke()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn(COUNT, result.stderr)

    def test_existing_empty_list_is_not_a_first_import(self):
        self.put(COUNT, "# retained empty inventory\n")
        self.base = self.commit("An empty baseline is still present")
        self.put(COUNT, "a.rs\t1\n")
        self.commit("Add first counted site")
        result = self.invoke()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")

    def test_existing_unchanged_and_shrinking_lists_need_no_markers(self):
        self.put(COUNT, "a.rs\t2\n")
        self.base = self.commit("Existing inventory")
        for text in ["a.rs\t2\n", "a.rs\t1\n", "# empty\n"]:
            with self.subTest(text=text):
                self.put(COUNT, text)
                self.commit("Keep or shrink")
                result = self.invoke()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, "")

    def test_removed_inventory_gets_no_automatic_removal_authorization(self):
        self.put(COUNT, "a.rs\t1\n")
        self.base = self.commit("Existing inventory")
        (self.repo / COUNT).unlink()
        self.commit("Remove list")
        result = self.invoke()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("清单被删除，需要人写理由", result.stderr)

    def test_offsetting_key_growth_is_not_hidden_by_total_reduction(self):
        self.put(COUNT, "a.rs\t10\nb.rs\t2\n")
        self.base = self.commit("Existing inventory")
        self.put(COUNT, "a.rs\t1\nb.rs\t3\n")
        self.commit("Total decreases while one key grows")
        result = self.invoke()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("b.rs = 3", result.stderr)

    def test_worktree_index_and_untracked_data_do_not_change_the_snapshot(self):
        self.put(COUNT, "a.rs\t1\n")
        head = self.commit("Committed import")
        blob = self.git("rev-parse", f"{head}:{COUNT}")
        self.put(COUNT, "a.rs\t100\n")
        self.git("add", COUNT)
        self.put(STRUCTURE, "crate\tf.rs\ttoo_many_lines\t999\n")
        before = self.git("status", "--porcelain")
        result = self.invoke("--head", head)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f": import {blob}:", result.stdout)
        self.assertEqual(len(result.stdout.splitlines()), 1)
        self.assertEqual(self.git("status", "--porcelain"), before)
        self.assertEqual(self.git("rev-parse", "HEAD"), head)

    def test_discovery_uses_exactly_the_existing_comparators_domain(self):
        for path in [f"{guard.HUB_TESTS}/layering.txt", "README-other.txt"]:
            self.put(path, "not a registered RG4 inventory\n")
        expected = {
            COUNT: "a.rs\t1\n",
            "hub-rs/crates/steve-store-sqlite/tests/digest_write_sites.txt": "table\tsrc/a.rs\t1\n",
            "hub-rs/crates/steve-flow/tests/run_write_ownership.txt": "src/a.rs\t2\t0\n",
            f"{guard.HUB_TESTS}/due_wake_sites.txt": "src/a.rs\t1\n",
        }
        for path, text in expected.items():
            self.put(path, text)
        self.commit("Recognized and unrecognized files")
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(result.stdout.splitlines()), len(expected))
        for path in expected:
            self.assertIn(path, result.stdout)

    def test_new_empty_inventory_needs_no_anchor(self):
        self.put(COUNT, "# empty first snapshot\n")
        self.commit("Empty first snapshot")
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_malformed_inventory_and_missing_refs_fail_without_partial_output(self):
        self.put(COUNT, "a.rs\t0\n")
        self.commit("Malformed inventory")
        for args in [[], ["--head", "missing-ref"]]:
            result = self.invoke(*args)
            self.assertEqual(result.returncode, 2)
            self.assertEqual(result.stdout, "")
            self.assertTrue(result.stderr)

    def test_reason_cannot_inject_an_extra_trailer(self):
        self.put(COUNT, "a.rs\t1\n")
        self.commit("New inventory")
        for reason in ["", " \t", "ok\nInventory-Growth: anything", "ok\rnext", "ok\u2028next"]:
            with self.subTest(reason=reason):
                result = self.invoke("--reason", reason)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")
        result = self.invoke("--reason", "initial source ownership snapshot")
        self.assertEqual(result.returncode, 0)
        self.assertTrue(result.stdout.endswith(": initial source ownership snapshot\n"))

    def test_existing_import_is_reused_and_never_replaced_to_hide_later_growth(self):
        self.put(COUNT, "a.rs\t2\n")
        self.commit("Import")
        blob = self.git("rev-parse", f"HEAD:{COUNT}")
        self.commit(f"Reviewed snapshot\n\nInventory-Growth: {COUNT}: import {blob}: initial review")
        self.put(COUNT, "a.rs\t1\n")
        self.commit("Shrink after import")
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f": import {blob}:", result.stdout)
        self.put(COUNT, "a.rs\t3\n")
        self.commit("Growth after import")
        result = self.invoke()
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertIn("有增长，需要人写理由", result.stderr)

    def test_empty_comparison_history_cannot_yield_an_ignored_anchor(self):
        self.put(COUNT, "a.rs\t1\n")
        old_head = self.commit("Import before the base")
        (self.repo / COUNT).unlink()
        self.base = self.commit("Later base lacks it")
        result = self.invoke("--head", old_head)
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("comparison history", result.stderr)

    def test_sha256_blob_cannot_be_silently_shortened_to_rg4s_40_digit_format(self):
        with patch.object(guard, "git", return_value="100644 blob " + "a" * 64 + "\t" + COUNT + "\0"):
            with self.assertRaisesRegex(guard.InventoryError, "40-character blob"):
                imports.head_blob(self.repo, "HEAD", COUNT)

    def family_import(self, path, text):
        self.put(path, text)
        self.commit("First family snapshot")
        self.assertIn(path, guard.check(self.repo, self.base)["unreviewed"])
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        blob = self.git("rev-parse", f"HEAD:{path}")
        self.assertEqual(result.stdout, f"Inventory-Growth: {path}: import {blob}: initial inventory snapshot\n")
        self.commit("Initial import review\n\n" + result.stdout)
        self.assertEqual(guard.check(self.repo, self.base)["unreviewed"], [])
        command = [sys.executable, str(Path(guard.__file__)), "--repo", str(self.repo), "--base", self.base]
        self.assertEqual(subprocess.run(command, env=self.env, capture_output=True).returncode, 0)

    def test_rg4b_digest_first_import_round_trips(self):
        self.family_import("hub-rs/crates/steve-store-sqlite/tests/digest_write_sites.txt", "table\tsrc/a.rs\t2\n")

    def test_rg4b_run_first_import_with_bare_zero_round_trips(self):
        self.family_import("hub-rs/crates/steve-flow/tests/run_write_ownership.txt", "src/a.rs\t2\t0\n")

    def test_rg4b_due_first_import_round_trips(self):
        self.family_import(f"{guard.HUB_TESTS}/due_wake_sites.txt", "src/a.rs\t2\n")

    def mixed_growth(self, path, before, after):
        self.put(path, before)
        self.base = self.commit("Already present family inventory")
        self.put(path, after)
        self.commit("Existing growth or layout switch")
        self.assertIn(path, guard.check(self.repo, self.base)["unreviewed"])
        self.put(STRUCTURE, "crate\tsrc/f.rs\ttoo_many_lines\t120\n")
        self.commit("New first import alongside existing review requirement")
        result = self.invoke()
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(result.stdout, "")
        self.assertIn(path, result.stderr)
        self.assertIn("需要人写理由", result.stderr)

    def test_rg4b_digest_growth_among_imports_prints_no_markers(self):
        self.mixed_growth("hub-rs/crates/steve-store-sqlite/tests/digest_write_sites.txt", "table\tsrc/a.rs\t2\n", "table\tsrc/a.rs\t3\n")

    def test_rg4b_run_bare_zero_to_one_among_imports_prints_no_markers(self):
        self.mixed_growth("hub-rs/crates/steve-flow/tests/run_write_ownership.txt", "src/a.rs\t2\t0\n", "src/a.rs\t2\t1\n")

    def test_rg4b_run_total_decrease_cannot_cancel_bare_growth_among_imports(self):
        self.mixed_growth("hub-rs/crates/steve-flow/tests/run_write_ownership.txt", "src/a.rs\t2\t0\n", "src/a.rs\t1\t1\n")

    def test_rg4b_run_two_to_three_columns_among_imports_prints_no_markers(self):
        self.mixed_growth("hub-rs/crates/steve-flow/tests/run_write_ownership.txt", "src/a.rs\t2\n", "src/a.rs\t2\t0\n")

    def test_rg4b_run_three_to_two_columns_among_imports_prints_no_markers(self):
        self.mixed_growth("hub-rs/crates/steve-flow/tests/run_write_ownership.txt", "src/a.rs\t2\t0\n", "src/a.rs\t2\n")

    def test_rg4b_due_growth_among_imports_prints_no_markers(self):
        self.mixed_growth(f"{guard.HUB_TESTS}/due_wake_sites.txt", "src/a.rs\t2\n", "src/a.rs\t3\n")

    def test_rg4b_due_added_row_among_imports_prints_no_markers(self):
        self.mixed_growth(f"{guard.HUB_TESTS}/due_wake_sites.txt", "src/a.rs\t2\n", "src/a.rs\t2\nsrc/b.rs\t1\n")



if __name__ == "__main__":
    unittest.main()
