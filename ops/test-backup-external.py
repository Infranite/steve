#!/usr/bin/env python3
"""External backup entry point (`ops/dev.py snapshot`) with a fake Hub and disposable homes."""
import importlib.util
from contextlib import closing
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

OPS = Path(__file__).resolve().parent


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace('-', '_'), OPS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ExternalBackup(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory(prefix="steve-backup-external-")
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name).resolve()
        self.home = self.root / "installation with spaces"
        (self.home / "data").mkdir(parents=True)
        self.candidate = self.home / "candidate"
        self.binary = self.candidate / "bin/steve-hub"
        self.binary.parent.mkdir(parents=True)
        # This fake exercises the process/JSON contract, not SQLite's backup implementation.
        self.binary.write_text(f'''#!{sys.executable}
import json,os,pathlib,shutil,sys
home=pathlib.Path(os.environ["STEVE_HOME"])
args=sys.argv[1:]
if args[1]=="restore":
    assert args[2:] == ["a"*64, "--to", args[4]]
    stage=pathlib.Path(args[4]).resolve()/"restored"
    (stage/"data").mkdir(parents=True)
    shutil.copyfile(home/"data/steve.sqlite3",stage/"data/steve.sqlite3")
    print(stage)
    sys.exit(0)
(home/"called.json").write_text(json.dumps(dict(args=args,home=str(home))))
if (home/"fail").exists():
    print("fixture refuses backup",file=sys.stderr)
    sys.exit(7)
print(json.dumps(dict(snapshot_id="a"*64,purpose=args[3],
                      database=dict(sha256="b"*64,bytes=0 if (home/"incomplete").exists() else 1))))
''')
        self.binary.chmod(0o700)
        (self.home / "current").symlink_to(self.candidate)
        self.db = self.home / "data/steve.sqlite3"
        with closing(sqlite3.connect(self.db, isolation_level=None)) as c:
            c.executescript("""CREATE TABLE users(id TEXT,subject TEXT,role TEXT,created INTEGER);
                INSERT INTO users VALUES('owner','original','owner',1);""")
        self.addCleanup(patch.stopall)
        patch.dict(os.environ, {"STEVE_HOME": str(self.home), "HOME": str(self.root)}).start()

    def assert_call(self, reason, offline=False):
        call = json.loads((self.home / "called.json").read_text())
        self.assertEqual(call, dict(args=["--admin", "backup", "--reason", reason] + (["--offline"] if offline else []), home=str(self.home)))

    def test_dev_snapshot_changes_only_the_copy_and_failure_preserves_it(self):
        m = load("dev")
        m.DEV = self.root / "development"
        m.DEV.mkdir()
        m.MODE = m.DEV / "mode"
        m.HUB_BIN = self.binary
        original = self.db.read_bytes()
        m.snapshot(self.home)
        self.assert_call("diagnostic")
        dst = m.DEV / "data/steve.sqlite3"
        with closing(sqlite3.connect(dst)) as c:
            self.assertEqual(c.execute("SELECT subject FROM users").fetchone()[0], "dev:local")
        self.assertEqual(self.db.read_bytes(), original)
        saved = dst.read_bytes()
        (self.home / "fail").touch()
        with self.assertRaises(subprocess.CalledProcessError):
            m.snapshot(self.home)
        self.assertEqual(dst.read_bytes(), saved)


if __name__ == "__main__":
    unittest.main(verbosity=2)
