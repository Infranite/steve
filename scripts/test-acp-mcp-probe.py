"""Offline checks for the isolated diagnostic; no installed harness or model required."""
import argparse
import asyncio
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import runpy
import stat
import subprocess
import sys
import tempfile
import threading
import tomllib
import unittest
import urllib.request

ROOT = Path(__file__).resolve().parent / "diagnostics"
PROBE = runpy.run_path(str(ROOT / "probe-codex-mcp.py"))
OBSERVER = runpy.run_path(str(ROOT / "responses-tool-observer.py"))


class DiagnosticTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)

    def test_state_import_is_private_and_excludes_live_tool_config(self):
        source = self.root / "source.toml"
        original = '''model_provider = "fixture"
notify = ["do-not-copy"]
[mcp_servers.live]
command = "do-not-run"
[hooks]
before = "do-not-run"
[model_providers.fixture]
base_url = "https://example.invalid/v1"
wire_api = "responses"
[model_providers.fixture.auth]
command = "owner-auth-helper"
args = ["owner-only"]
'''
        source.write_text(original)
        state = PROBE["prepare"](self.root / "isolated", source, "fixture-model")
        config = tomllib.loads((state / "config.toml").read_text())
        self.assertEqual(config["sandbox_mode"], "read-only")
        self.assertEqual(config["model_providers"]["fixture"]["auth"]["command"], "owner-auth-helper")
        self.assertNotIn("mcp_servers", config)
        self.assertNotIn("hooks", config)
        self.assertNotIn("notify", config)
        self.assertEqual(stat.S_IMODE(state.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((state / "config.toml").stat().st_mode), 0o600)
        self.assertEqual(source.read_text(), original)
        with self.assertRaises(FileExistsError):
            PROBE["prepare"](self.root / "isolated", source, "fixture-model")

    def test_fixture_returns_service_only_marker_and_rejects_other_tools(self):
        log = self.root / "mcp.jsonl"
        requests = [{"jsonrpc": "2.0", "id": "init", "method": "initialize", "params": {"protocolVersion": "2025-06-18"}},
                    {"jsonrpc": "2.0", "id": "list", "method": "tools/list"},
                    {"jsonrpc": "2.0", "id": "call", "method": "tools/call", "params": {"name": "steve_probe_read", "arguments": {}}},
                    {"jsonrpc": "2.0", "id": "other", "method": "tools/call", "params": {"name": "write"}}]
        process = subprocess.run([sys.executable, str(ROOT / "mcp-probe-fixture.py"), str(log)],
                                 input="\n".join(map(json.dumps, requests)) + "\n", text=True, capture_output=True, timeout=10, check=True)
        replies = [json.loads(line) for line in process.stdout.splitlines()]
        self.assertEqual([r["id"] for r in replies], [r["id"] for r in requests])
        marker = replies[2]["result"]["content"][0]["text"]
        self.assertTrue(marker.startswith("MCP-PROBE-"))
        self.assertNotIn(marker, json.dumps(requests))
        self.assertTrue(replies[1]["result"]["tools"][0]["annotations"]["readOnlyHint"])
        self.assertEqual(replies[3]["error"]["code"], -32601)
        protocol = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(len(protocol), 8)

    def test_tool_roundtrip_requires_matching_response_and_consumption(self):
        call = {"id": 7, "method": "tools/call", "params": {"name": "steve_probe_read"}}
        response = {"id": 7, "result": {"content": [{"type": "text", "text": "random-service-marker"}]}}
        protocol = [{"direction": "receive", "message": call}, {"direction": "send", "message": response}]
        phase = {"completed": True, "reply": "random-service-marker", "tool_updates": [
            {"sessionUpdate": "tool_call", "toolCallId": "probe-call", "rawInput": {"server": "steve", "tool": "steve_probe_read"}},
            {"sessionUpdate": "tool_call_update", "toolCallId": "probe-call", "status": "completed",
             "rawOutput": {"result": {"content": [{"type": "text", "text": "random-service-marker"}]}, "error": None}}]}
        self.assertTrue(PROBE["mcp_outcome"](phase, protocol)["passed"])
        phase["final_reply"] = ""
        self.assertFalse(PROBE["mcp_outcome"](phase, protocol)["model_consumed"])
        del phase["final_reply"]
        response["id"] = 8
        self.assertFalse(PROBE["mcp_outcome"](phase, protocol)["passed"])
        response["id"] = 7
        response["result"]["isError"] = True
        self.assertFalse(PROBE["mcp_outcome"](phase, protocol)["passed"])
        response["result"]["isError"] = False
        phase["reply"] = "I called the tool."
        self.assertFalse(PROBE["mcp_outcome"](phase, protocol)["passed"])
        self.assertFalse(PROBE["mcp_outcome"](phase, [protocol[0]])["service_returned"])

    def test_unrelated_or_failed_acp_completion_does_not_close_mcp_loop(self):
        protocol = [
            {"direction": "receive", "message": {"id": 7, "method": "tools/call", "params": {"name": "steve_probe_read"}}},
            {"direction": "send", "message": {"id": 7, "result": {"content": [{"type": "text", "text": "service-marker"}]}}}]
        start = {"sessionUpdate": "tool_call", "toolCallId": "probe-call", "rawInput": {"server": "steve", "tool": "steve_probe_read"}}
        end = {"sessionUpdate": "tool_call_update", "toolCallId": "probe-call", "status": "completed",
               "rawOutput": {"result": {"content": [{"type": "text", "text": "service-marker"}]}, "error": None}}
        cases = {
            "unrelated completion": [start, {**end, "toolCallId": "other-call"}],
            "different tool": [{**start, "rawInput": {"server": "steve", "tool": "other"}}, end],
            "different server": [{**start, "rawInput": {"server": "other", "tool": "steve_probe_read"}}, end],
            "no call identity": [{"status": "completed"}],
            "completion before call": [end, start],
            "failed ACP result": [start, {**end, "rawOutput": {"error": "dispatch failed"}}],
            "different ACP result": [start, {**end, "rawOutput": {"result": {"content": [{"type": "text", "text": "other-marker"}]}}}],
            "error result with marker": [start, {**end, "rawOutput": {"result": {"isError": True, "content": [{"type": "text", "text": "service-marker"}]}}}],
        }
        for name, updates in cases.items():
            with self.subTest(name=name):
                phase = {"completed": True, "reply": "service-marker", "tool_updates": updates}
                self.assertFalse(PROBE["mcp_outcome"](phase, protocol)["passed"])

    def test_mcp_response_must_follow_call_and_be_unambiguous(self):
        call = {"direction": "receive", "message": {"id": 7, "method": "tools/call", "params": {"name": "steve_probe_read"}}}
        reply = {"direction": "send", "message": {"id": 7, "result": {"content": [{"type": "text", "text": "service-marker"}]}}}
        phase = {"completed": True, "reply": "service-marker", "tool_updates": [
            {"sessionUpdate": "tool_call", "toolCallId": "call", "rawInput": {"server": "steve", "tool": "steve_probe_read"}},
            {"sessionUpdate": "tool_call_update", "toolCallId": "call", "status": "completed",
             "rawOutput": {"result": {"content": [{"type": "text", "text": "service-marker"}]}}}]}
        self.assertTrue(PROBE["mcp_outcome"](phase, [call, reply])["passed"])
        self.assertFalse(PROBE["mcp_outcome"](phase, [reply, call])["service_returned"])
        self.assertFalse(PROBE["mcp_outcome"](phase, [call, reply, reply])["passed"])

    def test_marker_in_text_before_last_tool_is_not_consumption(self):
        probe = PROBE["Probe"](argparse.Namespace(label="obsolete", model="fixture", output=self.root / "evidence", root=self.root / "private"))
        probe.sid = "fixture-session"

        async def request(*args):
            for update in [
                {"sessionUpdate": "agent_message_chunk", "content": {"text": "obsolete-marker"}},
                {"sessionUpdate": "tool_call", "toolCallId": "call"},
                {"sessionUpdate": "tool_call_update", "toolCallId": "call", "status": "completed"}]:
                probe.events.append({"message": {"method": "session/update", "params": {"update": update}}})
            return {"stopReason": "end_turn"}

        probe.request = request
        phase = asyncio.run(probe.prompt("mcp", "generated diagnostic", 1))
        self.assertEqual(phase["reply"], "obsolete-marker")
        self.assertEqual(phase["final_reply"], "")

    def test_timeout_retains_partial_events_and_original_error(self):
        probe = PROBE["Probe"](argparse.Namespace(label="partial", model="fixture", output=self.root / "evidence", root=self.root / "private"))
        probe.sid = "fixture-session"

        async def failing_request(*args):
            probe.events.append({"message": {"method": "session/update", "params": {"update": {"sessionUpdate": "agent_message_chunk", "content": {"text": "partial"}}}}})
            raise TimeoutError("ACP session/prompt did not complete within 1 seconds")

        probe.request = failing_request
        phase = asyncio.run(probe.prompt("mcp", "generated diagnostic", 1))
        self.assertFalse(phase["completed"])
        self.assertEqual(phase["reply"], "partial")
        self.assertEqual(phase["error_type"], "TimeoutError")
        self.assertIn("within 1 seconds", phase["error"])
        self.assertEqual(json.loads((probe.out / "result.json").read_text())["phases"]["mcp"], phase)

    def test_private_state_cannot_be_placed_in_shareable_evidence(self):
        with self.assertRaisesRegex(ValueError, "must be separate"):
            PROBE["Probe"](argparse.Namespace(label="probe", model="fixture", output=self.root, root=self.root / "private"))

    def test_bootstrap_failure_exits_nonzero_and_keeps_error(self):
        output = self.root / "evidence"
        process = subprocess.run([sys.executable, str(ROOT / "probe-codex-mcp.py"), "--root", str(self.root / "private"),
                                  "--output", str(output), "--source-config", str(self.root / "missing.toml"),
                                  "--adapter", str(self.root / "unused.js"), "--label", "missing-config", "--model", "fixture"],
                                 capture_output=True, text=True, timeout=10)
        self.assertEqual(process.returncode, 1)
        result = json.loads((output / "missing-config" / "result.json").read_text())
        self.assertEqual(result["error_type"], "FileNotFoundError")
        self.assertEqual(result["phases"], {})

    def test_observer_preserves_namespace_and_excludes_headers_and_prompts(self):
        seen = {}
        frame = b'data: {"type":"response.output_item.done","item":{"type":"function_call","namespace":"mcp__steve","name":"steve_probe_read","call_id":"call-1","arguments":"{}"}}\n\n'

        class Backend(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_POST(self):
                seen["auth"] = self.headers.get("Authorization")
                seen["body"] = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(frame)))
                self.end_headers()
                self.wfile.write(frame)

        upstream = ThreadingHTTPServer(("127.0.0.1", 0), Backend)
        threading.Thread(target=upstream.serve_forever, daemon=True).start()
        log = self.root / "wire.jsonl"
        observer, base = OBSERVER["start_observer"](f"http://127.0.0.1:{upstream.server_port}/v1", log)
        try:
            namespace = {"type": "namespace", "name": "mcp__steve", "tools": [{"type": "function", "name": "steve_probe_read", "parameters": {"type": "object", "properties": {}}}]}
            payload = {"model": "fixture", "tools": [namespace], "input": [{"type": "message", "content": "private-prompt-fixture"}]}
            request = urllib.request.Request(base + "/responses", data=json.dumps(payload).encode(), headers={"Authorization": "Bearer private-auth-fixture", "Content-Type": "application/json"})
            with urllib.request.urlopen(request, timeout=5) as response:
                self.assertEqual(response.read(), frame)
            self.assertEqual(seen["auth"], "Bearer private-auth-fixture")
            self.assertEqual(seen["body"], payload)
            evidence = log.read_text()
            self.assertNotIn("private-auth-fixture", evidence)
            self.assertNotIn("private-prompt-fixture", evidence)
            rows = [json.loads(line) for line in evidence.splitlines()]
            self.assertEqual(rows[0]["tools"], [namespace])
            self.assertEqual(rows[-1]["item"]["namespace"], "mcp__steve")
        finally:
            observer.shutdown()
            observer.server_close()
            upstream.shutdown()
            upstream.server_close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
