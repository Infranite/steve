#!/usr/bin/env python3
"""Safety and round-trip checks; run with Python 3.11+ -I."""

import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("restore_drill", Path(__file__).with_name("restore_drill.py"))
d = importlib.util.module_from_spec(spec)
spec.loader.exec_module(d)


class RestoreTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()

    def source(self):
        home = self.root / "source"
        (home / "data").mkdir(parents=True)
        with sqlite3.connect(home / "data/steve.sqlite3") as db:
            db.execute('CREATE TABLE "strange""table" (v TEXT)')
            db.execute('INSERT INTO "strange""table" VALUES (?)', ("marker",))
        return home

    def test_backup_restore_all_parts_and_counts(self):
        home = self.source()
        expected_files = d.fixtures(home, "synthetic-marker")
        database, archive = d.script_backup(home, 18760)
        with tarfile.open(archive) as tar:
            self.assertNotIn("signing/keychain-pass", tar.getnames())
            self.assertIn("deliveries/restore-drill.txt", tar.getnames())
        target = self.root / "target"
        d.restore(database, archive, target)
        self.assertEqual(d.state(database), d.state(target / "data/steve.sqlite3"))
        self.assertEqual(d.state(database)["quick_check"], ["ok"])
        for name in expected_files:
            self.assertEqual((home / name).read_bytes(), (target / name).read_bytes())
        self.assertFalse((target / "signing/keychain-pass").exists())
        with self.assertRaises(FileExistsError):
            d.restore(database, archive, target)

    def test_reject_production_paths_ports_and_symlink_alias(self):
        for name in (".steve", ".steve-dev/child", ".steve-copy/data"):
            with self.assertRaises(ValueError):
                d.boundary(Path.home() / name, 18760)
        for port in (8440, 8439, 8437, 8436, 65532, 80):
            with self.assertRaises(ValueError):
                d.boundary(self.root, port)
        alias = self.root / "alias"
        alias.symlink_to(Path.home() / ".steve")
        with self.assertRaises(ValueError):
            d.boundary(alias / "descendant", 18760)

    def test_archive_rejects_traversal_links_and_unexpected_parts(self):
        home = self.source()
        for index, name in enumerate(("../escaped", "/tmp/escaped", "data/steve.sqlite3",
                                      "signing/keychain-pass", "deliveries/link")):
            archive = self.root / f"bad-{index}.tar"
            with tarfile.open(archive, "w") as tar:
                info = tarfile.TarInfo(name)
                if name.endswith("link"):
                    info.type = tarfile.SYMTYPE
                    info.linkname = "../../escaped"
                    tar.addfile(info)
                else:
                    info.size = 1
                    tar.addfile(info, io.BytesIO(b"x"))
            with self.assertRaises(ValueError):
                d.restore(home / "data/steve.sqlite3", archive, self.root / f"target-{index}")
        self.assertFalse((self.root / "escaped").exists())

    def test_environment_drops_credentials_and_runtime_uses_explicit_home(self):
        home = self.source()
        with patch.dict(os.environ, {"AWS_SECRET_ACCESS_KEY": "must-not-inherit", "STEVE_HOME": "unsafe"}):
            env = d.isolated_environment(home, 18760, "on")
        self.assertNotIn("AWS_SECRET_ACCESS_KEY", env)
        for name in d.r.isolation.ADAPTERS:
            self.assertEqual(env[f"STEVE_ADAPTER_{name}"], "/usr/bin/false")
        self.assertEqual(env["STEVE_UPDATE_CHECK"], "0")
        self.assertEqual(env["STEVE_HOME"], str(home))
        self.assertEqual(env["STEVE_NO_RESUME"], "1")
        self.assertEqual(env["STEVE_BACKUP"], "on")
        self.assertEqual(env["PATH"], "/usr/bin:/bin:/usr/sbin:/sbin")
        with self.assertRaises((AssertionError, ValueError)):
            d.Process(Path("/usr/bin/false"), {**env, "STEVE_PORT": "8440"}, home / "bad.log", home)

    def test_default_method_fallback_and_backup_api_matching(self):
        home = self.source()
        d.fixtures(home, "marker")
        class FakeProcess:
            def __init__(self):
                self.signals = []
            def send(self, sig):
                self.signals.append(sig)
        process = FakeProcess()
        report = {"checks": []}
        def status(*args):
            db = next((home / "backups").glob("*.sqlite3"))
            return {"daily": True, "last": {"name": db.name, "bytes": db.stat().st_size}}
        with patch.object(d, "api", side_effect=status):
            d.backup(home, 18760, "hub", process, None, "", report)
        self.assertEqual(report["backup"]["method"], "script")
        self.assertIn("NO_RESUME", report["backup"]["fallback_reason"])
        self.assertEqual(process.signals, [signal.SIGSTOP, signal.SIGCONT])
        self.assertEqual(report["checks"][0]["status"], "PASS")

    def test_real_process_group_cleanup_including_child(self):
        home = self.source()
        program = home / "fake-hub"
        program.write_text("#!/bin/sh\nsleep 60 &\nwait\n")
        program.chmod(0o700)
        process = d.Process(program, d.isolated_environment(home, 18760, "off"), home / "hub.log", home)
        self.addCleanup(lambda: process.send(signal.SIGKILL))
        result = process.stop(2)
        self.assertTrue(result["process_group_gone"])
        self.assertIsNotNone(process.p.poll())

    def test_failure_writes_report_without_private_paths(self):
        home = self.source()
        args = type("Args", (), {"hub_bin": Path("/usr/bin/false"),
                 "snapshot": home / "data/steve.sqlite3", "work": self.root / "work",
                 "out_dir": self.root / "out", "port": 18760, "method": "hub"})()
        # Fixture intentionally lacks users: preparation fails before any Hub launch.
        self.assertEqual(d.run(args), 1)
        report_path = args.out_dir / "restore_report.json"
        text = report_path.read_text()
        self.assertEqual(json.loads(text)["verdict"], "FAIL")
        self.assertNotIn(str(self.root), text)
        self.assertTrue((args.out_dir / "restore_report.md").read_text().startswith("FAIL\n"))

    def test_sigterm_cleans_started_group_and_writes_failure(self):
        home = self.source()
        with sqlite3.connect(home / "data/steve.sqlite3") as db:
            db.execute("CREATE TABLE users (id TEXT, subject TEXT, role TEXT, created INTEGER)")
            db.execute("INSERT INTO users VALUES ('u', 'test', 'owner', 1)")
        program = self.root / "fake-hub"
        program.write_text('#!/bin/sh\necho $$ > "$STEVE_HOME/started.pid"\nsleep 60 &\nwait\n')
        program.chmod(0o700)
        work, out = self.root / "work", self.root / "out"
        proc = subprocess.Popen([sys.executable, "-I", str(Path(d.__file__)),
                "--hub-bin", str(program), "--snapshot", str(home / "data/steve.sqlite3"),
                "--work", str(work), "--out-dir", str(out), "--port", "18760"],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.addCleanup(lambda: proc.kill() if proc.poll() is None else None)
        deadline = time.monotonic() + 10
        pidfile = work / "source/started.pid"
        while not pidfile.exists() and time.monotonic() < deadline and proc.poll() is None:
            time.sleep(.02)
        self.assertTrue(pidfile.exists())
        group = int(pidfile.read_text())
        self.addCleanup(lambda: self.kill_group_if_present(group))
        proc.send_signal(signal.SIGTERM)
        stdout, stderr = proc.communicate(timeout=15)
        self.assertEqual(proc.returncode, 1, (stdout, stderr))
        report = json.loads((out / "restore_report.json").read_text())
        self.assertEqual(report["verdict"], "FAIL")
        cleanup = next(c for c in report["checks"] if c["name"] == "source_cleanup")
        self.assertTrue(cleanup["evidence"]["process_group_gone"])
        with self.assertRaises(ProcessLookupError):
            os.killpg(group, 0)

    @staticmethod
    def kill_group_if_present(group):
        try:
            os.killpg(group, signal.SIGKILL)
        except ProcessLookupError:
            pass


if __name__ == "__main__":
    os.umask(0o077)
    unittest.main()
