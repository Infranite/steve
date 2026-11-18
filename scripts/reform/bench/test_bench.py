"""Fast offline checks: python3 -I -m unittest discover -s scripts/reform/bench."""
import importlib.util
from contextlib import nullcontext
import gzip
import io
import json
import os
import signal
import time
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock
from types import SimpleNamespace
import urllib.error

HERE = Path(__file__).resolve().parent


def module(name):
    spec = importlib.util.spec_from_file_location(name, HERE / (name + ".py"))
    loaded = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(loaded)
    return loaded


run, compare = module("run"), module("compare")


class PercentileTests(unittest.TestCase):
    def test_empty_singleton_unsorted_interpolation(self):
        self.assertEqual(run.distribution([]), {"count": 0, "p50": None, "p90": None, "p99": None, "max": None})
        self.assertEqual(run.distribution([7])["p99"], 7)
        values = [100, 0, 20, 10]
        result = run.distribution(values)
        self.assertEqual(result["p50"], 15)
        self.assertAlmostEqual(result["p90"], 76)
        self.assertAlmostEqual(result["p99"], 97.6)
        self.assertEqual(result["max"], 100)
        self.assertEqual(values, [100, 0, 20, 10])

    def test_default_file_matches_builtin_and_partial_override(self):
        self.assertEqual(json.loads((HERE / "default.json").read_text()), run.DEFAULT)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.json"
            path.write_text('{"clients": 2}')
            self.assertEqual(run.configuration(path)["clients"], 2)
            self.assertEqual(run.configuration(path)["endpoints"], run.DEFAULT["endpoints"])
            for value in ('{"unknown": 1}', '{"clients": 0}', '{"load_s": -1}',
                          '{"load_path": "http://example.com"}', '{"idle_s": NaN}', '{"settle_s": 0}'):
                path.write_text(value)
                with self.assertRaises(ValueError):
                    run.configuration(path)

    def test_home_guard_resolves_symlinks(self):
        with self.assertRaises(ValueError):
            run.safe_work(Path.home() / ".steve-dev" / "bench")
        with tempfile.TemporaryDirectory() as directory:
            alias = Path(directory) / "alias"
            alias.symlink_to(Path.home() / ".steve", target_is_directory=True)
            with self.assertRaises(ValueError):
                run.safe_work(alias / "bench")


class DocumentationAndEncodingTests(unittest.TestCase):
    def test_help_contains_operator_documentation(self):
        help_text = subprocess.check_output([sys.executable, "-I", str(HERE / "run.py"), "--help"], text=True)
        for section in ("隔离与生命周期\n", "测量方法\n", "JSON 字段\n", "settle_s", "content_encodings"):
            self.assertIn(section, help_text)

    def test_request_and_summary_preserve_wire_encodings(self):
        compressed = gzip.compress(b"x" * 2000)
        client = run.Client("http://127.0.0.1:1", 1)
        client.opener = Mock()
        client.opener.open.side_effect = [
            nullcontext(SimpleNamespace(status=200, headers={"Content-Encoding": "gzip"}, read=lambda: compressed)),
            nullcontext(SimpleNamespace(status=200, headers={}, read=lambda: b"plain")),
            urllib.error.HTTPError("http://127.0.0.1:1/api/test", 503, "unavailable",
                                   {"Content-Encoding": "gzip"}, io.BytesIO(compressed)),
            urllib.error.URLError("offline"),
        ]
        rows = [client.request("/api/test") for _ in range(4)]
        self.assertEqual([r["content_encoding"] for r in rows], ["gzip", "identity", "gzip", None])
        self.assertEqual(rows[0]["bytes"], len(compressed))
        self.assertEqual(rows[2]["status"], 503)
        self.assertEqual(client.opener.open.call_args_list[0].args[0].get_header("Accept-encoding"), "gzip")
        summary = run.requests_summary(rows)
        self.assertEqual(summary["accept_encodings"], {"gzip": 4})
        self.assertEqual(summary["content_encodings"], {"gzip": 2, "identity": 1, "unavailable": 1})
        self.assertEqual(summary["errors"], 2)


class LifecycleTests(unittest.TestCase):
    def test_detached_descendant_disk_and_cleanup_after_parent_exit(self):
        # Exercise setsid() without starting a Hub or touching any real database.
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory).resolve()
            (home / "data/node").mkdir(parents=True)
            (home / "tmp").mkdir()
            env = run.isolated_environment(home, 18770)
            processes = run.Processes()
            child_code = ("import os,time;from pathlib import Path;"
                          "Path(os.environ['STEVE_HOME'],'data/node/service.pid').write_text(str(os.getpid()));"
                          "time.sleep(30)")
            parent_code = ("import subprocess,sys,time;"
                           f"subprocess.Popen([sys.executable,'-c',{child_code!r},'--local-node'],"
                           "start_new_session=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);"
                           "time.sleep(1)")
            child_pid = None
            try:
                parent = processes.start([sys.executable, "-c", parent_code], env,
                                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                pidfile = home / "data/node/service.pid"
                deadline = time.perf_counter() + 5
                while not pidfile.exists() and time.perf_counter() < deadline:
                    time.sleep(.01)
                self.assertTrue(pidfile.exists())
                child_pid = int(pidfile.read_text())
                self.assertEqual(os.getpgid(child_pid), child_pid)
                if sys.platform == "darwin":
                    meter = run.DiskMeter(parent.pid)
                    meter.sample(run.process_table(), baseline=True)
                    self.assertIn(child_pid, [r["pid"] for r in meter.result(1)["processes"]])
                parent.wait(timeout=5)
                self.assertEqual(processes.cleanup()["process_groups_remaining"], [])
                self.assertEqual(run.live_groups({child_pid}), set())
            finally:
                processes.cleanup()
                if child_pid:
                    try:
                        os.killpg(child_pid, signal.SIGKILL)
                    except (ProcessLookupError, PermissionError):
                        pass


class CompareTests(unittest.TestCase):
    def test_load_context_and_post_start_measurements(self):
        a = {"load_average_before": [1, 2, 3], "load_average_after": [4, 5, 6],
             "cpu_top": [{"name": "fourth", "cpu_percent": 5}, {"name": "third", "cpu_percent": 10},
                         {"name": "first|worker", "cpu_percent": 50}, {"name": "second", "cpu_percent": 30}],
             "post_start": {"disk": {"device_bytes": 10}}}
        b = {"load_average_before": [7, 8, 9], "load_average_after": [10, 11, 12]}
        table = compare.compare(a, b)
        preamble = table.split("| Metric |")[0]
        self.assertIn("A load_average_before: [1, 2, 3]; load_average_after: [4, 5, 6]", preamble)
        self.assertIn("B load_average_before: [7, 8, 9]; load_average_after: [10, 11, 12]", preamble)
        self.assertIn("first\\|worker 50%, second 30%, third 10%", preamble)
        self.assertNotIn("fourth", preamble)
        self.assertIn("负载不同时，性能数字不可直接比较", preamble)
        self.assertIn("| post_start.disk.device_bytes | 10 | — | n/a |", table)

    def test_percent_missing_and_zero_baselines(self):
        self.assertEqual(compare.percent(20, 10), "-50.00%")
        self.assertEqual(compare.percent(10, 20), "+100.00%")
        self.assertEqual(compare.percent(0, 0), "0.00%")
        self.assertEqual(compare.percent(0, 1), "n/a (A=0)")
        self.assertEqual(compare.percent(None, 1), "n/a")

    def test_union_escape_and_exclusions(self):
        a = {"label": "a|b", "status": "ok", "config": {"clients": 4},
             "load": {"disk": {"processes": [{"pid": 1, "name": "hub", "device_bytes": 20}]}, "errors": 0}}
        b = {"status": "failed", "load": {"new_metric": 5, "errors": 0}}
        table = compare.compare(a, b)
        self.assertIn("a\\|b", table)
        self.assertIn("| load.errors | 0 | 0 | 0.00% |", table)
        self.assertIn("| load.new_metric | — | 5 | n/a |", table)
        self.assertNotIn("config.clients", table)
        self.assertNotIn(".pid", table)
        self.assertIn("processes[hub#1].device_bytes", table)

    def test_cli_self_comparison(self):
        sample = {"status": "ok", "startup_to_health_ms": 10, "load": {"errors": 0}}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "sample.json"
            path.write_text(json.dumps(sample))
            output = subprocess.check_output([sys.executable, "-I", str(HERE / "compare.py"),
                                              str(path), str(path), "--md"], text=True)
        self.assertIn("| startup_to_health_ms | 10 | 10 | +0.00% |", output)
        self.assertIn("| Metric | A | B |", output)


if __name__ == "__main__":
    unittest.main()
