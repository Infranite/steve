"""Offline checks for the isolated diagnostic; no installed harness or model required."""
import argparse
import asyncio
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import runpy
import signal
import stat
import subprocess
import sys
import tempfile
import threading
import time
import tomllib
import unittest
from unittest import mock
import urllib.request

ROOT = Path(__file__).resolve().parent / "diagnostics"
PROBE = runpy.run_path(str(ROOT / "probe-codex-mcp.py"))
OBSERVER = runpy.run_path(str(ROOT / "responses-tool-observer.py"))
PROCESSES = runpy.run_path(str(ROOT / "owned-processes.py"))
PROBE_SCRIPT = ROOT / "probe-codex-mcp.py"


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


class CleanupTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.owned = []
        self.addCleanup(self.stop_new_processes)

    def remember(self, info):
        self.assertIsNotNone(info)
        self.owned.append(info)
        return info

    def stop_new_processes(self):
        # Test fallback also refuses a recycled or unverifiable PID. Never use
        # a PID from a previous run, a name, or a process group.
        signals = []
        for expected in reversed(self.owned):
            info = PROCESSES["process_info"](expected["pid"])
            if info and info["state"] == "alive":
                self.assertEqual(info["birth"], expected["birth"])
                os.kill(info["pid"], signal.SIGKILL)
                signals.append({"pid": info["pid"], "signal": "SIGKILL", "verified": info})
        readbacks = []
        for expected in self.owned:
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                info = PROCESSES["process_info"](expected["pid"])
                if not info or info["state"] == "zombie":
                    break
                self.assertEqual(info["birth"], expected["birth"])
                time.sleep(.02)
            else:
                self.fail("test-owned process remained alive after fallback")
            readbacks.append({"expected": expected, "final": info,
                              "passed": info is None or info["state"] == "zombie"})
        self.fallbacks = getattr(self, "fallbacks", []) + [{"signals": signals, "readbacks": readbacks}]
        self.save("fallback-" + self._testMethodName, self.fallbacks)

    def save(self, name, value):
        if directory := os.environ.get("STEVE_DIAGNOSTIC_EVIDENCE"):
            destination = Path(directory)
            destination.mkdir(parents=True, exist_ok=True)
            (destination / (name + ".json")).write_text(json.dumps(value, indent=2) + "\n")

    async def family(self, grandchild=False):
        child_code = "import signal,time; signal.signal(signal.SIGTERM,signal.SIG_IGN); print('ready',flush=True); time.sleep(300)"
        if grandchild:
            child_code = "\n".join(["import signal,time,subprocess,sys", "from pathlib import Path",
                "signal.signal(signal.SIGTERM,signal.SIG_IGN)",
                f"child=subprocess.Popen([sys.executable,'-u','-c',{child_code!r}],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE)",
                "child.stdout.readline()", f"Path({str(self.root / 'grandchild.pid')!r}).write_text(str(child.pid))",
                "print('ready',flush=True)", "time.sleep(300)"])
        parent_code = "\n".join([
            "import subprocess,sys,signal",
            f"child=subprocess.Popen([sys.executable,'-u','-c',{child_code!r}],stdin=subprocess.DEVNULL,stdout=subprocess.PIPE)",
            "child.stdout.readline()",
            "def stop(*args):\n child.wait(timeout=5)\n sys.exit(0)",
            "signal.signal(signal.SIGTERM,stop)",
            "print(child.pid,flush=True)",
            "sys.stdin.readline()"])
        parent = await asyncio.create_subprocess_exec(sys.executable, "-u", "-c", parent_code,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE)
        self.remember(PROCESSES["process_info"](parent.pid))
        manager = PROCESSES["OwnedProcesses"](parent, grace=.15, kill_timeout=1)
        child_pid = int(await asyncio.wait_for(parent.stdout.readline(), 5))
        child = self.remember(PROCESSES["process_info"](child_pid))
        self.assertEqual(child["ppid"], parent.pid)
        await manager.observe()
        self.assertIn(child_pid, manager.records)
        return parent, child, manager

    def test_native_identity_is_stable_and_excludes_command_lines(self):
        first = PROCESSES["process_info"](os.getpid())
        self.assertEqual(first, PROCESSES["process_info"](os.getpid()))
        self.assertEqual(set(first), {"pid", "ppid", "birth", "state"})
        self.assertEqual(first["ppid"], os.getppid())

    def test_sigterm_ignoring_descendant_is_killed_and_unrelated_process_is_untouched(self):
        async def check():
            unrelated = await asyncio.create_subprocess_exec(sys.executable, "-c", "import time;time.sleep(300)")
            other = self.remember(PROCESSES["process_info"](unrelated.pid))
            parent, child, manager = await self.family()
            receipt = await manager.cleanup()
            self.save("ignores-sigterm-and-unrelated", {"cleanup": receipt, "unrelated_before": other,
                "unrelated_after": PROCESSES["process_info"](unrelated.pid)})
            self.assertTrue(receipt["passed"], receipt)
            self.assertNotIn(unrelated.pid, manager.records)
            self.assertEqual(PROCESSES["process_info"](unrelated.pid)["birth"], other["birth"])
            self.assertEqual(PROCESSES["process_info"](unrelated.pid)["state"], "alive")
            events = manager.records[child["pid"]]["events"]
            self.assertTrue(any(e.get("signal") == "SIGTERM" and e["sent"] for e in events))
            self.assertTrue(any(e.get("stage") == "after_sigterm" and e["readback"]["state"] == "alive" for e in events))
            self.assertTrue(any(e.get("signal") == "SIGKILL" and e["sent"] for e in events))
            self.assertIn(parent.returncode, (0, -signal.SIGKILL))
            # Reap this direct child after the identity-checked test cleanup.
            self.stop_new_processes()
            await unrelated.wait()
        asyncio.run(check())

    def test_tracked_descendant_is_cleaned_after_parent_exits_zero(self):
        async def check():
            parent, child, manager = await self.family()
            parent.stdin.write(b"exit\n")
            await parent.stdin.drain()
            await asyncio.wait_for(parent.wait(), 3)
            self.assertEqual(parent.returncode, 0)
            reparented = PROCESSES["process_info"](child["pid"])
            self.assertNotEqual(reparented["ppid"], parent.pid)
            receipt = await manager.cleanup()
            self.save("parent-exited-zero", {"reparented": reparented, "cleanup": receipt})
            self.assertTrue(receipt["passed"], receipt)
        asyncio.run(check())

    def test_grandchild_is_tracked_and_reaped(self):
        async def check():
            parent, child, manager = await self.family(grandchild=True)
            grandchild = self.remember(PROCESSES["process_info"](int((self.root / "grandchild.pid").read_text())))
            self.assertEqual(grandchild["ppid"], child["pid"])
            self.assertEqual(set(manager.records), {parent.pid, child["pid"], grandchild["pid"]})
            receipt = await manager.cleanup()
            self.save("grandchild", receipt)
            self.assertTrue(receipt["passed"], receipt)
            self.assertEqual(manager.records[grandchild["pid"]]["owner_pid"], child["pid"])
        asyncio.run(check())

    def test_discovery_failure_is_reported_even_if_known_processes_exit(self):
        async def check():
            parent, child, manager = await self.family()
            with mock.patch.dict(manager.observe.__globals__, {"process_parents": mock.Mock(side_effect=PermissionError("synthetic ps denial"))}):
                receipt = await manager.cleanup()
            self.save("discovery-failed", receipt)
            self.assertFalse(receipt["passed"])
            self.assertTrue(all(r["passed"] for r in receipt["processes"]))
            self.assertTrue(any(e["stage"] == "discovery" for e in receipt["errors"]))
        asyncio.run(check())

    def test_residual_or_unknown_identity_fails_without_signalling_other_pids(self):
        for scenario in ("residual", "identity_unreadable", "identity_changed"):
            with self.subTest(scenario=scenario):
                async def check():
                    parent, child, manager = await self.family()
                    parent.stdin.write(b"exit\n")
                    await parent.stdin.drain()
                    await asyncio.wait_for(parent.wait(), 3)
                    manager.kill_timeout = .1
                    original = PROCESSES["process_info"]

                    def inspect(pid):
                        info = original(pid)
                        if pid == child["pid"]:
                            if scenario == "identity_unreadable":
                                raise PermissionError("synthetic identity denial")
                            if scenario == "identity_changed":
                                return {**info, "birth": "synthetic different birth"}
                        return info

                    with mock.patch.dict(manager.inspect.__globals__, {"process_info": inspect}), \
                            mock.patch.object(os, "kill", side_effect=PermissionError("synthetic signal denial")) as kill, \
                            mock.patch.object(signal, "pidfd_send_signal", side_effect=PermissionError("synthetic signal denial"), create=True) as pidfd:
                        receipt = await manager.cleanup()
                    self.save(scenario, receipt)
                    self.assertFalse(receipt["passed"])
                    self.assertEqual(parent.returncode, 0)
                    self.assertEqual(original(child["pid"])["state"], "alive")
                    if scenario != "residual":
                        kill.assert_not_called()
                        pidfd.assert_not_called()
                        self.assertEqual(manager.records[child["pid"]]["final"]["state"], "identity_unknown")
                    else:
                        self.assertTrue(any(e.get("error_type") == "PermissionError" for e in manager.records[child["pid"]]["events"]))
                asyncio.run(check())

    def run_adapter(self, exit_early):
        # The ST-84 negative case: a ready child ignores SIGTERM; session/new
        # fails. The second case exits its parent after initialize instead.
        child_code = "process.on('SIGTERM',()=>{}); console.log('ready'); setInterval(()=>{},1000);"
        adapter = self.root / "fake-adapter.cjs"
        adapter.write_text("const {spawn}=require('child_process'); const readline=require('readline'); const fs=require('fs');\n" +
            "const child=spawn(process.execPath,['-e'," + json.dumps(child_code) + "],{stdio:['ignore','pipe','ignore']});\n" +
            "fs.writeFileSync(" + json.dumps(str(self.root / "child.pid")) + ", String(child.pid));\n" +
            "child.stdout.once('data',()=>{const rl=readline.createInterface({input:process.stdin}); rl.on('line',line=>{ const q=JSON.parse(line); if(!q.id)return; if(q.method==='initialize') console.log(JSON.stringify({jsonrpc:'2.0',id:q.id,result:{protocolVersion:1}})); else " +
            ("process.exit(0)" if exit_early else "console.log(JSON.stringify({jsonrpc:'2.0',id:q.id,error:{code:-32603,message:'synthetic session startup failure'}}))") + "; });});\n")
        source = self.root / "source.toml"
        source.write_text('model_provider="fixture"\n[model_providers.fixture]\nbase_url="http://127.0.0.1:1/v1"\nwire_api="responses"\n')
        process = subprocess.Popen([sys.executable, str(PROBE_SCRIPT), "--root", str(self.root / "private"),
            "--output", str(self.root / "evidence"), "--source-config", str(source), "--adapter", str(adapter),
            "--label", "cleanup", "--model", "fixture", "--timeout", "1"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        probe = self.remember(PROCESSES["process_info"](process.pid))
        deadline = time.monotonic() + 20
        captured = False
        while process.poll() is None and time.monotonic() < deadline:
            if not captured and (pidfile := self.root / "child.pid").exists() and pidfile.read_text():
                child = PROCESSES["process_info"](int(pidfile.read_text()))
                if child:
                    adapter_info = PROCESSES["process_info"](child["ppid"])
                    if adapter_info and adapter_info["ppid"] == probe["pid"]:
                        self.remember(adapter_info)
                        self.remember(child)
                        captured = True
            time.sleep(.005)
        stdout, stderr = process.communicate(timeout=1)
        self.assertTrue(captured, "test must verify its own new child's ancestry before any fallback cleanup")
        self.assertEqual(process.returncode, 1, stderr)
        result = json.loads((self.root / "evidence/cleanup/result.json").read_text())
        self.save("probe-parent-exit" if exit_early else "probe-review-negative", result)
        self.assertIn(child["pid"], result["owned_pids_at_cleanup"])
        self.assertTrue(result["cleanup"]["passed"], result)
        self.assertFalse(result["passed"])
        state = PROCESSES["process_info"](child["pid"])
        self.assertTrue(state is None or state["state"] == "zombie", (stdout, stderr, state))
        self.assertEqual(result["adapter_exit_code"], 0 if exit_early else -signal.SIGTERM)
        if not exit_early:
            self.assertIn("synthetic session startup failure", result["error"])

    def test_review_negative_case_reaps_ignoring_child(self):
        self.run_adapter(False)

    def test_probe_cleans_tracked_child_when_adapter_exits_before_response(self):
        self.run_adapter(True)

    def test_successful_protocol_cannot_override_failed_cleanup(self):
        async def check():
            source = self.root / "source.toml"
            source.write_text('model_provider="fixture"\n')
            adapter = self.root / "idle.cjs"
            adapter.write_text("process.stdin.resume();process.on('SIGTERM',()=>process.exit(0));")
            probe = PROBE["Probe"](argparse.Namespace(label="cleanup-gate", model="fixture", root=self.root / "private",
                output=self.root / "evidence", source_config=source, adapter=adapter, trace_provider=False, timeout=1))

            async def request(method, params, timeout=60):
                self.remember(PROCESSES["process_info"](probe.process.pid))
                if method == "session/new":
                    return {"sessionId": "fixture-session"}
                return {"configOptions": [{"id": "mode", "currentValue": "read-only"},
                    {"id": "model", "currentValue": "fixture"}, {"id": "reasoning_effort", "currentValue": "high"}]}

            async def prompt(name, *args):
                phase = {"passed": True, "completed": True, "reply": "fixture-marker", "tool_updates": [
                    {"sessionUpdate": "tool_call", "toolCallId": "fixture", "rawInput": {"server": "steve", "tool": "steve_probe_read"}},
                    {"sessionUpdate": "tool_call_update", "toolCallId": "fixture", "status": "completed",
                     "rawOutput": {"result": {"content": [{"type": "text", "text": "fixture-marker"}]}}}]}
                if name == "text":
                    phase["reply"] = args[0].split("Reply exactly: ")[1]
                    phase["tool_updates"] = []
                else:
                    protocol = [{"direction": "receive", "message": {"id": 1, "method": "tools/call", "params": {"name": "steve_probe_read"}}},
                        {"direction": "send", "message": {"id": 1, "result": {"content": [{"type": "text", "text": "fixture-marker"}]}}}]
                    (probe.out / "mcp-protocol.jsonl").write_text("\n".join(map(json.dumps, protocol)))
                probe.result["phases"][name] = phase
                return phase

            cleanup = PROBE["OwnedProcesses"].cleanup

            async def failed_cleanup(manager):
                receipt = await cleanup(manager)
                receipt.update(passed=False, injected_failure="synthetic incomplete cleanup receipt")
                return receipt

            probe.request, probe.prompt = request, prompt
            with mock.patch.object(PROBE["OwnedProcesses"], "cleanup", failed_cleanup):
                passed = await probe.run()
            self.save("passing-protocol-failed-cleanup", probe.result)
            self.assertEqual(set(probe.result["phases"]), {"session_start", "text", "mcp"})
            self.assertTrue(all(p["passed"] for p in probe.result["phases"].values()))
            self.assertFalse(passed)
            self.assertFalse(probe.result["passed"])
        asyncio.run(check())


if __name__ == "__main__":
    unittest.main(verbosity=2)
