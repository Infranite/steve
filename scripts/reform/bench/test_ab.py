"""Offline A/B contract checks; no Hub or production home is opened."""
import ctypes
import errno
import importlib.util
import json
from pathlib import Path
import sqlite3
from types import SimpleNamespace
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
import urllib.error

spec = importlib.util.spec_from_file_location("ab", Path(__file__).with_name("ab.py"))
ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ab)


def row(latencies, cpu, duration=2, busy=1, probes=10, disk=100):
    return {"samples": {"warm_ms": {"/api/state": latencies}, "reads_ms": latencies},
            "load": {"elapsed_s": duration, "reads": {"latency_ms": {"count": len(latencies)}},
                     "cpu": {"http_requests": len(latencies), "user_system_ns": cpu},
                     "lock": {"probes": probes, "busy_probes": busy},
                     "disk": {"device_bytes": disk, "logical_bytes": disk * 2}}}


class StatisticsTests(unittest.TestCase):
    def test_abba_order_and_equal_counts(self):
        self.assertEqual(''.join(ab.order(3)), "ABBAAB")
        self.assertEqual(''.join(ab.order(4)), "ABBAABBA")
        for n in range(3, 11):
            self.assertEqual(ab.order(n).count("A"), n)
            self.assertEqual(ab.order(n).count("B"), n)

    def test_percentiles_interpolate_without_mutation(self):
        samples = [100, 0, 20, 10]
        q = ab.percentiles(samples)
        self.assertEqual(q["p50_ms"], 15)
        self.assertAlmostEqual(q["p95_ms"], 88)
        self.assertAlmostEqual(q["p99_ms"], 97.6)
        self.assertEqual(samples, [100, 0, 20, 10])
        self.assertEqual(ab.percentiles([])["p95_ms"], None)
        self.assertEqual(ab.percentiles([7])["p99_ms"], 7)

    def test_pooled_samples_and_weighted_totals(self):
        a = row([1], 1000, duration=1, busy=1, probes=1)
        b = row([3, 5, 7], 3000, duration=3, busy=0, probes=9)
        pooled = ab.aggregate([a, b])
        self.assertEqual(pooled["load.read.p50_ms"], 4)
        self.assertEqual(pooled["load.read_requests_per_s"], 1)
        self.assertEqual(pooled["load.cpu_us_per_request"], 1)
        self.assertEqual(pooled["load.lock_busy_ratio"], .1)
        self.assertEqual(pooled["load.disk_device_bytes"], 200)

    def test_bootstrap_determinism_known_shift_and_noise(self):
        a = [row([1, 2, 3], 3000)] * 3
        b = [row([1.02, 2.04, 3.06], 3060)] * 3
        first = ab.bootstrap(a, b, 1, replicates=100)
        self.assertEqual(first, ab.bootstrap(a, b, 1, replicates=100))
        for key in ab.PRIMARY:
            self.assertAlmostEqual(first[key]["overhead_pct"], 2)
            self.assertAlmostEqual(first[key]["ci95_pct"][0], 2)
            self.assertEqual(first[key]["verdict"], "FAIL")
        noisy = [row([v, v * 2], v * 3000) for v in (1, 2, 3)]
        result = ab.bootstrap(noisy, noisy, 1, replicates=200)
        ci = result[ab.PRIMARY[0]]["ci95_pct"]
        self.assertLess(ci[0], 0)
        self.assertGreater(ci[1], 1)
        self.assertEqual(result[ab.PRIMARY[0]]["verdict"], "INCONCLUSIVE")

    def test_verdict_strict_boundaries_and_unavailable(self):
        for interval, expected in (([-2, .99], "PASS"), ([1.01, 2], "FAIL"),
                                   ([-2, 1], "INCONCLUSIVE"), ([1, 2], "INCONCLUSIVE"),
                                   (None, "INCONCLUSIVE")):
            self.assertEqual(ab.verdict(interval, 1), expected)
        self.assertIsNone(ab.relative(0, 1))
        self.assertIsNone(ab.relative(0, 0))
        a = [row([1], None)] * 3
        result = ab.bootstrap(a, a, 1, replicates=5)
        self.assertIsNone(result["load.cpu_us_per_request"]["ci95_pct"])


class IsolationTests(unittest.TestCase):
    def test_shared_environment_protects_all_isolation_settings(self):
        extra, ports = ab.environment_options(["STEVE_METRICS_LISTEN=127.0.0.1:18774"], "STEVE_METRICS")
        self.assertEqual(extra, {"STEVE_METRICS_LISTEN": "127.0.0.1:18774"})
        self.assertEqual(ports, [("127.0.0.1", 18774)])
        for item in ("STEVE_HOME=/tmp/unsafe", "STEVE_PORT=8440", "STEVE_METRICS_LISTEN=127.0.0.1:8440",
                     "STEVE_METRICS_LISTEN=0.0.0.0:18774", "STEVE_METRICS_LISTEN=unix:" + str(Path.home() / ".steve/socket"),
                     "STEVE_METRICS=off", "PATH=/bad", "X-KEY=1", "invalid"):
            with self.subTest(item=item), self.assertRaises(ValueError):
                ab.environment_options([item], "STEVE_METRICS")
        with self.assertRaises(ValueError):
            ab.environment_options([], "STEVE_HOME")

    def test_probes_allow_only_loopback_no_auth_no_redirect(self):
        self.assertEqual(ab.probe_url("/healthz", 18770), "http://127.0.0.1:18770/healthz")
        for url in ("http://127.0.0.1:18774/metrics", "http://[::1]:18774/metrics", "http://localhost:18774/metrics"):
            self.assertEqual(ab.probe_url(url, 18770), url)
        for url in ("http://example.org:18774/metrics", "http://127.0.0.1:8440/healthz", "https://127.0.0.1:18774/",
                    "//external/", "http://user:secret@127.0.0.1:18774/", "http://127.0.0.1:18774/#fragment"):
            with self.subTest(url=url), self.assertRaises(ValueError):
                ab.probe_url(url, 18770)
        opener = Mock()
        with patch.object(ab.urllib.request, "build_opener", return_value=opener):
            opener.open.side_effect = urllib.error.URLError(ConnectionRefusedError(errno.ECONNREFUSED, "refused"))
            self.assertEqual(ab.probe("http://127.0.0.1:18774/metrics", 1), "refused")
            exc = urllib.error.HTTPError("http://127.0.0.1:18774/metrics", 401, "unauthorized", {}, None)
            opener.open.side_effect = exc
            self.assertEqual(ab.probe("http://127.0.0.1:18774/metrics", 1), 401)
            self.assertEqual(opener.open.call_args.args[0], "http://127.0.0.1:18774/metrics")

    def test_clone_fallback_and_source_unchanged(self):
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "source"
            source.mkdir()
            (source / "db").write_bytes(b"original")
            target = Path(tmp) / "target"
            with patch.object(ab.sys, "platform", "other"):
                self.assertEqual(ab.clone(source, target), "copy")
            (target / "db").write_bytes(b"new")
            self.assertEqual((source / "db").read_bytes(), b"original")
            with self.assertRaises(FileExistsError):
                ab.clone(source, target)

    def test_clone_cleans_only_its_partial_destination_before_fallback(self):
        with tempfile.TemporaryDirectory() as tmp:
            source, target = Path(tmp) / "source", Path(tmp) / "target"
            source.write_bytes(b"original")
            def failed_cp(*args, **kwargs):
                target.write_bytes(b"partial")
                return SimpleNamespace(returncode=1)
            with patch.object(ab.sys, "platform", "darwin"), patch.object(ab.subprocess, "run", side_effect=failed_cp):
                self.assertEqual(ab.clone(source, target), "copy")
            self.assertEqual(source.read_bytes(), b"original")
            self.assertEqual(target.read_bytes(), b"original")

    def test_cpu_counts_live_and_reaped_descendants_once(self):
        meter = ab.CpuMeter(100)
        values = {100: (1000, 100, 200, 20), 101: (500, 50, 0, 0)}
        def rusage(pid, flavor, pointer):
            usage = ctypes.cast(pointer, ctypes.POINTER(ab.run.RUsage)).contents
            usage.user, usage.system, usage.c_user, usage.c_system = values[pid]
            return 0
        meter.lib = Mock()
        meter.lib.proc_pid_rusage.side_effect = rusage
        meter.sample([(100, 1, 0, "hub"), (101, 100, 0, "child")], baseline=True)
        # Child was reaped: its cumulative time is now in the Hub's c_*.
        values[100] = (2000, 200, 800, 80)
        meter.sample([(100, 1, 0, "hub")])
        result = meter.result(2)
        self.assertEqual(result["user_system_ns"], 1210)
        self.assertAlmostEqual(result["us_per_request"], .605)

    def test_help_and_cli_validation_without_hub(self):
        help_text = subprocess.check_output([sys.executable, "-I", str(Path(ab.__file__)), "--help"], text=True)
        for word in ("--env", "--a-unset", "refused", "bootstrap", "golden"):
            self.assertIn(word, help_text.lower())

    def test_a_unset_and_shared_env_reach_launcher_with_cleanup_on_failure(self):
        args = SimpleNamespace(port=18770, var="STEVE_METRICS", a="off", b="prometheus",
                               a_unset=True, hub_bin=Path("/unused-hub"))
        for variant in ("A", "B"):
            with tempfile.TemporaryDirectory() as tmp:
                processes = Mock()
                processes.cleanup.return_value = {"process_groups_remaining": []}
                with patch.object(ab.run, "Processes", return_value=processes), \
                     patch.object(ab.run, "process_table", return_value=[]), \
                     patch.object(ab.run, "load_average", return_value=[1, 2, 3]), \
                     patch.object(ab.run, "health", side_effect=RuntimeError("test startup failure")):
                    result = {}
                    with self.assertRaises(RuntimeError):
                        ab.collect(Path(tmp), args, ab.run.DEFAULT, variant,
                                   {"STEVE_METRICS_LISTEN": "127.0.0.1:18774"}, result, golden=True)
                env = processes.start.call_args.args[1]
                if variant == "A":
                    self.assertNotIn("STEVE_METRICS", env)
                else:
                    self.assertEqual(env["STEVE_METRICS"], "prometheus")
                self.assertEqual(env["STEVE_METRICS_LISTEN"], "127.0.0.1:18774")
                self.assertEqual(env["STEVE_HOME"], tmp)
                self.assertEqual(result["status"], "failed")
                processes.cleanup.assert_called_once()

    def test_equal_probe_warning_and_partial_run_cannot_pass(self):
        args = SimpleNamespace(rounds=3, probe=["/healthz"], threshold_pct=1)
        rounds = []
        for index, variant in enumerate(ab.order(3), 1):
            value = row([1, 2, 3], 3000)
            value.update(index=index, variant=variant, status="ok", probes={"/healthz": 200})
            rounds.append(value)
        result = {"rounds": rounds, "warnings": [], "status": "ok"}
        ab.summarize(result, args)
        self.assertEqual(result["overall"], "PASS")
        self.assertIn("PROBE IDENTICAL", result["warnings"][0])
        rounds[-1]["status"] = "measurement_errors"
        ab.summarize(result, args)
        self.assertEqual(result["overall"], "INCONCLUSIVE")
        rounds.pop()
        ab.summarize(result, args)
        self.assertEqual(result["overall"], "INCONCLUSIVE")

    def test_workflow_clones_golden_and_preserves_source_without_starting_hub(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp).resolve()
            snapshot, output = root / "snapshot.db", root / "result.json"
            conn = sqlite3.connect(snapshot)
            conn.execute("CREATE TABLE users(id,subject,role,created)")
            conn.execute("INSERT INTO users VALUES(1,'original-owner','owner',0)")
            conn.commit()
            conn.close()
            def fake_collect(home, args, config, variant, extra, result, golden=False):
                db = sqlite3.connect(home / "data/steve.sqlite3")
                try:
                    self.assertEqual(db.execute("SELECT subject FROM users").fetchone()[0], "dev:local")
                    self.assertIsNone(db.execute("SELECT name FROM sqlite_master WHERE name='round_mutation'").fetchone())
                    if not golden:
                        db.execute("CREATE TABLE round_mutation(value)")
                        db.commit()
                        result.update(row([1, 2, 3], 3000))
                    result.update(status="ok", elapsed_s=1)
                finally:
                    db.close()
            argv = ["--hub-bin", sys.executable, "--snapshot", str(snapshot), "--work", str(root / "work"),
                    "--out", str(output), "--md", str(root / "report.md"), "--port", "18770",
                    "--var", "TEST_NOOP", "--a", "0", "--b", "1"]
            with patch.object(ab.socket, "socket"), patch.object(ab.run, "host_info", return_value={}), \
                 patch.object(ab, "collect", side_effect=fake_collect), patch("builtins.print"):
                self.assertEqual(ab.main(argv), 0)
            result = json.loads(output.read_text())
            self.assertEqual([r["variant"] for r in result["rounds"]], list("ABBAAB"))
            self.assertEqual(result["overall"], "PASS")
            self.assertTrue((root / "report.md").read_text().startswith("PASS:"))
            conn = sqlite3.connect(f"file:{snapshot}?mode=ro", uri=True)
            try:
                self.assertEqual(conn.execute("SELECT subject FROM users").fetchone()[0], "original-owner")
            finally:
                conn.close()
            with patch.object(ab.socket, "socket"), patch.object(ab.run, "host_info", return_value={}), \
                 patch.object(ab, "collect", side_effect=RuntimeError("test failure")), patch("builtins.print"):
                self.assertEqual(ab.main(argv), 1)
            self.assertEqual(json.loads(output.read_text())["status"], "failed")


if __name__ == "__main__":
    unittest.main()
