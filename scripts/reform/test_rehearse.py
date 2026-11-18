#!/usr/bin/env python3
"""Contract tests using a disposable fake Hub; no production services or data."""
import argparse
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest

SPEC = importlib.util.spec_from_file_location("rehearse", Path(__file__).with_name("rehearse.py"))
rehearse = importlib.util.module_from_spec(SPEC)
sys.dont_write_bytecode = True
SPEC.loader.exec_module(rehearse)

FAKE_HUB = r'''
import http.server, json, os, signal, sqlite3, sys
from pathlib import Path
assert "TEST_UNTRUSTED_ENV" not in os.environ
assert os.environ["STEVE_NO_RESUME"] == "1"
home = Path(os.environ["STEVE_HOME"])
assert home != Path.home()
if "--build-info" in sys.argv:
    print(json.dumps({"schema": 2, "commit": "fixture"}))
    sys.exit(0)
db = sqlite3.connect(home / "data/steve.sqlite3")
before = db.execute("SELECT v FROM schema_version").fetchone()[0]
if before < 2:
    db.execute("CREATE TABLE added(id INTEGER)")
    db.execute("UPDATE schema_version SET v=2")
    db.commit()
    print("INFO migrated database from=1 to=2", flush=True)
db.close()
print("INFO outbox dispatcher started", flush=True)
print("WARN fixture warning", flush=True)
(home / "fixture.pid").write_text(str(os.getpid()))
class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args): pass
    def do_GET(self):
        status, body = 200, {"ok": True, "version": "fixture", "build_commit": "fixture"}
        if self.path == "/auth/dev":
            self.send_response(302)
            self.send_header("Set-Cookie", "steve_session=x; Path=/; HttpOnly")
            self.send_header("Location", "/")
            self.end_headers()
            return
        if self.path.startswith("/api/") and "steve_session=x" not in self.headers.get("Cookie", ""):
            status = 401
        if self.path == "/api/error":
            status = 500
            print("ERROR fixture failure", flush=True)
            print("thread main panicked at fixture", flush=True)
        if self.path == "/api/redirect":
            self.send_response(302)
            self.send_header("Location", "/healthz")
            self.end_headers()
            return
        if self.path == "/": status = 404
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(json.dumps(body).encode())
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
http.server.HTTPServer.allow_reuse_address = True
http.server.HTTPServer(("127.0.0.1", int(os.environ["STEVE_PORT"])), Handler).serve_forever()
'''


class RehearsalTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binary = self.root / "fake-hub"
        self.binary.write_text(f"#!{sys.executable}\n" + FAKE_HUB)
        self.binary.chmod(0o700)
        self.snapshot = self.root / "snapshot.sqlite3"
        with contextlib.closing(sqlite3.connect(self.snapshot)) as db, db:
            db.executescript('''CREATE TABLE schema_version(v INTEGER);
                INSERT INTO schema_version VALUES(1);
                CREATE TABLE users(id TEXT, subject TEXT, role TEXT, created INTEGER);
                INSERT INTO users VALUES('owner', 'original', 'owner', 1);
                CREATE TABLE "odd""name"(value INTEGER);
                INSERT INTO "odd""name" VALUES(1);''')

    def args(self, suffix="one", endpoints=None, tasks=None):
        return argparse.Namespace(hub_bin=self.binary, snapshot=self.snapshot,
            work=self.root / ("home-" + suffix), out_dir=self.root / ("out-" + suffix),
            label="fixture", port=18761, timeout=3, observe_seconds=.03,
            endpoints=endpoints, expect_tasks=tasks, baseline=None)

    def execute(self, args):
        with contextlib.redirect_stdout(io.StringIO()):
            status = rehearse.run(args)
        return status, rehearse.read_json(args.out_dir / "report.json")

    def test_full_upgrade_restart_cookie_and_compare(self):
        original = self.snapshot.read_bytes()
        args = self.args()
        status, report = self.execute(args)
        self.assertEqual(status, 0)
        self.assertEqual(report["verdict"], "PASS")
        self.assertEqual(report["before"]["schema"], 1)
        self.assertEqual(report["after"]["schema"], 2)
        self.assertEqual(report["starts"][0]["logs"]["migrations"], [{"from": 1, "to": 2}])
        self.assertEqual(report["starts"][1]["logs"]["migrations"], [])
        self.assertEqual(len(report["starts"][0]["endpoints"]), 11)
        self.assertTrue(all(r["status"] == 200 for r in report["starts"][0]["endpoints"]))
        self.assertTrue(all(r["shutdown"]["process_group_gone"] for r in report["starts"]))
        self.assertEqual(self.snapshot.read_bytes(), original)
        self.assertEqual(report["after"]["tables"]['odd"name'], 1)
        markdown = (args.out_dir / "report.md").read_text()
        self.assertTrue(markdown.startswith("PASS\n"))
        self.assertNotIn(str(self.root), markdown)
        self.assertNotIn("DIFF", rehearse.compare(report, report))
        with self.assertRaises(ValueError):
            rehearse.run(args)

    def test_strict_tasks_and_failure_evidence(self):
        config = self.root / "config.json"
        config.write_text(json.dumps({"endpoints": ["/api/error", "/api/redirect"],
            "expected_tasks": [{"name": "absent", "log_pattern": "never started"}]}))
        status, report = self.execute(self.args(endpoints=config))
        self.assertEqual(status, 1)
        checks = {c["name"]: c["status"] for c in report["checks"]}
        for name in ("api_200", "start_1_logs", "start_1_tasks", "start_2_tasks"):
            self.assertEqual(checks[name], "FAIL")
        self.assertEqual(report["starts"][0]["logs"]["levels"]["panic"]["count"], 1)
        self.assertEqual(report["starts"][0]["endpoints"][1]["status"], 302)

    def test_task_health_evidence_and_baseline(self):
        config = self.root / "config.json"
        config.write_text(json.dumps({"expected_tasks": [
            {"name": "outbox", "log_pattern": "outbox dispatcher started"},
            {"name": "ready", "health_path": "ok", "equals": True}]}))
        args = self.args(endpoints=config)
        status, report = self.execute(args)
        self.assertEqual(status, 0)
        second = self.args("two", endpoints=config)
        second.baseline = args.out_dir / "report.json"
        status, report = self.execute(second)
        self.assertEqual(status, 0)
        self.assertEqual(report["checks"][-1]["name"], "baseline_tasks")
        changed = json.loads(json.dumps(report))
        changed["after"]["tables"]["added"] = 5
        self.assertIn("DIFF", rehearse.compare(report, changed))

    def test_protected_home_ports_and_environment_assertion(self):
        for home in (Path.home(), Path.home() / ".steve", Path.home() / ".steve-dev/x"):
            with self.assertRaises(ValueError):
                rehearse.safe_work(home, 18761)
        link = self.root / "alias"
        link.symlink_to(Path.home() / ".steve")
        with self.assertRaises(ValueError):
            rehearse.safe_work(link / "child", 18761)
        with self.assertRaises(ValueError):
            rehearse.safe_work(self.root / "safe", 8440)
        with self.assertRaises(AssertionError):
            rehearse.Process([str(self.binary)], {}, self.root / "log", self.root)

    def test_nonempty_wal_rejected_empty_wal_allowed(self):
        wal = Path(str(self.snapshot) + "-wal")
        wal.write_bytes(b"pending data")
        home = self.root / "home"
        home.mkdir()
        with self.assertRaises(ValueError):
            rehearse.prepare(self.snapshot, home)
        wal.write_bytes(b"")
        target, evidence = rehearse.prepare(self.snapshot, home)
        self.assertTrue(target.exists())
        self.assertEqual(evidence["bytes"], self.snapshot.stat().st_size)

    def test_health_timeout_reports_failure_and_stops(self):
        self.binary.write_text(f"#!{sys.executable}\n" + FAKE_HUB.replace(
            'if self.path == "/": status = 404', 'if self.path == "/healthz": status = 503'))
        args = self.args()
        args.timeout = .3
        status, report = self.execute(args)
        self.assertEqual(status, 1)
        self.assertFalse(report["starts"][0]["healthy"])
        self.assertTrue(report["starts"][0]["shutdown"]["process_group_gone"])
        self.assertEqual(len(report["starts"]), 1)

    def test_unsupported_build_info_is_isolated_with_log_fallback(self):
        self.binary.write_text(f"#!{sys.executable}\n" + FAKE_HUB.replace(
            'if "--build-info" in sys.argv:', 'if False:'))
        args = self.args()
        args.timeout = 2
        status, report = self.execute(args)
        self.assertEqual(status, 0, report)
        self.assertEqual(report["schema_target_source"], "migration log")
        self.assertEqual(report["schema_target"], 2)
        self.assertTrue(report["build_probe"]["shutdown"]["process_group_gone"])

    def test_timeout_reaps_child_group(self):
        home = self.root / "timeout"
        home.mkdir()
        env = rehearse.environment(home, 18761)
        script = """import signal, subprocess, sys, time
p = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
def stop(*args):
    p.wait(timeout=3)
    sys.exit(0)
signal.signal(signal.SIGTERM, stop)
print('ready', flush=True)
time.sleep(60)
"""
        proc = rehearse.Process([sys.executable, "-c", script], env, home / "log", home)
        try:
            deadline = time.monotonic() + 3
            while "ready" not in (home / "log").read_text() and time.monotonic() < deadline:
                time.sleep(.02)
        finally:
            stopped = proc.stop(3)
        self.assertEqual(stopped["exit_code"], 0)
        self.assertTrue(stopped["process_group_gone"])

    def test_interruption_emits_failure_report_and_cleans_up(self):
        args = self.args()
        cmd = [sys.executable, "-I", str(Path(rehearse.__file__)), "run", "--hub-bin", str(self.binary),
               "--snapshot", str(self.snapshot), "--work", str(args.work), "--out-dir", str(args.out_dir),
               "--port", "18761", "--observe-seconds", "30"]
        child = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 env={**os.environ, "TEST_UNTRUSTED_ENV": "must not leak"})
        try:
            deadline = time.monotonic() + 5
            pidfile = args.work / "fixture.pid"
            while not pidfile.exists() and time.monotonic() < deadline:
                time.sleep(.03)
            self.assertTrue(pidfile.exists())
            child.send_signal(signal.SIGINT)
            child.communicate(timeout=10)
            report = rehearse.read_json(args.out_dir / "report.json")
            self.assertEqual(report["verdict"], "FAIL")
            self.assertTrue(report["starts"][0]["shutdown"]["process_group_gone"])
            with self.assertRaises(ProcessLookupError):
                os.kill(int(pidfile.read_text()), 0)
        finally:
            if child.poll() is None:
                child.kill()
            child.communicate()

    def test_redaction_log_dedup_and_count_drop_warning(self):
        log = self.root / "log"
        private_path = str(Path.home() / "private-data/db")
        log.write_text("2026-01-01T00:00:00Z ERROR bad " + private_path + "\n" +
                       "2026-01-01T00:00:01Z ERROR bad " + private_path + "\nWARN warning\n")
        summary, _ = rehearse.log_summary(log)
        self.assertEqual(summary["levels"]["ERROR"]["count"], 2)
        self.assertEqual(len(summary["levels"]["ERROR"]["examples"]), 1)
        self.assertNotIn(str(Path.home()), json.dumps(rehearse.scrub(summary)))
        self.assertEqual(rehearse.scrub("http://127.0.0.1:18760"), "http://127.0.0.1:18760")
        report = {"checks": [], "starts": [], "before": {"schema": 1, "tables": {"lost": 5}},
                  "after": {"schema": 1, "tables": {"lost": 2}}, "build": {"schema": 1}}
        rehearse.summarize(report, False, None)
        self.assertEqual(next(c for c in report["checks"] if c["name"] == "table_changes")["status"], "WARN")


if __name__ == "__main__":
    unittest.main()
