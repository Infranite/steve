"""T0.3 acceptance contracts, including real HTTP with in-memory credentials."""
from contextlib import nullcontext
import importlib.util
import json
import os
from pathlib import Path
import sqlite3
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from types import SimpleNamespace
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location("ab", Path(__file__).with_name("ab.py"))
ab = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ab)
run, telemetry = ab.run, ab.telemetry


class AcceptanceTests(unittest.TestCase):
    def test_workloads_are_opt_in_and_disable_invalidation(self):
        self.assertEqual(run.configuration(None)["invalidate_rps"], 2)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "config.json"
            for mode in ("read", "write"):
                path.write_text(json.dumps({"workload": mode, "auth": "bearer", "cli_runs": 0}))
                config = run.configuration(path)
                self.assertEqual(config["invalidate_rps"], 0)
                self.assertEqual(config["load_path"], "/api/summary" if mode == "read" else "/api/notifications/read")
                self.assertEqual(config["warmup_s"], 5)
                self.assertEqual(config["load_body"], {"at": 1})
            path.write_text('{"workload":"write"}')
            self.assertEqual(run.configuration(path, "read")["load_path"], "/api/summary")
            for override in ({"workload": "mixed"}, {"auth": "other"}, {"warmup_s": 0}, {"load_body": []}):
                path.write_text(json.dumps(override))
                with self.assertRaises(ValueError):
                    run.configuration(path)

    def test_medians_are_of_rounds_and_throughput_is_inverted(self):
        def row(values, seconds):
            return {"samples": {"warm_ms": {}, "reads_ms": values},
                    "load": {"elapsed_s": seconds, "reads": {"latency_ms": {"count": len(values)}},
                             "cpu": {"http_requests": len(values), "user_system_ns": None},
                             "lock": {"probes": 1, "busy_probes": 0},
                             "disk": {"device_bytes": None, "logical_bytes": None}}}
        a = [row([1], 1), row([3], 1), row([100] * 100, 1)]
        b = [row([1], 2), row([3], 2), row([100] * 100, 2)]
        result = ab.bootstrap(a, b, 1, replicates=50)
        self.assertEqual(result["load.p50_ms"]["a"], 3)
        self.assertEqual(result["load.p95_ms"]["a"], 3)
        self.assertEqual(result["load.requests_per_s"]["a"], 1)
        self.assertEqual(result["load.requests_per_s"]["overhead_pct"], 50)
        self.assertEqual(result["load.requests_per_s"]["verdict"], "FAIL")
        for overhead, ci, expected in ((.5, [-1, .9], "PASS"), (.5, [-1, 1], "PASS(median)/INCONCLUSIVE(CI)"),
                                       (1, [-1, .9], "FAIL"), (-3, [-5, -1], "PASS")):
            self.assertEqual(ab.median_verdict(overhead, ci, 1), expected)

    def test_cli_credentials_are_captured_only_in_memory(self):
        process = Mock(returncode=0)
        process.communicate.return_value = (b'{"user":"owner","token":"private-token"}', None)
        processes = Mock()
        processes.start.return_value = process
        env = {"STEVE_HOME": "/isolated", "STEVE_PORT": "18770"}
        token = run.access_token(processes, Path("/test-binary"), env, 10)
        self.assertEqual(token, {"user": "owner", "token": "private-token"})
        args, options = processes.start.call_args
        self.assertEqual(args[0], ["/test-binary", "--access", "token", "create", "--name", "metrics-acceptance", "--days", "1", "--json"])
        self.assertEqual(args[1], env)
        self.assertEqual(options["stdout"], run.subprocess.PIPE)
        self.assertEqual(options["stderr"], run.subprocess.DEVNULL)

    def test_real_http_owner_exchange_workspace_write_warmup_and_scrape(self):
        observed = []
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def handle_request(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                observed.append((self.command, self.path, dict(self.headers), body))
                code, ctype, payload = 200, "application/json", b'{"seen":1}'
                if self.path == "/auth/token":
                    payload = b'{"credential":"private-credential"}'
                elif self.path == "/api/me":
                    payload = b'{"workspace":"workspace-a"}'
                elif self.path == "/metrics":
                    code = 200 if self.headers.get("Authorization") == "Bearer private-credential" else 401
                    ctype, payload = "text/plain; version=0.0.4", b'steve_process_disk_written_bytes 10\n'
                self.send_response(code)
                self.send_header("Content-Type", ctype)
                self.end_headers()
                self.wfile.write(payload)
            do_GET = do_POST = handle_request
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            client = run.Client(f"http://127.0.0.1:{server.server_port}", 2)
            client.login({"user": "owner", "token": "private-token"})
            with tempfile.TemporaryDirectory() as tmp:
                db = Path(tmp) / "db"
                sqlite3.connect(db).close()
                config = run.configure_workload(dict(run.DEFAULT), "write")
                config.update(warmup_s=.08, load_s=.08, process_interval_s=.02, cli_runs=0, clients=2)
                hub = Mock(pid=os.getpid())
                hub.poll.return_value = None
                with patch.object(run, "process_table", return_value=[]):
                    warm = run.warmup(hub, db, client, config, {}, Mock(), Path("/unused"))
                    samples = {}
                    result = run.phase("load", .08, hub, db, client, config, {}, Mock(), Path("/unused"), samples)
                self.assertGreater(warm["requests"], 0)
                self.assertGreater(result["writes"]["latency_ms"]["count"], 0)
                self.assertEqual(result["reads"]["latency_ms"]["count"], 0)
                self.assertEqual(result["errors"], 0)
                self.assertEqual(len(samples["workload_ms"]), result["requests"])
                target = Path(tmp) / "before.prom"
                auth = telemetry.scrape(client, client.base + "/metrics", target)
                anon = telemetry.scrape(client, client.base + "/metrics", None, False)
                self.assertEqual((auth["status"], anon["status"]), (200, 401))
                self.assertNotIn("private-", target.read_text() + json.dumps(result))
            writes = [r for r in observed if r[1] == "/api/notifications/read"]
            self.assertGreater(len(writes), 2)
            for method, path, headers, body in writes:
                self.assertEqual(method, "POST")
                self.assertEqual(json.loads(body), {"at": 1})
                self.assertEqual(headers["Content-Type"], "application/json")
                self.assertEqual(headers["Authorization"], "Bearer private-credential")
                self.assertEqual(headers["X-Steve-Ws"], "workspace-a")
            self.assertEqual(json.loads(observed[0][3])["client"]["name"], "metrics-acceptance")
        finally:
            server.shutdown()
            thread.join(timeout=2)
            server.server_close()

    def test_write_smoke_failure_prevents_warmup_and_timing(self):
        client = Mock()
        client.json_request.side_effect = [{"seen": 1}, {"seen": 2}]
        config = run.configure_workload(dict(run.DEFAULT), "write")
        with patch.object(run, "phase") as phase, self.assertRaises(RuntimeError):
            run.warmup(Mock(), Path("/unused"), client, config, {}, Mock(), Path("/unused"))
        phase.assert_not_called()

    def test_numeric_deltas_filter_route_and_sort_lock_callers(self):
        labels = '{route="/api/summary",method="GET",status="2xx"}'
        before = f'steve_http_requests_total{labels} 10\nsteve_process_disk_written_bytes 100\n'
        after = f'steve_http_requests_total{labels} 25\nsteve_process_disk_written_bytes 300\n'
        for suffix, value in (("sum", .2), ("count", 15)):
            after += f'steve_http_request_duration_seconds_{suffix}{labels} {value}\n'
            after += f'steve_http_store_calls_per_request_{suffix}{{route="/api/summary"}} {value}\n'
        after += 'steve_http_requests_total{route="/other",method="GET",status="2xx"} 999\n'
        after += 'steve_metrics_collection_errors_total{source="io"} 2\n'
        for lock in ("store", "core"):
            for i in range(12):
                for kind in ("wait", "hold"):
                    after += f'steve_lock_{kind}_seconds_sum{{lock="{lock}",caller="file:{i}"}} {i}\n'
                    after += f'steve_lock_{kind}_seconds_count{{lock="{lock}",caller="file:{i}"}} 2\n'
        delta = telemetry.difference(before, after, "/api/summary", "GET")
        self.assertEqual(delta["http"]["steve_http_requests_total"], 15)
        self.assertEqual(delta["disk_written_bytes"]["delta"], 200)
        self.assertEqual(delta["collection_errors"], [{"source": "io", "delta": 2}])
        self.assertEqual(delta["missing"], [])
        for kind in ("wait", "hold"):
            for lock in ("store", "core"):
                rows = delta["lock_top10"][kind][lock]
                self.assertEqual(len(rows), 10)
                self.assertEqual(rows[0]["caller"], "file:11")
                self.assertEqual(rows[0]["mean_ms"], 5500)
        self.assertIn("steve_http_requests_total", telemetry.difference("", "", "/api/summary", "GET")["missing"])

    def test_adapter_environment_is_shared_and_complete(self):
        self.assertEqual(len(run.isolation.ADAPTERS), 17)
        with tempfile.TemporaryDirectory() as tmp:
            env = run.isolated_environment(Path(tmp), 18770)
            self.assertEqual(env["STEVE_UPDATE_CHECK"], "0")
            self.assertTrue(Path(env["STEVE_WEB_DIR"]).is_dir())
            for name in run.isolation.ADAPTERS:
                self.assertEqual(env[f"STEVE_ADAPTER_{name}"], "/usr/bin/false")
                with self.assertRaises(ValueError):
                    ab.environment_options([f"STEVE_ADAPTER_{name}=/bin/true"], "STEVE_METRICS")


if __name__ == "__main__":
    unittest.main()
