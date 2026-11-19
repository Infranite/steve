#!/usr/bin/env python3
"""External backup entry points with a fake Hub and disposable homes, without services or network."""
import importlib.util
from contextlib import closing
import json
import os
from pathlib import Path
import plistlib
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
(home/"called.json").write_text(json.dumps(dict(args=sys.argv[1:],home=str(home))))
if (home/"fail").exists():
    print("fixture refuses backup",file=sys.stderr)
    sys.exit(7)
directory=home/"backups/pre-upgrade-fixture"
directory.mkdir(parents=True,exist_ok=True)
os.chmod(directory,0o700)
shutil.copyfile(home/"data/steve.sqlite3",directory/"steve.sqlite3")
os.chmod(directory/"steve.sqlite3",0o600)
manifest=dict(completed=not (home/"incomplete").exists(),directory=str(directory),
              database=dict(name="steve.sqlite3",quick_check="ok"))
(directory/"manifest.json").write_text(json.dumps(manifest))
print(json.dumps(manifest))
''')
        self.binary.chmod(0o700)
        (self.home / "current").symlink_to(self.candidate)
        self.db = self.home / "data/steve.sqlite3"
        with closing(sqlite3.connect(self.db, isolation_level=None)) as c:
            c.executescript("""CREATE TABLE users(id TEXT,subject TEXT,role TEXT,created INTEGER);
                INSERT INTO users VALUES('owner','original','owner',1);
                CREATE TABLE artifacts(id TEXT,item_id TEXT,kind TEXT,title TEXT,location TEXT,created INTEGER);
                INSERT INTO artifacts VALUES('artifact','ST-1','report','report','交付摘要',1);
                CREATE TABLE workflow_units(item_id TEXT,summary TEXT);""")
            c.execute("INSERT INTO workflow_units VALUES('ST-1',?)", ("saved full report " * 10,))
        self.addCleanup(patch.stopall)
        patch.dict(os.environ, {"STEVE_HOME": str(self.home), "HOME": str(self.root)}).start()

    def assert_call(self, reason):
        call = json.loads((self.home / "called.json").read_text())
        self.assertEqual(call, dict(args=["--admin", "backup", "--reason", reason], home=str(self.home)))

    def test_local_release_uses_candidate_manifest_and_rejects_failed_or_incomplete_backup(self):
        m = load("local-release")
        directory, database = m.hub_backup(self.candidate)
        self.assert_call("pre-upgrade")
        self.assertEqual(database, directory / "steve.sqlite3")
        self.assertEqual(database.read_bytes(), self.db.read_bytes())
        (self.home / "fail").touch()
        with self.assertRaisesRegex(m.Refused, "fixture refuses"):
            m.hub_backup(self.candidate)
        (self.home / "fail").unlink()
        (self.home / "incomplete").touch()
        with self.assertRaisesRegex(m.Refused, "incomplete"):
            m.hub_backup(self.candidate)

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

    def test_repair_backup_failure_prevents_file_and_row_changes_then_success_is_retryable(self):
        m = load("recover-delivery-outputs")
        (self.home / "fail").touch()
        with self.assertRaises(subprocess.CalledProcessError):
            m.run(self.home, True)
        self.assert_call("diagnostic")
        self.assertFalse((self.home / "workspaces").exists())
        with closing(sqlite3.connect(self.db, isolation_level=None)) as c:
            self.assertEqual(c.execute("SELECT location FROM artifacts").fetchone()[0], "交付摘要")
        (self.home / "fail").unlink()
        m.run(self.home, True)
        with closing(sqlite3.connect(self.db, isolation_level=None)) as c:
            location = c.execute("SELECT location FROM artifacts").fetchone()[0]
        self.assertEqual(Path(location).read_text(), "saved full report " * 10)
        m.run(self.home, True)

    def test_legacy_release_helper_checks_command_status_and_manifest(self):
        source = (OPS / "release.sh").read_text()
        helper = source[source.index("hub_backup() {"):source.index("\nschema() {")]
        def invoke():
            return subprocess.run(["/bin/zsh", "-fc", helper + '\nhub_backup "$1"', "zsh", str(self.binary)],
                                  env={**os.environ, "HOME_DIR": str(self.home)},
                                  text=True, capture_output=True, timeout=10)
        result = invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(self.home / "backups/pre-upgrade-fixture"))
        self.assert_call("pre-upgrade")
        (self.home / "fail").touch()
        self.assertNotEqual(invoke().returncode, 0)
        (self.home / "fail").unlink()
        (self.home / "incomplete").touch()
        self.assertNotEqual(invoke().returncode, 0)

    def test_source_install_registers_only_hub_and_no_backup_job(self):
        (self.home / "steve.env").touch()
        tools = self.root / "tools"
        tools.mkdir()
        stub = tools / "launchctl"
        stub.write_text('#!/bin/sh\nprintf "%s\\n" "$*" >> "$HOME/launchctl-calls"\n')
        stub.chmod(0o700)
        env = {**os.environ, "PATH": f"{tools}:{os.environ['PATH']}", "STEVE_LABEL": "fixture.hub"}
        subprocess.run(["/bin/zsh", str(OPS / "install.sh")], env=env, check=True, capture_output=True, timeout=10)
        agents = self.root / "Library/LaunchAgents"
        self.assertEqual([p.name for p in agents.iterdir()], ["fixture.hub.plist"])
        with (agents / "fixture.hub.plist").open('rb') as f:
            job = plistlib.load(f)
        self.assertEqual(job["ProgramArguments"], ["/bin/zsh", f"{self.home}/current/ops/run.sh"])
        self.assertEqual((self.root / "launchctl-calls").read_text().splitlines(),
                         [f"bootout gui/{os.getuid()}/fixture.hub",
                          f"bootstrap gui/{os.getuid()} {agents}/fixture.hub.plist"])
        self.assertFalse((OPS / "backup.sh").exists())
        self.assertFalse((OPS / "io.github.infranite.steve-backup.plist").exists())


if __name__ == "__main__":
    unittest.main(verbosity=2)
