#!/usr/bin/env python3
"""Inventory comparison on constructed snapshots and isolated Git histories; no Cargo builds."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("inventory_growth", Path(__file__).with_name("check-inventory-growth.py"))
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)
COUNT = f"{guard.HUB_TESTS}/test_sleep.txt"
DEBT = f"{guard.SCHEMA}/debt_stream.txt"
TRIGGERS = f"{guard.SCHEMA}/triggers.tsv"
STRUCTURE = guard.STRUCTURE


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
        self.assertEqual((paths, growth), ([], []))

    def test_new_and_deleted_files_and_sqlite_count_sidecars(self):
        self.assertTrue(guard.compare_snapshots({}, {COUNT: "new.rs\t1\n"})[1])
        self.assertFalse(guard.compare_snapshots({COUNT: "old.rs\t1\n"}, {})[1])
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


class Events(unittest.TestCase):
    def test_pr_uses_the_target_sha_and_push_uses_the_first_parent(self):
        sha = "a" * 40
        self.assertEqual(guard.event_base("pull_request", {"pull_request": {"base": {"sha": sha}, "head": {"sha": "b" * 40}}}), sha)
        self.assertEqual(guard.event_base("push", {"before": "c" * 40}), "HEAD^")
        self.assertEqual(guard.event_base("merge_group", {"merge_group": {"base_sha": sha}}), sha)

    def test_unsupported_or_missing_baselines_are_not_silently_skipped(self):
        for name, event in (("workflow_dispatch", {}), ("pull_request", {}), ("pull_request", []),
                            ("pull_request", {"pull_request": []}),
                            ("merge_group", {"merge_group": {"base_sha": "bad"}})):
            with self.assertRaises(guard.InventoryError):
                guard.event_base(name, event)


class Histories(unittest.TestCase):
    def setUp(self):
        scratch = guard.ROOT / ".scratch"
        scratch.mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="inventory-git-test-", dir=scratch)
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
            "-c", "core.hooksPath=/dev/null", *args], env=self.env, text=True, stderr=subprocess.PIPE).strip()

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
        self.commit("Reviewed increase\n\nInventory-Growth: new reviewed source ownership")
        report = self.report()
        self.assertTrue(report["growth"])
        self.assertEqual(report["markers"][0][1], "new reviewed source ownership")
        self.base = self.git("rev-parse", "HEAD")
        self.put(COUNT, "a.rs\t4\n")
        self.commit("A later increase cannot inherit an older approval")
        self.assertTrue(self.report()["growth"])
        self.assertFalse(self.report()["markers"])

    def test_marker_in_subject_or_body_and_empty_reason_do_not_authorize_growth(self):
        for message in ("Inventory-Growth: subject only", "Explain\n\nInventory-Growth: quoted in body\n\nUnrelated final paragraph", "Empty\n\nInventory-Growth:"):
            self.put(COUNT, "a.rs\t3\n")
            self.commit(message)
            self.assertFalse(self.report()["markers"], message)

    def test_marker_cannot_authorize_bad_inventory_format(self):
        self.put(COUNT, "a.rs\t0\n")
        self.commit("Bad data\n\nInventory-Growth: reason cannot bypass parsing")
        with self.assertRaises(guard.InventoryError):
            self.report()

    def test_marker_example_in_a_file_is_not_a_commit_trailer(self):
        self.put(COUNT, "a.rs\t3\n")
        self.put("README.md", "Inventory-Growth: documentation is not approval\n")
        self.commit("Document the marker format")
        self.assertTrue(self.report()["growth"])
        self.assertFalse(self.report()["markers"])

    def test_cli_rejects_unmarked_growth_and_accepts_explicit_marker(self):
        self.put(COUNT, "a.rs\t3\n")
        self.commit("No marker")
        command = [sys.executable, str(guard.ROOT / "scripts/check-inventory-growth.py"), "--repo", str(self.repo), "--base", self.base]
        result = subprocess.run(command, capture_output=True, text=True, env=self.env)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("requires a reviewed Inventory-Growth", result.stderr)
        self.commit("Intentional approval\n\nInventory-Growth: acknowledge this candidate's source debt")
        result = subprocess.run(command, capture_output=True, text=True, env=self.env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("explicit growth exception", result.stdout)

    def test_invalid_commit_fails_instead_of_treating_history_as_an_empty_inventory(self):
        with self.assertRaisesRegex(guard.InventoryError, "cannot resolve comparison commit"):
            guard.check(self.repo, "does-not-exist")

    def test_event_payload_cli_uses_pr_target_and_handles_push(self):
        self.put(COUNT, "a.rs\t3\n")
        self.commit("Unmarked PR growth")
        event = self.repo / ".git/event.json"
        event.write_text(json.dumps({"pull_request": {"base": {"sha": self.base}}}))
        command = [sys.executable, str(guard.ROOT / "scripts/check-inventory-growth.py"), "--repo", str(self.repo), "--event-file", str(event)]
        env = dict(self.env, GITHUB_EVENT_NAME="pull_request")
        result = subprocess.run(command, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"inventory comparison: {self.base}..", result.stdout)
        event.write_text(json.dumps({"before": "0" * 40}))
        env["GITHUB_EVENT_NAME"] = "push"
        result = subprocess.run(command, env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"inventory comparison: {self.base}..", result.stdout)


if __name__ == "__main__":
    unittest.main()
