"""Fast offline checks: python3 -I -m unittest discover -s scripts/reform/bench."""
import importlib.util
import json
import os
import signal
import time
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

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
                          '{"load_path": "http://example.com"}', '{"idle_s": NaN}'):
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
