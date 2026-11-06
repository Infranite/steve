#!/usr/bin/env python3
"""ops/local-release.py's part in a one-click upgrade, in a throwaway STEVE_HOME: the journal it writes for the Hub
(--operation) and the release lock it shares with ops/release.sh. Nothing is built, deployed or started."""
import json
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest
import uuid

OPS = pathlib.Path(__file__).resolve().parent
SCRIPT = OPS / "local-release.py"


class Journal(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = pathlib.Path(self.tmp.name)
        self.id = str(uuid.uuid4())
        ops = self.home / "updates/operations"
        ops.mkdir(parents=True)
        self.op = ops / f"{self.id}.json"
        self.op.write_text(json.dumps({"id": self.id, "idempotency_key": "k", "version": "0.3.0", "phase": "ready",
                                       "created_at": 1, "updated_at": 1, "requested_by": "owner", "error": None,
                                       "previous": None, "target": "/r", "runner": {"kind": "local_release"}}))

    def tearDown(self):
        self.tmp.cleanup()

    def run_script(self, *args):
        env = {k: os.environ[k] for k in ("PATH", "TMPDIR") if k in os.environ}
        env.update(HOME=str(self.home), STEVE_HOME=str(self.home))
        return subprocess.run([sys.executable, "-I", str(SCRIPT), *args], env=env, capture_output=True, text=True, timeout=60)

    def module(self):
        os.environ["STEVE_HOME"] = str(self.home)
        import importlib.util
        spec = importlib.util.spec_from_file_location("local_release", SCRIPT)
        m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(m)
        return m

    def test_each_step_keeps_the_hubs_fields_and_stays_private(self):
        m = self.module()
        j = m.Journal(self.id)
        j("draining")
        j("backing_up", backup="/b/pre.sqlite3")
        j("restarting", previous="/old")
        op = json.loads(self.op.read_text())
        self.assertEqual((op["phase"], op["backup"], op["previous"], op["target"]), ("restarting", "/b/pre.sqlite3", "/old", "/r"))
        self.assertEqual(op["runner"], {"kind": "local_release"}, "the Hub's own fields are kept")
        self.assertGreater(op["updated_at"], 1)
        self.assertEqual(stat.S_IMODE(self.op.stat().st_mode), 0o600)
        self.assertFalse(j.terminal())
        j("succeeded")
        self.assertTrue(j.terminal())
        self.assertEqual(list(self.op.parent.glob(".*.tmp")), [])
        # without an id nothing is written; an id is only ever an operation id
        m.Journal(None)("failed")
        with self.assertRaises(ValueError):
            m.Journal("../../etc/passwd")
        with self.assertRaises(m.Refused):
            m.Journal(str(uuid.uuid4()))

    def test_a_deploy_refused_before_anything_changed_says_so_in_the_journal(self):
        # no signing identity in this STEVE_HOME: refused before the drain
        r = self.run_script("deploy", str(self.home / "releases/none"), "--operation", self.id)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        op = json.loads(self.op.read_text())
        self.assertEqual(op["phase"], "failed")
        self.assertTrue(op["error"].startswith("没有改动："), op["error"])

    def test_one_release_at_a_time_with_release_sh(self):
        lock = self.home / "releases/.release.lock"
        lock.parent.mkdir(parents=True)
        lock.touch()
        holder = subprocess.Popen([sys.executable, "-c", "import fcntl,sys,time; f=open(sys.argv[1],'a'); "
                                   "fcntl.lockf(f,fcntl.LOCK_EX); print('held',flush=True); time.sleep(30)", str(lock)],
                                  stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(holder.stdout.readline().strip(), "held")
            r = self.run_script("deploy", str(self.home / "releases/none"), "--operation", self.id)
            self.assertEqual(r.returncode, 1)
            self.assertIn("另一个发布正在进行", r.stdout)
            self.assertIn("另一个发布正在进行", json.loads(self.op.read_text())["error"])
        finally:
            holder.kill()
            holder.wait()


if __name__ == "__main__":
    unittest.main()
