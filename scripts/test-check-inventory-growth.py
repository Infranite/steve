#!/usr/bin/env python3
"""Inventory comparison on constructed snapshots and isolated Git histories; no Cargo builds."""
import ast
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest
from unittest.mock import patch
import zlib

spec = importlib.util.spec_from_file_location("inventory_growth", Path(__file__).with_name("check-inventory-growth.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)
COUNT = f"{guard.HUB_TESTS}/test_sleep.txt"
DEBT = f"{guard.SCHEMA}/debt_stream.txt"
TRIGGERS = f"{guard.SCHEMA}/triggers.tsv"
STRUCTURE = guard.STRUCTURE
DIGEST_WRITES = "hub-rs/crates/steve-store-sqlite/tests/digest_write_sites.txt"
RUN_OWNERSHIP = "hub-rs/crates/steve-flow/tests/run_write_ownership.txt"
DUE_WAKE = f"{guard.HUB_TESTS}/due_wake_sites.txt"


# Complete structure.txt at 955f19fe; Git blob 1f2dad5392047ad9cbeb75eaed67f46f9020c2da.
W36_BASE_BLOB = '1f2dad5392047ad9cbeb75eaed67f46f9020c2da'
W36_BASE_TEXT = zlib.decompress(base64.b64decode(
    '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'
    'f/8/s1m8qA=='
)).decode("utf-8")

# Complete structure.txt at 7f689912d; Git blob e70b43f3feb60a5428afa67e1df1d1a0833109b6.
W36_HEAD_BLOB = 'e70b43f3feb60a5428afa67e1df1d1a0833109b6'
W36_HEAD_TEXT = zlib.decompress(base64.b64decode(
    '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'
    'f1IovKA='
)).decode("utf-8")


class Snapshots(unittest.TestCase):
    def growth(self, before, after, path=COUNT):
        return guard.compare_snapshots({path: before}, {path: after})[1]

    def test_count_keys_can_only_shrink(self):
        old = "a.rs\t2\nb.rs\t1\n"
        for now in (old, "a.rs\t1\n", ""):
            self.assertFalse(self.growth(old, now))
        for now in ("a.rs\t3\n", "a.rs\t2\nc.rs\t1\n"):
            self.assertTrue(self.growth(old, now))

    def test_structure_values_are_a_descending_multiset(self):
        old = "a\tf.rs\ttoo_many_lines\t300,120\n"
        for values in ("300,120", "250", "300,110"):
            self.assertFalse(self.growth(old, f"a\tf.rs\ttoo_many_lines\t{values}\n", STRUCTURE))
        for values in ("301,120", "300,121", "300,120,101"):
            self.assertTrue(self.growth(old, f"a\tf.rs\ttoo_many_lines\t{values}\n", STRUCTURE))
        self.assertTrue(self.growth(old, "a\tnew.rs\ttoo_many_lines\t101\n", STRUCTURE))

    def test_w36v_complete_historical_files_shrink_without_any_marker(self):
        for text, expected in [(W36_BASE_TEXT, W36_BASE_BLOB), (W36_HEAD_TEXT, W36_HEAD_BLOB)]:
            raw = text.encode("utf-8")
            digest = hashlib.sha1(b"blob " + str(len(raw)).encode() + b"\0" + raw, usedforsecurity=False).hexdigest()
            self.assertEqual(digest, expected)
        paths, changes = guard.compare_snapshots({STRUCTURE: W36_BASE_TEXT}, {STRUCTURE: W36_HEAD_TEXT})
        self.assertEqual(paths, [STRUCTURE])
        self.assertEqual(changes, {})

    def test_schema_metadata_preserves_stable_debt_and_trigger_identities(self):
        self.assertFalse(self.growth("write\tstream/write/1\told reason\n", "write\tstream/write/1\tnew reason\n", DEBT))
        self.assertTrue(self.growth("write\tstream/write/1\treason\n", "write\tstream/write/2\treason\n", DEBT))
        self.assertFalse(self.growth("name\ttable\tcore\told.operation\n", "name\ttable\tcore\tnew.operation\n", TRIGGERS))
        self.assertTrue(self.growth("name\ttable\tcore\top\n", "new_name\ttable\tcore\top\n", TRIGGERS))

    def test_real_published_i18n_set_format_projects_to_count_one(self):
        path = guard.I18N_REMAINING
        old = 'src/a.rs\t"中文"\n'
        self.assertFalse(self.growth(old, 'src/a.rs\t"中文"\t1\n', path))
        self.assertTrue(self.growth(old, 'src/a.rs\t"中文"\t2\n', path))
        self.assertTrue(self.growth(old, 'src/b.rs\t"中文"\t1\n', path))
        with self.assertRaises(guard.InventoryError):
            guard.parse(path, "src/a.rs\t1\n")

    def test_every_guard_family_is_selected_but_target_layering_is_not(self):
        included = [STRUCTURE, COUNT, f"{guard.HUB_TESTS}/new_guard.txt", f"{guard.HUB_TESTS}/layering_debt.txt",
                    *guard.WEB, DEBT, TRIGGERS, f"{guard.SQLITE_TESTS}/inventory_triggers.txt"]
        for path in included:
            self.assertTrue(guard.is_inventory(path), path)
        for path in (f"{guard.HUB_TESTS}/layering.txt", "README.md", "web/test/i18n.test.mjs", f"{guard.SCHEMA}/core/0001_baseline.sql"):
            self.assertFalse(guard.is_inventory(path), path)
        paths, growth = guard.compare_snapshots({}, {f"{guard.HUB_TESTS}/layering.txt": "any reviewed target edit"})
        self.assertEqual((paths, growth), ([], {}))

    def test_new_and_deleted_files_and_sqlite_count_sidecars(self):
        self.assertTrue(guard.compare_snapshots({}, {COUNT: "new.rs\t1\n"})[1])
        self.assertTrue(guard.compare_snapshots({COUNT: "old.rs\t1\n"}, {})[1])
        path = f"{guard.SQLITE_TESTS}/inventory_debt_comms.txt"
        self.assertTrue(self.growth("id\t1\n", "id\t2\n", path))
        self.assertFalse(guard.compare_snapshots({}, {COUNT: "# empty inventory\n"})[1])

    def test_layering_owner_or_deadline_changes_are_explicit_key_changes(self):
        path = f"{guard.HUB_TESTS}/layering_debt.txt"
        self.assertTrue(self.growth("w4\tc1\tedge a b\t1\n", "w4\tg1\tedge a b\t1\n", path))

    def test_duplicate_malformed_or_unsorted_data_fails_closed(self):
        for text in ("a\t0\n", "a\t-1\n", "a\t1e3\n", "a\t1.5\n", "a\tx\n", "a\n", "a\t1\na\t2\n", "\t1\n", "a\t1\nb\tx\t1\n"):
            with self.assertRaises(guard.InventoryError, msg=text):
                guard.parse(COUNT, text)
        for text in ("a\tf\ttoo_many_lines\t120,300\n", "a\tf\tunknown\t101\n", "a\tf\ttoo_many_lines\t0\n"):
            with self.assertRaises(guard.InventoryError):
                guard.parse(STRUCTURE, text)
        with self.assertRaises(guard.InventoryError):
            guard.parse(DEBT, "write\tid\n")
        with self.assertRaises(guard.InventoryError):
            guard.parse(TRIGGERS, "name\ttable\tcore\n")
        for path, text in ((DEBT, "write\tid\tfirst\nwrite\tid\tsecond\n"),
                           (TRIGGERS, "name\ta\tcore\top\nname\tb\tcore\top\n"),
                           (STRUCTURE, "a\tf\ttoo_many_lines\t101\na\tf\ttoo_many_lines\t102\n")):
            with self.assertRaisesRegex(guard.InventoryError, "duplicate key"):
                guard.parse(path, text)

    def test_crlf_and_unicode_in_json_keys(self):
        text = 'src/a.ts\t"汉\u2028字"\t1\r\n'
        self.assertEqual(guard.parse("web/test/guard-zh-compare.txt", text), {("src/a.ts", '"汉\u2028字"'): [1]})



    def test_rg4b_three_paths_are_discovered_by_the_shared_entry(self):
        for path in [DIGEST_WRITES, RUN_OWNERSHIP, DUE_WAKE]:
            self.assertTrue(guard.is_inventory(path), path)
        self.assertFalse(guard.is_inventory(f"{guard.HUB_TESTS}/layering.txt"))

    def test_rg4b_digest_kind_and_path_are_the_count_identity(self):
        parsed = guard.parse(DIGEST_WRITES, "table\tsrc/a.rs\t2\nwrite\tsrc/a.rs\t1\n")
        self.assertEqual(parsed, {("table", "src/a.rs"): [2], ("write", "src/a.rs"): [1]})
        changes = self.growth("table\tsrc/a.rs\t2\n", "write\tsrc/a.rs\t2\n", DIGEST_WRITES)
        self.assertIn(DIGEST_WRITES, changes)

    def test_rg4b_run_total_and_bare_are_independent_including_zero(self):
        before = "src/a.rs\t2\t0\n"
        self.assertEqual(guard.parse(RUN_OWNERSHIP, before),
                         {("src/a.rs", "total"): [2], ("src/a.rs", "bare"): [0]})
        self.assertNotIn(("src/a.rs", "bare"), guard.parse(RUN_OWNERSHIP, "src/a.rs\t2\n"))
        for after in ["src/a.rs\t2\t1\n", "src/a.rs\t1\t1\n"]:
            with self.subTest(after=after):
                changes = self.growth(before, after, RUN_OWNERSHIP)[RUN_OWNERSHIP]
                self.assertEqual([(x["key"], x["before"], x["head"]) for x in changes],
                                 [(("src/a.rs", "bare"), [0], [1])])
        self.assertFalse(self.growth("src/a.rs\t2\t1\n", "src/a.rs\t1\t0\n", RUN_OWNERSHIP))
        changes = self.growth("src/a.rs\t2\t1\n", "src/a.rs\t3\t0\n", RUN_OWNERSHIP)[RUN_OWNERSHIP]
        self.assertEqual([(x["key"], x["before"], x["head"]) for x in changes],
                         [(("src/a.rs", "total"), [2], [3])])

    def test_rg4b_new_family_formats_fail_closed(self):
        for path, text in [(DIGEST_WRITES, "table\tsrc/a.rs\t0\n"),
                           (DIGEST_WRITES, "table\tsrc/a.rs\textra\t1\n"),
                           (RUN_OWNERSHIP, "src/a.rs\t0\t0\n"),
                           (RUN_OWNERSHIP, "src/a.rs\t2\t-1\n"),
                           (RUN_OWNERSHIP, "src/a.rs\t1\t2\n"),
                           (RUN_OWNERSHIP, "src/a.rs\t2\t0\t1\n"),
                           (RUN_OWNERSHIP, "src/a.rs\t2\nsrc/b.rs\t2\t0\n"),
                           (RUN_OWNERSHIP, "src/a.rs\t2\t0\nsrc/b.rs\t2\n"),
                           (DUE_WAKE, "src/a.rs\tother\t1\n"),
                           (DUE_WAKE, "src/a.rs\t0\n")]:
            with self.subTest(path=path, text=text), self.assertRaises(guard.InventoryError):
                guard.parse(path, text)



class Histories(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="inventory-git-test-")
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        self.env = os.environ.copy()
        for key in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY", "GIT_ALTERNATE_OBJECT_DIRECTORIES"):
            self.env.pop(key, None)
        self.git("init", "-b", "trunk")
        self.put(COUNT, "a.rs\t2\n")
        self.base = self.commit("Initial inventory")

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.repo), "-c", "user.name=Inventory tests",
            "-c", "user.email=inventory-tests@users.noreply.github.com", "-c", "commit.gpgsign=false",
            "-c", "core.hooksPath=/dev/null", "-c", "rebase.autoStash=false", "-c", "rebase.updateRefs=false", *args], env=self.env, text=True, stderr=subprocess.PIPE).strip()

    def put(self, path, body):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(body, encoding="utf-8")

    def commit(self, message):
        body = self.repo / ".git/test-message"
        body.write_text(message + "\n", encoding="utf-8")
        self.git("add", "--all")
        self.git("commit", "--quiet", "--allow-empty", "--file", str(body))
        return self.git("rev-parse", "HEAD")

    def report(self):
        return guard.check(self.repo, self.base)

    def test_only_git_objects_count_working_tree_and_init_cannot_hide_growth(self):
        self.put(COUNT, "a.rs\t3\n")
        self.assertFalse(self.report()["growth"])  # uncommitted working data is not a CI snapshot
        self.commit("Grow without approval")
        (self.repo / COUNT).unlink()
        self.put(COUNT, "a.rs\t1\n")
        self.assertTrue(self.report()["growth"])
        self.assertFalse(self.report()["markers"])
        self.put(COUNT, "a.rs\t1\n")
        self.commit("Record the actual reduction")
        self.assertFalse(self.report()["growth"])

    def test_review_marker_must_be_a_nonempty_trailer_in_this_range(self):
        self.put(COUNT, "a.rs\t3\n")
        self.commit(f"Reviewed increase\n\nInventory-Growth: {COUNT} a.rs = 3: new reviewed source ownership")
        report = self.report()
        self.assertTrue(report["growth"])
        self.assertEqual(report["markers"][0]["reason"], "new reviewed source ownership")
        self.base = self.git("rev-parse", "HEAD")
        self.put(COUNT, "a.rs\t4\n")
        self.commit("A later increase cannot inherit an older approval")
        self.assertTrue(self.report()["growth"])
        self.assertFalse(self.report()["markers"])

    def test_marker_in_subject_or_body_and_empty_reason_do_not_authorize_growth(self):
        for message in (f"Inventory-Growth: {COUNT} a.rs = 3: subject only", f"Explain\n\nInventory-Growth: {COUNT} a.rs = 3: quoted in body\n\nUnrelated final paragraph", f"Empty\n\nInventory-Growth: {COUNT}:", "Old format\n\nInventory-Growth: global exception"):
            self.put(COUNT, "a.rs\t3\n")
            self.commit(message)
            self.assertFalse(self.report()["markers"], message)

    def test_marker_cannot_authorize_bad_inventory_format(self):
        self.put(COUNT, "a.rs\t0\n")
        self.commit(f"Bad data\n\nInventory-Growth: {COUNT} a.rs = 3: reason cannot bypass parsing")
        with self.assertRaises(guard.InventoryError):
            self.report()

    def test_marker_example_in_a_file_is_not_a_commit_trailer(self):
        self.put(COUNT, "a.rs\t3\n")
        self.put("README.md", f"Inventory-Growth: {COUNT} a.rs = 3: documentation is not approval\n")
        self.commit("Document the marker format")
        self.assertTrue(self.report()["growth"])
        self.assertFalse(self.report()["markers"])

    def test_cli_rejects_unmarked_growth_and_accepts_explicit_marker(self):
        self.put(COUNT, "a.rs\t3\n")
        self.commit("No marker")
        command = [sys.executable, str(guard.ROOT / "scripts/check-inventory-growth.py"), "--repo", str(self.repo), "--base", self.base]
        result = subprocess.run(command, capture_output=True, text=True, env=self.env)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("Inventory-Growth key/result", result.stderr)
        self.commit(f"Intentional approval\n\nInventory-Growth: {COUNT} a.rs = 3: acknowledge this candidate's source debt")
        result = subprocess.run(command, capture_output=True, text=True, env=self.env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("exact-result review markers", result.stdout)

    def test_invalid_commit_fails_instead_of_treating_history_as_an_empty_inventory(self):
        with self.assertRaisesRegex(guard.InventoryError, "cannot resolve comparison commit"):
            guard.check(self.repo, "does-not-exist")

    def test_markers_name_each_growing_path_including_initial_imports(self):
        other = "web/test/harness-parity.txt"
        self.put(COUNT, "a.rs\t3\n")
        self.commit(f"Review A\n\nInventory-Growth: {COUNT} a.rs = 3: reviewed increase")
        self.put(other, 'src/a.ts\t"codex"\t1\n')
        self.commit("Import B later without a marker")
        report = self.report()
        self.assertEqual(report["unreviewed"], [other])
        self.commit(f'Review the new path\n\nInventory-Growth: {other} src/a.ts\t"codex" = 1: explicit new key')
        self.assertFalse(self.report()["unreviewed"])

    def test_generic_path_marker_cannot_authorize_an_existing_key(self):
        self.put(COUNT, "a.rs\t3\n")
        self.commit(f"Old path approval\n\nInventory-Growth: {COUNT}: no key or head result")
        self.assertEqual(self.report()["unreviewed"], [COUNT])

    def test_authorization_is_the_whole_head_result_not_a_permanent_key_budget(self):
        self.put(COUNT, "a.rs\t3\n")
        self.commit(f"Review this result\n\nInventory-Growth: {COUNT} a.rs = 3: approved result")
        self.assertFalse(self.report()["unreviewed"])
        self.put(COUNT, "a.rs\t4\n")
        self.commit("The same key changes again")
        self.assertEqual(self.report()["unreviewed"], [COUNT])

    def test_wildcard_does_not_cover_real_inventory_paths(self):
        self.put(COUNT, "a.rs\t3\n")
        self.commit(f"Not a path review\n\nInventory-Growth: {guard.HUB_TESTS}/*.txt: broad exception")
        self.assertEqual(self.report()["unreviewed"], [COUNT])
        self.assertFalse(self.report()["markers"])

    def test_deleted_or_moved_inventory_needs_its_own_path_marker(self):
        (self.repo / COUNT).unlink()
        self.put("notes/counts.txt", "a.rs\t2\n")
        self.commit("Move outside discovery rules")
        report = self.report()
        self.assertEqual(report["unreviewed"], [COUNT])
        self.assertIn("inventory file removed", " ".join(report["growth"]))
        self.commit("Wrong path\n\nInventory-Growth: notes/counts.txt: moved data is not the old inventory")
        self.assertEqual(self.report()["unreviewed"], [COUNT])
        self.commit(f"Review removal\n\nInventory-Growth: {COUNT}: guard retired and inventory removed")
        self.assertFalse(self.report()["unreviewed"])

    def test_even_an_empty_inventory_cannot_disappear_without_a_marker(self):
        self.put(COUNT, "# keep empty inventories\n")
        self.base = self.commit("Empty inventory")
        (self.repo / COUNT).unlink()
        self.commit("Remove empty file")
        self.assertEqual(self.report()["unreviewed"], [COUNT])

    def test_import_anchor_does_not_hide_offsetting_per_key_growth_table(self):
        initial = "pkg\tx.rs\ttoo_many_lines\t120\npkg\ty.rs\ttoo_many_lines\t130\n"
        changed = "pkg\tx.rs\ttoo_many_lines\t120,110\n"
        larger = "pkg\tx.rs\ttoo_many_lines\t125,110\n"
        self.put(STRUCTURE, initial)
        first = self.commit("Initial structure snapshot")
        blob = self.git("rev-parse", f"{first}:{STRUCTURE}")
        cases = [
            ("no marker", first, changed, None, True),
            ("import only", self.base, None, f"{STRUCTURE}: import {blob}: reviewed initial content", True),
            ("partial result", self.base, None, f"{STRUCTURE} pkg\tx.rs\ttoo_many_lines = 110: not the complete result", True),
            ("exact A result", self.base, None, f"{STRUCTURE} pkg\tx.rs\ttoo_many_lines = 120,110: review A", False),
            ("A changes again", self.base, larger, None, True),
        ]
        for label, base, body, marker, blocked in cases:
            with self.subTest(case=label):
                if body is not None:
                    self.put(STRUCTURE, body)
                self.commit(label + (f"\n\nInventory-Growth: {marker}" if marker else ""))
                report = guard.check(self.repo, base)
                self.assertEqual(report["totals"][STRUCTURE], {"baseline": 2, "head": 2})
                self.assertEqual([c["key"] for c in report["changes"][STRUCTURE]], [("pkg", "x.rs", "too_many_lines")])
                self.assertEqual(bool(report["unreviewed"]), blocked)
                if label == "import only":
                    self.assertEqual([m["blob"] for m in report["markers"] if m["kind"] == "import"], [blob])

    def test_import_blob_must_be_full_length_and_appear_at_that_path_in_range(self):
        text = "pkg\tx.rs\ttoo_many_lines\t300\n"
        self.put("elsewhere.txt", text)
        other = self.commit("A blob stored at another path")
        wrong = self.git("rev-parse", f"{other}:elsewhere.txt")
        self.put(STRUCTURE, "pkg\tx.rs\ttoo_many_lines\t120\n")
        self.commit(f"Wrong path anchor\n\nInventory-Growth: {STRUCTURE}: import {wrong}: not this path")
        report = self.report()
        self.assertEqual(report["unreviewed"], [STRUCTURE])
        self.assertTrue(any("not in this path's history" in note for note in report["notes"]))
        actual = self.git("rev-parse", f"HEAD:{STRUCTURE}")
        self.commit(f"Short hash is not an import anchor\n\nInventory-Growth: {STRUCTURE}: import {actual[:12]}: incomplete")
        self.assertEqual(self.report()["unreviewed"], [STRUCTURE])
        # An object from before the comparison range cannot silently become an import baseline.
        prior = self.base
        self.base = self.git("rev-parse", "HEAD")
        self.put("older.txt", text)
        self.commit("Later history")
        self.assertNotIn(wrong, guard.path_history_blobs(self.repo, self.base, "HEAD", STRUCTURE))
        self.base = prior

    def test_import_anchor_before_the_range_is_not_accepted_for_a_reintroduced_path(self):
        self.put(STRUCTURE, "pkg\tx.rs\ttoo_many_lines\t300\n")
        old = self.commit("Earlier inventory")
        blob = self.git("rev-parse", f"{old}:{STRUCTURE}")
        (self.repo / STRUCTURE).unlink()
        self.base = self.commit("Retire that inventory")
        self.put(STRUCTURE, "pkg\tx.rs\ttoo_many_lines\t120\n")
        self.commit(f"Reintroduction\n\nInventory-Growth: {STRUCTURE}: import {blob}: this blob is outside the range")
        report = self.report()
        self.assertEqual(report["unreviewed"], [STRUCTURE])
        self.assertTrue(any("not in this path's history" in note for note in report["notes"]))

    def test_conflicting_valid_import_anchors_cannot_silently_choose_a_larger_budget(self):
        self.put(STRUCTURE, "pkg\tx.rs\ttoo_many_lines\t120\n")
        first = self.commit("First snapshot")
        a = self.git("rev-parse", f"{first}:{STRUCTURE}")
        self.put(STRUCTURE, "pkg\tx.rs\ttoo_many_lines\t300\n")
        second = self.commit("Second snapshot")
        b = self.git("rev-parse", f"{second}:{STRUCTURE}")
        self.commit(f"Conflicting reviews\n\nInventory-Growth: {STRUCTURE}: import {a}: first\nInventory-Growth: {STRUCTURE}: import {b}: second")
        with self.assertRaisesRegex(guard.InventoryError, "conflicting import anchors"):
            self.report()

    def test_import_does_not_replace_an_existing_inventory_baseline(self):
        self.put(COUNT, "a.rs\t3\n")
        commit = self.commit("Existing inventory grows")
        blob = self.git("rev-parse", f"{commit}:{COUNT}")
        self.commit(f"Not a first import\n\nInventory-Growth: {COUNT}: import {blob}: cannot reset existing budget")
        report = self.report()
        self.assertEqual(report["unreviewed"], [COUNT])
        self.assertTrue(any("only a newly introduced" in note for note in report["notes"]))

    def test_result_key_can_include_colons_and_reason_can_include_equal_signs(self):
        path = f"{guard.HUB_TESTS}/runtime_guard.txt"
        self.put(path, "file.rs\ttimer\timpl Runner::run\t1\n")
        self.base = self.commit("Original key")
        self.put(path, "file.rs\ttimer\timpl Runner::run\t2\n")
        self.commit(f"Specific result\n\nInventory-Growth: {path} file.rs\ttimer\timpl Runner::run = 2: reason = 7: still data")
        self.assertFalse(self.report()["unreviewed"])

    def test_tab_separated_key_fields_do_not_share_a_space_join_authorization(self):
        path = "web/test/harness-parity.txt"
        self.put(path, "a b\tc\t1\na\tb c\t1\n")
        self.base = self.commit("Two distinct keys that collide under space join")
        self.put(path, "a b\tc\t2\na\tb c\t2\n")
        self.commit(f"Space-joined key is not approval\n\nInventory-Growth: {path} a b c = 2: ambiguous old format")
        self.assertEqual(len(self.report()["unreviewed_changes"]), 2)
        self.commit(f"Only the first key reviewed\n\nInventory-Growth: {path} a b\tc = 2: first key only")
        report = self.report()
        self.assertEqual([marker["key"] for marker in report["markers"]], [("a b", "c")])
        self.assertEqual(report["unreviewed_changes"], [f"{path}: a\tb c = 2 (baseline 1)"])

    def test_comparison_range_equals_reviewed_coverage_event_table(self):
        # One table pins the four integration modes. Rebase rewrites commit IDs, not path reviews.
        cases = [
            ("PR", "pull_request", "merge", False),
            ("rebase multi-commit push", "push", "rebase", False),
            ("merge_group", "merge_group", "merge", False),
            ("new branch first push", "push", "first", True),
        ]
        for label, name, mode, fallback in cases:
            with self.subTest(event=label):
                self.git("checkout", "-B", "trunk", self.base)
                self.put("target.txt", label)
                target = self.commit("Unrelated target-branch update")
                self.git("checkout", "-B", "feature", self.base)
                if mode == "first":
                    self.put("branch.txt", label)
                    reviewed_base = self.commit("Set up new branch")
                    self.put(COUNT, "a.rs\t3\n")
                    head = self.commit(f"Reviewed first-push change\n\nInventory-Growth: {COUNT} a.rs = 3: {label}")
                    event = {"before": "0" * 40}
                    covered = {head}
                else:
                    self.put(COUNT, "a.rs\t3\n")
                    first = self.commit(f"Reviewed growth in first commit\n\nInventory-Growth: {COUNT} a.rs = 3: {label}")
                    self.put("feature.txt", label)
                    source = self.commit("Second commit has no growth marker")
                    reviewed_base = target
                    if mode == "rebase":
                        self.git("rebase", target)
                        head = self.git("rev-parse", "HEAD")
                        rewritten = self.git("rev-parse", "HEAD^")
                        self.assertNotEqual(rewritten, first)
                        event = {"before": target}
                        covered = {rewritten, head}
                    else:
                        self.git("checkout", "-B", "candidate", target)
                        self.git("merge", "--no-ff", "--no-edit", source)
                        head = self.git("rev-parse", "HEAD")
                        event = ({"pull_request": {"base": {"sha": target}, "head": {"sha": source}}}
                                 if name == "pull_request" else {"merge_group": {"base_sha": target}})
                        covered = {first, source, head}
                selection = guard.event_range(self.repo, name, event, head)
                report = guard.check(self.repo, selection["base"], selection["head"])
                self.assertEqual((report["base"], report["head"]), (reviewed_base, head))
                self.assertEqual(set(self.git("rev-list", f"{report['base']}..{report['head']}").splitlines()), covered)
                self.assertTrue(report["growth"])
                self.assertFalse(report["unreviewed"])
                self.assertEqual({marker["path"] for marker in report["markers"]}, {COUNT})
                self.assertEqual("falling back to HEAD^" in selection["reason"], fallback)

    def test_pr_merge_parents_must_match_both_payload_shas(self):
        self.git("checkout", "-b", "feature")
        self.put(COUNT, "a.rs\t3\n")
        source = self.commit(f"Feature\n\nInventory-Growth: {COUNT} a.rs = 3: reviewed feature")
        self.git("checkout", "trunk")
        self.put("target.txt", "target change")
        target = self.commit("Target advanced")
        self.git("merge", "--no-ff", "--no-edit", source)
        for base, feature in [(self.base, source), (target, self.base)]:
            event = {"pull_request": {"base": {"sha": base}, "head": {"sha": feature}}}
            with self.assertRaisesRegex(guard.InventoryError, "PR merge parents"):
                guard.event_range(self.repo, "pull_request", event)
        with self.assertRaisesRegex(guard.InventoryError, "PR merge parents"):
            guard.event_range(self.repo, "pull_request", {"pull_request": {"base": {"sha": self.base}, "head": {"sha": source}}}, source)

    def test_force_push_falls_back_but_missing_before_history_does_not(self):
        self.put(COUNT, "a.rs\t9\n")
        abandoned = self.commit("Abandoned history")
        self.git("checkout", "-B", "rewritten", self.base)
        self.put(COUNT, "a.rs\t3\n")
        head = self.commit(f"Replacement\n\nInventory-Growth: {COUNT} a.rs = 3: reviewed replacement")
        selection = guard.event_range(self.repo, "push", {"before": abandoned})
        self.assertIn("force push", selection["reason"])
        self.assertEqual(guard.resolve(self.repo, selection["base"]), self.base)
        self.assertFalse(guard.check(self.repo, selection["base"])["unreviewed"])
        with self.assertRaisesRegex(guard.InventoryError, "cannot resolve"):
            guard.event_range(self.repo, "push", {"before": "f" * 40})
        missing_force = guard.event_range(self.repo, "push", {"before": "f" * 40, "forced": True})
        self.assertIn("payload confirms force push", missing_force["reason"])
        self.assertEqual(guard.resolve(self.repo, missing_force["base"]), self.base)
        event = self.repo / ".git/event.json"
        event.write_text(json.dumps({"before": abandoned}))
        result = subprocess.run([sys.executable, str(guard.ROOT / "scripts/check-inventory-growth.py"),
                                 "--repo", str(self.repo), "--event-file", str(event)],
                                env=dict(self.env, GITHUB_EVENT_NAME="push"), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("force push", result.stdout)
        self.assertIn("falling back to HEAD^", result.stdout)

    def test_invalid_event_payloads_fail_closed(self):
        for name, event in [("workflow_dispatch", {}), ("pull_request", []),
                            ("pull_request", {"pull_request": []}),
                            ("merge_group", {"merge_group": {"base_sha": "bad"}}),
                            ("push", {}), ("push", {"before": "bad"})]:
            with self.subTest(name=name, event=event), self.assertRaises(guard.InventoryError):
                guard.event_range(self.repo, name, event)

    def comparator_cli(self, base):
        return subprocess.run([sys.executable, str(guard.ROOT / "scripts/check-inventory-growth.py"),
                               "--repo", str(self.repo), "--base", base], env=self.env,
                              text=True, capture_output=True)

    def test_rg4b_each_new_family_is_found_blocked_then_can_purely_shrink(self):
        families = [
            (DIGEST_WRITES, "table\tsrc/a.rs\t2\n", ["table\tsrc/a.rs\t3\n"], "table\tsrc/a.rs\t1\n"),
            (RUN_OWNERSHIP, "src/a.rs\t2\t0\n", ["src/a.rs\t2\t1\n", "src/a.rs\t1\t1\n"], "src/a.rs\t1\t0\n"),
            (DUE_WAKE, "src/a.rs\t2\n", ["src/a.rs\t3\n", "src/a.rs\t2\nsrc/b.rs\t1\n"], "src/a.rs\t1\n"),
        ]
        for path, original, failures, shrink in families:
            with self.subTest(path=path):
                self.put(path, original)
                base = self.commit("Known baseline for " + path)
                for after in failures:
                    self.put(path, after)
                    self.commit("Unreviewed increase for " + path)
                    report = guard.check(self.repo, base)
                    self.assertIn(path, report["paths"])
                    self.assertIn(path, report["unreviewed"])
                    self.assertEqual(self.comparator_cli(base).returncode, 1)
                    if path == RUN_OWNERSHIP:
                        self.assertEqual(report["changes"][path][0]["key"], ("src/a.rs", "bare"))
                self.put(path, shrink)
                self.commit("Pure decrease for " + path)
                self.assertEqual(guard.check(self.repo, base)["unreviewed"], [])
                self.assertEqual(self.comparator_cli(base).returncode, 0)

    def test_rg4b_existing_run_format_switch_cannot_use_an_import_or_infer_bare_zero(self):
        for before, after, old_columns, new_columns in [
            ("src/a.rs\t2\n", "src/a.rs\t2\t0\n", 2, 3),
            ("src/a.rs\t2\n", "src/a.rs\t1\t1\n", 2, 3),
            ("src/a.rs\t2\t0\n", "src/a.rs\t2\n", 3, 2),
        ]:
            with self.subTest(old=old_columns, new=new_columns, after=after):
                self.put(RUN_OWNERSHIP, before)
                base = self.commit("Existing run inventory layout")
                self.put(RUN_OWNERSHIP, after)
                self.commit("Switch run inventory layout")
                blob = self.git("rev-parse", f"HEAD:{RUN_OWNERSHIP}")
                self.commit(f"An existing path cannot import\n\nInventory-Growth: {RUN_OWNERSHIP}: import {blob}: not a new file")
                report = guard.check(self.repo, base)
                self.assertIn(RUN_OWNERSHIP, report["unreviewed"])
                formats = [c for c in report["changes"][RUN_OWNERSHIP] if c["kind"] == "format"]
                self.assertEqual([(c["before_format"], c["head_format"]) for c in formats], [(old_columns, new_columns)])
                self.assertTrue(any("only a newly introduced" in n for n in report["notes"]))
                self.assertFalse(any(m["kind"] == "import" for m in report["markers"]))
                self.assertEqual(self.comparator_cli(base).returncode, 1)
                # Each new bare budget AND the layout change need their own exact human reason.
                lines = [f"Inventory-Growth: {RUN_OWNERSHIP}: format {old_columns}->{new_columns}: reviewed column semantics"]
                for change in report["changes"][RUN_OWNERSHIP]:
                    if change["kind"] == "budget":
                        lines.append(f"Inventory-Growth: {RUN_OWNERSHIP} {guard.key_text(change['key'])} = {guard.value_text(change['head'])}: reviewed new column budget")
                self.commit("Explicit layout and key review\n\n" + "\n".join(lines))
                self.assertEqual(self.comparator_cli(base).returncode, 0)

    def assert_run_layout(self, before, after, formats, bare_before, bare_after):
        self.put(RUN_OWNERSHIP, before)
        base = self.commit("Run layout baseline with exact line endings")
        self.put(RUN_OWNERSHIP, after)
        self.commit("Run layout after line ending or column change")
        self.assertEqual(guard.snapshot(self.repo, base)[RUN_OWNERSHIP], before)
        self.assertEqual(guard.snapshot(self.repo, "HEAD")[RUN_OWNERSHIP], after)
        report = guard.check(self.repo, base)
        actual = [(c["before_format"], c["head_format"])
                  for c in report["changes"].get(RUN_OWNERSHIP, []) if c["kind"] == "format"]
        self.assertEqual(actual, formats)
        self.assertEqual(report["totals"][RUN_OWNERSHIP],
                         {"baseline": 2, "head": 2, "bare_baseline": bare_before, "bare_head": bare_after})
        result = self.comparator_cli(base)
        self.assertEqual(result.returncode, 1 if formats else 0, result.stderr)
        if formats:
            self.assertIn(RUN_OWNERSHIP, report["unreviewed"])
            old, new = formats[0]
            self.assertIn(f"format changed {old}->{new}", result.stderr)
        else:
            self.assertEqual(report["changes"], {})
            self.assertEqual(report["unreviewed"], [])

    def test_rg4b_crlf_blank_line_cannot_hide_three_to_two_columns(self):
        self.assert_run_layout("\r\nsrc/a.rs\t2\t0\r\n", "\r\nsrc/a.rs\t2\r\n",
                               [(3, 2)], 0, None)

    def test_rg4b_crlf_blank_line_cannot_hide_two_to_three_columns(self):
        self.assert_run_layout("\r\nsrc/a.rs\t2\r\n", "\r\nsrc/a.rs\t2\t0\r\n",
                               [(2, 3)], None, 0)

    def test_rg4b_adding_only_crlf_blank_lines_preserves_layout_and_totals(self):
        for row, bare in [("src/a.rs\t2\r\n", None), ("src/a.rs\t2\t0\r\n", 0)]:
            with self.subTest(bare=bare):
                self.assert_run_layout(row, "\r\n" + row, [], bare, bare)

    def test_rg4b_a_layout_review_cannot_authorize_later_inventory_removal(self):
        self.put(RUN_OWNERSHIP, "src/a.rs\t2\n")
        base = self.commit("Existing run inventory")
        self.put(RUN_OWNERSHIP, "src/a.rs\t2\t0\n")
        self.commit(f"Format review only\n\nInventory-Growth: {RUN_OWNERSHIP}: format 2->3: reviewed layout")
        (self.repo / RUN_OWNERSHIP).unlink()
        self.commit("Later removal has no removal reason")
        self.assertIn(RUN_OWNERSHIP, guard.check(self.repo, base)["unreviewed"])



class WorkflowWiring(unittest.TestCase):
    def setUp(self):
        self.workflow = (guard.ROOT / ".github/workflows/ci.yml").read_text()

    def job(self, name):
        match = re.search(rf"(?ms)^  {re.escape(name)}:\n(.*?)(?=^  [a-zA-Z][a-zA-Z0-9-]*:|\Z)", self.workflow)
        self.assertIsNotNone(match, name)
        return match[1]

    def steps(self, job):
        return re.split(r"(?m)^      - ", self.job(job))

    def condition(self, event, changed, cancelled=False):
        expression = next(line.split("if:", 1)[1].strip() for line in self.job("structure").splitlines() if line.strip().startswith("if:"))
        expression = expression.removeprefix("${{").removesuffix("}}").strip()
        expression = expression.replace("!cancelled()", "not cancelled").replace("github.event_name", "event").replace("needs.changes.outputs.structure", "changed").replace("&&", " and ").replace("||", " or ")
        tree = ast.parse(expression, mode="eval")
        allowed = (ast.Expression, ast.BoolOp, ast.And, ast.Or, ast.UnaryOp, ast.Not,
                   ast.Compare, ast.Eq, ast.NotEq, ast.Name, ast.Load, ast.Constant)
        self.assertTrue(all(isinstance(node, allowed) for node in ast.walk(tree)))
        return eval(compile(tree, "structure-job-if", "eval"), {"__builtins__": {}},
                    {"event": event, "changed": changed, "cancelled": cancelled})

    def test_rg4b_ci_executes_import_marker_tests_in_the_scripts_job(self):
        step = next(s for s in self.steps("scripts") if "name: Verify public boundary" in s)
        commands = [line.strip() for line in step.split("run: |\n", 1)[1].splitlines()
                    if line.strip() and not line.lstrip().startswith("#")]
        command = "python3 scripts/test-inventory-import-markers.py"
        self.assertEqual(commands.count(command), 1)
        self.assertLess(commands.index("python3 scripts/test-check-inventory-growth.py"), commands.index(command))

    def test_structure_is_an_independent_macos_pr_job_and_is_required_by_the_gate(self):
        structure = self.job("structure")
        self.assertIn("runs-on: macos-15", structure)
        self.assertIn("needs: changes", structure)
        commands = re.findall(r"(?m)^\s+run: (.+)$", structure)
        self.assertEqual(commands, [
            "cargo clean --locked --manifest-path hub-rs/Cargo.toml --workspace --profile dev",
            "python3 scripts/structure_guard.py",
        ])
        cache = next(step for step in self.steps("structure") if "actions/cache/restore@" in step)
        paths = cache.split("          path: |\n", 1)[1].split("          key:", 1)[0]
        self.assertEqual(paths.split(), ["~/.cargo/registry", "~/.cargo/git"])
        self.assertLess(structure.index("actions/cache/restore@"), structure.index("Always rebuild workspace packages"))
        self.assertIn("components: clippy", structure)
        self.assertNotIn("structure_guard.py", self.job("rust-test"))
        self.assertIn("components: rustfmt", self.job("rust-test"))
        self.assertNotIn("clippy", self.job("rust-test"))
        self.assertEqual(self.workflow.count("run: python3 scripts/structure_guard.py"), 1)
        needs = re.search(r"needs: \[([^]]+)\]", self.job("test"))[1].split(", ")
        self.assertIn("structure", needs)
        for event, changed, included in [("pull_request", "true", True),
                                         ("pull_request", "false", False),
                                         ("pull_request", "", True),
                                         ("push", "false", True),
                                         ("merge_group", "", True)]:
            with self.subTest(event=event, changed=changed):
                self.assertEqual(self.condition(event, changed), included)
        self.assertFalse(self.condition("push", "true", cancelled=True))

    def test_structure_input_paths_include_pr_job_and_api_failure_is_conservative(self):
        step = next(step for step in self.steps("changes") if "id: apple" in step)
        self.assertIn(".previous_filename", step)
        self.assertIn("structure: ${{ steps.apple.outputs.structure }}", self.job("changes"))
        script = textwrap.dedent(step.split("        run: |\n", 1)[1])
        script = script.replace("${{ github.repository }}", "example/repo")
        cases = [
            ("pull_request", ["hub-rs/crates/example/src/lib.rs"], False, "true"),
            ("pull_request", ["scripts/structure_guard.py"], False, "true"),
            ("pull_request", [".github/workflows/ci.yml"], False, "true"),
            ("pull_request", ["scripts/structure_guard.py.bak"], False, "false"),
            ("pull_request", [".github/workflows/ci.yml.bak"], False, "false"),
            ("pull_request", ["CONTRIBUTING.md"], False, "false"),
            ("pull_request", ["notes/moved.txt", "hub-rs/structure.txt"], False, "true"),
            ("pull_request", [], True, "true"),
            ("push", [], False, "true"),
        ]
        for event, files, failed, expected in cases:
            with self.subTest(event=event, files=files, failed=failed), tempfile.TemporaryDirectory() as tmp:
                directory = Path(tmp)
                fake = directory / "gh"
                fake.write_text("#!/bin/sh\nexit 1\n" if failed else '#!/bin/sh\ncat "$FILE_LIST"\n')
                fake.chmod(0o700)
                listing = directory / "paths"
                listing.write_text("\n".join(files) + "\n")
                output = directory / "outputs"
                env = dict(os.environ, PATH=str(directory) + os.pathsep + os.environ["PATH"],
                           FILE_LIST=str(listing), GITHUB_OUTPUT=str(output), PR="1")
                result = subprocess.run(["bash", "-e", "-c", script.replace("${{ github.event_name }}", event)],
                                        cwd=directory, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                outputs = dict(line.split("=", 1) for line in output.read_text().splitlines())
                self.assertEqual(outputs["structure"], expected)
                self.assertEqual(self.condition(event, outputs["structure"]), expected == "true")

    def test_gate_only_allows_structure_skipping_for_a_pr_without_rust_changes(self):
        job = self.job("test")
        needs = re.search(r"needs: \[([^]]+)\]", job)[1].split(", ")
        code = textwrap.dedent(job.split("python3 - <<'PY'\n", 1)[1].rsplit("\n          PY", 1)[0])
        cases = [
            ("pull_request", "true", "success", True),
            ("pull_request", "true", "skipped", False),
            ("pull_request", "true", "failure", False),
            ("pull_request", "false", "skipped", True),
            ("pull_request", "false", "failure", False),
            ("push", "false", "skipped", False),
            ("push", "false", "success", True),
            ("merge_group", "false", "skipped", False),
            ("merge_group", "false", "success", True),
        ]
        for event, changed, result, accepted in cases:
            for rust_skip in (("false",) if event == "merge_group" else ("false", "true")):
                with self.subTest(event=event, changed=changed, result=result, rust_skip=rust_skip):
                    states = {name: {"result": "success", "outputs": {}} for name in needs}
                    states["changes"]["outputs"] = {"skip": rust_skip, "apple": "true", "structure": changed}
                    if event == "merge_group":
                        states["changes"]["result"] = "skipped"
                    if rust_skip == "true":
                        for name in ("rust-test", "rust", "apple"):
                            states[name]["result"] = "skipped"
                    states["structure"]["result"] = result
                    with patch.dict(os.environ, RESULTS=json.dumps(states), GITHUB_EVENT_NAME=event):
                        if accepted:
                            exec(compile(code, "CI gate", "exec"), {})
                        else:
                            with self.assertRaises(AssertionError):
                                exec(compile(code, "CI gate", "exec"), {})


if __name__ == "__main__":
    unittest.main()
