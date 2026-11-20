#!/usr/bin/env python3
"""Inventory comparison on constructed snapshots and isolated Git histories; no Cargo builds."""
import importlib.util
import json
import os
from pathlib import Path
import re
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
        self.commit(f"Reviewed increase\n\nInventory-Growth: {COUNT}: new reviewed source ownership")
        report = self.report()
        self.assertTrue(report["growth"])
        self.assertEqual(report["markers"][0]["reason"], "new reviewed source ownership")
        self.base = self.git("rev-parse", "HEAD")
        self.put(COUNT, "a.rs\t4\n")
        self.commit("A later increase cannot inherit an older approval")
        self.assertTrue(self.report()["growth"])
        self.assertFalse(self.report()["markers"])

    def test_marker_in_subject_or_body_and_empty_reason_do_not_authorize_growth(self):
        for message in (f"Inventory-Growth: {COUNT}: subject only", f"Explain\n\nInventory-Growth: {COUNT}: quoted in body\n\nUnrelated final paragraph", f"Empty\n\nInventory-Growth: {COUNT}:", "Old format\n\nInventory-Growth: global exception"):
            self.put(COUNT, "a.rs\t3\n")
            self.commit(message)
            self.assertFalse(self.report()["markers"], message)

    def test_marker_cannot_authorize_bad_inventory_format(self):
        self.put(COUNT, "a.rs\t0\n")
        self.commit(f"Bad data\n\nInventory-Growth: {COUNT}: reason cannot bypass parsing")
        with self.assertRaises(guard.InventoryError):
            self.report()

    def test_marker_example_in_a_file_is_not_a_commit_trailer(self):
        self.put(COUNT, "a.rs\t3\n")
        self.put("README.md", f"Inventory-Growth: {COUNT}: documentation is not approval\n")
        self.commit("Document the marker format")
        self.assertTrue(self.report()["growth"])
        self.assertFalse(self.report()["markers"])

    def test_cli_rejects_unmarked_growth_and_accepts_explicit_marker(self):
        self.put(COUNT, "a.rs\t3\n")
        self.commit("No marker")
        command = [sys.executable, str(guard.ROOT / "scripts/check-inventory-growth.py"), "--repo", str(self.repo), "--base", self.base]
        result = subprocess.run(command, capture_output=True, text=True, env=self.env)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("Inventory-Growth: <path>: <reason>", result.stderr)
        self.commit(f"Intentional approval\n\nInventory-Growth: {COUNT}: acknowledge this candidate's source debt")
        result = subprocess.run(command, capture_output=True, text=True, env=self.env)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("path-scoped review markers", result.stdout)

    def test_invalid_commit_fails_instead_of_treating_history_as_an_empty_inventory(self):
        with self.assertRaisesRegex(guard.InventoryError, "cannot resolve comparison commit"):
            guard.check(self.repo, "does-not-exist")

    def test_markers_name_each_growing_path_including_initial_imports(self):
        other = "web/test/harness-parity.txt"
        self.put(COUNT, "a.rs\t3\n")
        self.commit(f"Review A\n\nInventory-Growth: {COUNT}: reviewed increase")
        self.put(other, 'src/a.ts\t"codex"\t1\n')
        self.commit("Import B later without a marker")
        report = self.report()
        self.assertEqual(report["unreviewed"], [other])
        self.commit(f"Review the new path\n\nInventory-Growth: {other}: initial import")
        self.assertFalse(self.report()["unreviewed"])

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
                    head = self.commit(f"Reviewed first-push change\n\nInventory-Growth: {COUNT}: {label}")
                    event = {"before": "0" * 40}
                    covered = {head}
                else:
                    self.put(COUNT, "a.rs\t3\n")
                    first = self.commit(f"Reviewed growth in first commit\n\nInventory-Growth: {COUNT}: {label}")
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
        source = self.commit(f"Feature\n\nInventory-Growth: {COUNT}: reviewed feature")
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
        head = self.commit(f"Replacement\n\nInventory-Growth: {COUNT}: reviewed replacement")
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


class WorkflowWiring(unittest.TestCase):
    def test_macos_guard_runs_after_test_reports_even_when_tests_fail(self):
        workflow = (guard.ROOT / ".github/workflows/ci.yml").read_text()
        steps = re.split(r"(?m)^      - ", workflow)
        guards = [step for step in steps if "run: python3 scripts/structure_guard.py" in step]
        self.assertEqual(len(guards), 1)
        condition = next(line for line in guards[0].splitlines() if line.strip().startswith("if:"))
        for required in ("!cancelled()", "steps.rust-build.outcome == 'success'", "runner.os == 'macOS'", "matrix.shard == 1"):
            self.assertIn(required, condition)
        position = workflow.index("run: python3 scripts/structure_guard.py")
        self.assertLess(workflow.index("id: rust-tests"), position)
        self.assertLess(workflow.index("python3 scripts/nextest-summary.py"), position)
        self.assertLess(workflow.index("uses: actions/upload-artifact@"), position)
        components = re.findall(r"(?m)^\s+components: (.+)$", workflow)
        self.assertEqual(len(components), 1)
        self.assertIn("runner.os == 'macOS' && matrix.shard == 1", components[0])
        self.assertIn("rustfmt,clippy", components[0])


if __name__ == "__main__":
    unittest.main()
