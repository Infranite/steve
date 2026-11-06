#!/usr/bin/env python3
"""ops/local-release.py prune, in a throwaway STEVE_HOME: which pre-release backups and release dirs go after a
healthy deploy, and which never do. Nothing is built, deployed or started."""
import fcntl
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

OPS = pathlib.Path(__file__).resolve().parent
SCRIPT = OPS / "local-release.py"


def load(home):
    os.environ["STEVE_HOME"] = str(home)
    spec = importlib.util.spec_from_file_location("local_release_prune_under_test", SCRIPT)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


class Prune(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = pathlib.Path(self.tmp.name)
        self.backups = self.home / "backups"
        self.releases = self.home / "releases"
        self.backups.mkdir()
        self.releases.mkdir()
        self.m = load(self.home)

    def tearDown(self):
        self.tmp.cleanup()

    def touch(self, *names):
        for n in names:
            p = self.backups / n
            if n.endswith("/"):
                p.mkdir()
                (p / "steve.sqlite3").write_text("x")
            else:
                p.write_text("x")

    def release(self, *names):
        for n in names:
            (self.releases / n / "bin").mkdir(parents=True)
            (self.releases / n / "web/dist").mkdir(parents=True)

    def names(self, d):
        return sorted(p.name for p in d.iterdir())

    def test_keeps_the_newest_backups_with_their_side_files_and_nothing_else_is_touched(self):
        local = [f"pre-0.2.{n}-local-{n:07x}-202611{n:02d}-120000.sqlite3" for n in range(1, 13)]
        self.touch(*local)
        self.touch(local[0] + "-wal", local[0] + "-shm", local[11] + "-wal", local[11] + "-shm")
        # a side file whose database is gone, newer than everything
        orphan = "pre-0.2.99-local-abcdef0-20261130-120000.sqlite3"
        self.touch(orphan + "-wal", orphan + "-shm")
        others = ["pre-rust-20261101-010101-36161f9b.sqlite3", "pre-rust-20261101-010101-36161f9b.sqlite3-wal",
                  "pre-purge-1793631050067.sqlite3", "pre-takeover-20261102-010101.sqlite3",
                  "pre-models-20261102-010101.sqlite3", "pre-20261101-020547-1d16430c.sqlite3",
                  "steve-20261105.sqlite3", "failed-probe-0.2.14-local-b14d9c0-20261104-152133/"]
        self.touch(*others)

        removed = self.m.prune()

        kept = self.names(self.backups)
        for n in local[4:]:
            self.assertIn(n, kept)
        self.assertIn(local[11] + "-wal", kept)
        self.assertIn(local[11] + "-shm", kept)
        for n in local[:4] + [local[0] + "-wal", local[0] + "-shm", orphan + "-wal", orphan + "-shm"]:
            self.assertNotIn(n, kept)
            self.assertIn(str(self.backups / n), removed)
        for n in others:
            self.assertIn(n.rstrip("/"), kept)
        self.assertEqual(len([n for n in kept if n.endswith(".sqlite3") and "-local-" in n]), self.m.KEEP_BACKUPS)

    def test_keeps_current_previous_and_the_newest_three_release_dirs(self):
        self.release("0.2.5-local-8c3d835", "0.2.10-local-e319e77", "0.2.47-local-0b0ffe9", "0.2.54-local-3c1c1e0",
                     "0.2.55-local-9af7df7", "0.2.56-local-7c3020e", "20261101-020547-1d16430c",
                     "20261103-002107-c2ee544c", "0.9.0-a")
        (self.releases / "history.jsonl").write_text("{}\n")
        (self.releases / ".release.lock").write_text("")
        # rolled back: current is an old one, the previous is the one it came from
        (self.home / "current").symlink_to(self.releases / "0.2.5-local-8c3d835")
        (self.home / "web").mkdir()
        (self.home / "web/current").symlink_to(self.releases / "0.2.47-local-0b0ffe9/web/dist")

        removed = self.m.prune(keep=[self.releases / "0.2.10-local-e319e77"])

        self.assertEqual(self.names(self.releases), sorted([
            ".release.lock", "0.2.5-local-8c3d835", "0.2.10-local-e319e77", "0.2.47-local-0b0ffe9",
            "0.2.54-local-3c1c1e0", "0.2.55-local-9af7df7", "0.2.56-local-7c3020e", "0.9.0-a", "history.jsonl"]))
        self.assertEqual(sorted(pathlib.Path(p).name for p in removed),
                         ["20261101-020547-1d16430c", "20261103-002107-c2ee544c"])

    def test_versions_rank_as_numbers_not_text(self):
        self.release("0.2.9-local-2ecd29d", "0.2.10-local-e319e77", "0.2.11-local-1d86008", "0.2.100-local-aaaaaaa")
        self.m.prune()
        self.assertEqual(self.names(self.releases),
                         ["0.2.10-local-e319e77", "0.2.100-local-aaaaaaa", "0.2.11-local-1d86008"])

    def test_dry_run_lists_and_removes_nothing(self):
        self.release(*[f"0.3.{n}-local-{n:07x}" for n in range(6)])
        self.touch(*[f"pre-0.3.{n}-local-{n:07x}-202611{n + 1:02d}-120000.sqlite3" for n in range(10)])
        before = (self.names(self.releases), self.names(self.backups))
        would = self.m.prune(dry=True)
        self.assertEqual(len(would), 3 + 2)
        self.assertEqual((self.names(self.releases), self.names(self.backups)), before)

    def test_a_failure_while_pruning_is_logged_never_the_deploys_result(self):
        saved = {}

        def broken(**kw):
            raise PermissionError("denied")
        self.m.prune = broken
        self.m.prune_after(self.releases / "x", lambda **kw: saved.update(kw))
        self.assertIn("denied", saved["pruned_error"])

    def run_script(self, *args):
        env = {k: os.environ[k] for k in ("PATH", "TMPDIR") if k in os.environ}
        env.update(HOME=str(self.home), STEVE_HOME=str(self.home))
        return subprocess.run([sys.executable, "-I", str(SCRIPT), *args], env=env, capture_output=True, text=True,
                              timeout=60)

    def test_prune_command_takes_the_release_lock(self):
        self.release(*[f"0.4.{n}-local-{n:07x}" for n in range(5)])
        with open(self.releases / ".release.lock", "a") as f:
            fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
            r = self.run_script("prune")
            self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
            self.assertEqual(len(self.names(self.releases)), 6)
        r = self.run_script("prune", "--dry-run")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(r.stdout.count("would remove "), 2)
        self.assertEqual(len(self.names(self.releases)), 6)
        r = self.run_script("prune")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.names(self.releases),
                         [".release.lock", "0.4.2-local-0000002", "0.4.3-local-0000003", "0.4.4-local-0000004"])

    def test_prune_command_keeps_the_previous_release_of_the_last_healthy_deploy(self):
        # the first local deploy came over an ops/release.sh dir (ranked below any local build), then 3 builds
        # were made and not deployed: the rollback target is outside the newest 3
        self.release("20261103-002107-c2ee544c", "0.5.0-local-0000000", "0.5.1-local-0000001",
                     "0.5.2-local-0000002", "0.5.3-local-0000003")
        (self.home / "current").symlink_to(self.releases / "0.5.0-local-0000000")
        logs = self.home / "logs/release"
        logs.mkdir(parents=True)
        (logs / "local-0.4.9-20261101-100000.json").write_text(json.dumps(
            dict(ok=True, previous=str(self.releases / "0.4.8-local-gone000"), finished="20261101-100500")))
        (logs / "local-0.5.0-20261104-100000.json").write_text(json.dumps(
            dict(ok=True, previous=str(self.releases / "20261103-002107-c2ee544c"), finished="20261104-100500")))
        (logs / "local-0.5.1-20261105-100000.json").write_text(json.dumps(
            dict(ok=False, previous=str(self.releases / "0.5.0-local-0000000"))))
        r = self.run_script("prune")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(self.names(self.releases), [
            ".release.lock", "0.5.0-local-0000000", "0.5.1-local-0000001", "0.5.2-local-0000002", "0.5.3-local-0000003",
            "20261103-002107-c2ee544c"])

    def test_prune_command_without_local_deploys_keeps_release_sh_previous(self):
        self.release("20261101-020547-1d16430c", "20261102-020547-2d16430c", "20261103-002107-c2ee544c",
                     *[f"0.6.{n}-local-{n:07x}" for n in range(3)])
        (self.home / "current").symlink_to(self.releases / "20261103-002107-c2ee544c")
        (self.releases / "history.jsonl").write_text("\n".join(json.dumps(h) for h in [
            dict(kind="hub", ok=True, prev="20261101-020547-1d16430c"),
            dict(kind="web", ok=True, prev="x"),
            dict(kind="hub", ok=False, prev="20261102-020547-2d16430c")]) + "\n")
        self.assertEqual(self.m.last_previous(), self.releases / "20261101-020547-1d16430c")
        r = self.run_script("prune")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(r.stdout.strip(), f"removed {self.releases / '20261102-020547-2d16430c'}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
