"""Bounded Codex ACP/MCP probe with fresh state and generated diagnostic traffic only.

Provider settings (including the owner's auth helper) stay in the private state
directory. Evidence excludes that directory and adapter stderr. No Hub is used.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import runpy
import secrets
import shutil
import sys
import time
import tomllib

OwnedProcesses = runpy.run_path(str(Path(__file__).with_name("owned-processes.py")))["OwnedProcesses"]


def dump(path, value):
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")


def mcp_outcome(phase, protocol):
    calls = [(i, e["message"]) for i, e in enumerate(protocol)
             if e["direction"] == "receive" and e["message"].get("method") == "tools/call"]
    valid = [(i, c) for i, c in calls if c.get("id") is not None and c.get("params", {}).get("name") == "steve_probe_read"]
    responses = [e["message"] for i, e in enumerate(protocol) if e["direction"] == "send"
                 and any(i > at and c["id"] == e["message"].get("id") for at, c in valid)]

    def text_results(result):
        if not isinstance(result, dict) or result.get("isError", False):
            return []
        return [c["text"] for c in result.get("content", []) if c.get("type") == "text" and c.get("text")]

    markers = [m for response in responses if "error" not in response for m in text_results(response.get("result"))]
    # ACP call ids differ from the stdio MCP request id. Join the ACP start and
    # completion by toolCallId, then match its result against the service marker.
    # A completed tool search or an unrelated native tool is not an MCP receipt.
    acp_calls = {}
    for update in phase["tool_updates"]:
        call_id = update.get("toolCallId")
        if not call_id:
            continue
        raw = update.get("rawInput") or {}
        if update.get("sessionUpdate") == "tool_call" and raw.get("server") == "steve" and raw.get("tool") == "steve_probe_read":
            acp_calls[call_id] = update
        elif call_id in acp_calls:
            acp_calls[call_id] = {**acp_calls[call_id], **update}
    acp_completed = any(u.get("status") == "completed" and not (u.get("rawOutput") or {}).get("error")
                        and any(m in text_results((u.get("rawOutput") or {}).get("result")) for m in markers)
                        for u in acp_calls.values())
    # Older recordings lack final_reply; replay their ACP events to derive it
    # when auditing consumption. New probes always record the final text too.
    consumed = any(m in phase.get("final_reply", phase["reply"]) for m in markers)
    return {"model_initiated_mcp": bool(calls), "service_returned": bool(markers), "model_consumed": consumed,
            "acp_completed": acp_completed, "server_call_count": len(calls),
            "passed": phase["completed"] and len(calls) == 1 and len(valid) == 1 and len(responses) == 1
            and bool(markers) and consumed and acp_completed}


def prepare(root, source, model, observed_base=None):
    state = root / "state"
    state.mkdir(mode=0o700, parents=True, exist_ok=False)
    config = tomllib.loads(source.read_text())
    if observed_base:
        config["model_providers"][config["model_provider"]]["base_url"] = observed_base
    lines = [f"model = {json.dumps(model)}"]
    for key in ["model_provider", "model_reasoning_effort", "service_tier"]:
        if key in config:
            lines.append(f"{key} = {json.dumps(config[key])}")
    if config.get("model_catalog_json"):
        catalog = state / "models.json"
        shutil.copyfile(config["model_catalog_json"], catalog)
        lines.append(f"model_catalog_json = {json.dumps(str(catalog))}")
    lines += ['approval_policy = "never"', 'sandbox_mode = "read-only"', 'web_search = "disabled"', '[features]', 'apps = false']

    def table(keys, values):
        lines.append("[" + ".".join(json.dumps(k) for k in keys) + "]")
        for key, value in values.items():
            if not isinstance(value, dict):
                lines.append(json.dumps(key) + " = " + json.dumps(value))
        for key, value in values.items():
            if isinstance(value, dict):
                table(keys + [key], value)

    # Owner's existing provider settings only. No auth.json, refresh tokens, hooks,
    # MCP config, projects, sessions, memories, or production state are imported.
    for key, value in config.get("model_providers", {}).items():
        table(["model_providers", key], value)
    destination = state / "config.toml"
    fd = os.open(destination, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, "w") as stream:
        stream.write("\n".join(lines) + "\n")
    return state


class Probe:
    def __init__(self, args):
        if not args.label or Path(args.label).name != args.label or args.label in (".", ".."):
            raise ValueError("label must be a single directory name")
        private, evidence = args.root.resolve(), args.output.resolve()
        if private == evidence or private in evidence.parents or evidence in private.parents:
            raise ValueError("private state and shareable evidence roots must be separate")
        self.args = args
        self.pending = {}
        self.events = []
        self.n = 0
        self.started = time.monotonic()
        self.result = {"label": args.label, "model": args.model, "at": time.time(), "fixture_only": True, "phases": {}}
        self.out = args.output / args.label
        self.out.mkdir(parents=True, exist_ok=False)

    async def send(self, message):
        self.events.append({"at": time.time(), "direction": "send", "message": message})
        self.process.stdin.write((json.dumps(message) + "\n").encode())
        await self.process.stdin.drain()

    async def read(self):
        try:
            async for line in self.process.stdout:
                message = json.loads(line)
                self.events.append({"at": time.time(), "direction": "receive", "message": message})
                # Capture descendants before releasing a failure response to
                # run(): cancellation or adapter exit may reparent them next.
                await self.owned.observe()
                if "id" in message and "method" in message:
                    if message["method"] == "session/request_permission":
                        answer = {"result": {"outcome": {"outcome": "cancelled"}}}
                    else:
                        answer = {"error": {"code": -32601, "message": "diagnostic client permits no filesystem or terminal operations"}}
                    await self.send({"jsonrpc": "2.0", "id": message["id"], **answer})
                elif message.get("id") in self.pending:
                    future = self.pending.pop(message["id"])
                    if not future.done():
                        future.set_result(message)
        finally:
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(RuntimeError("adapter stdout closed before ACP response"))

    async def request(self, method, params, timeout=60):
        self.n += 1
        request_id = self.n
        future = asyncio.get_running_loop().create_future()
        self.pending[request_id] = future
        await self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        try:
            response = await asyncio.wait_for(future, timeout)
        except asyncio.TimeoutError:
            raise TimeoutError(f"ACP {method} did not complete within {timeout} seconds") from None
        finally:
            self.pending.pop(request_id, None)
        if "error" in response:
            raise RuntimeError(json.dumps(response["error"], ensure_ascii=False))
        return response["result"]

    def updates(self, start):
        return [e["message"].get("params", {}).get("update", {}) for e in self.events[start:]
                if e["message"].get("method") == "session/update"]

    async def prompt(self, name, prompt, timeout):
        start = len(self.events)
        at = time.monotonic()
        phase = {"completed": False, "timeout_seconds": timeout}
        try:
            phase["result"] = await self.request("session/prompt", {"sessionId": self.sid, "prompt": [{"type": "text", "text": prompt}]}, timeout)
            phase["completed"] = True
        except Exception as error:
            phase["error"] = str(error)
            phase["error_type"] = type(error).__name__
        phase["elapsed_seconds"] = round(time.monotonic() - at, 3)
        updates = self.updates(start)
        phase["reply"] = "".join(u.get("content", {}).get("text", "") for u in updates if u.get("sessionUpdate") == "agent_message_chunk")
        phase["final_reply"] = ""
        for update in updates:
            if update.get("sessionUpdate") in ("tool_call", "tool_call_update"):
                phase["final_reply"] = ""
            elif update.get("sessionUpdate") == "agent_message_chunk":
                phase["final_reply"] += update.get("content", {}).get("text", "")
        phase["tool_updates"] = [u for u in updates if u.get("sessionUpdate") in ("tool_call", "tool_call_update")]
        self.result["phases"][name] = phase
        dump(self.out / "result.json", self.result)
        return phase

    async def run(self):
        root = self.args.root / self.args.label
        observer = None
        observed_base = None
        stderr = None
        try:
            (self.args.root / "tmp").mkdir(mode=0o700, parents=True, exist_ok=True)
            if self.args.trace_provider:
                config = tomllib.loads(self.args.source_config.read_text())
                upstream = config["model_providers"][config["model_provider"]]["base_url"]
                observer, observed_base = runpy.run_path(str(Path(__file__).with_name("responses-tool-observer.py")))["start_observer"](upstream, self.out / "provider-tool-protocol.jsonl")
            state = prepare(root, self.args.source_config, self.args.model, observed_base)
            cwd = root / "work"
            cwd.mkdir()
            env = dict(os.environ)
            for key in ["CODEX_PATH", "CODEX_CONFIG", "MODEL_PROVIDER", "APP_SERVER_LOGS", "DEFAULT_AUTH_REQUEST", "CLAUDECODE"]:
                env.pop(key, None)
            env["CODEX_HOME"] = str(state)
            env["TMPDIR"] = str(self.args.root / "tmp")
            env["INITIAL_AGENT_MODE"] = "read-only"
            stderr = (root / "adapter.stderr").open("wb")
            self.process = await asyncio.create_subprocess_exec("node", str(self.args.adapter), cwd=cwd, env=env,
                stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=stderr, start_new_session=True)
            self.result["adapter_pid"] = self.process.pid
            self.owned = OwnedProcesses(self.process)
            self.watcher = asyncio.create_task(self.owned.watch())
            self.reader = asyncio.create_task(self.read())
            init = await self.request("initialize", {"protocolVersion": 1, "clientCapabilities": {"fs": {"readTextFile": False, "writeTextFile": False}, "terminal": False}, "clientInfo": {"name": "steve-isolated-diagnostic", "version": "1"}})
            self.result["initialize"] = init
            fixture_log = self.out / "mcp-protocol.jsonl"
            new = await self.request("session/new", {"cwd": str(cwd), "mcpServers": [{"name": "steve", "command": sys.executable, "args": [str(Path(__file__).with_name("mcp-probe-fixture.py")), str(fixture_log)], "env": []}]}, 90)
            self.sid = new["sessionId"]
            dump(self.out / "session-offer.json", new)
            self.result["session_id"] = self.sid
            self.result["phases"]["session_start"] = {"passed": True}
            self.result["mode_set"] = await self.request("session/set_mode", {"sessionId": self.sid, "modeId": "read-only"})
            for key, value in [("model", self.args.model), ("reasoning_effort", "high")]:
                response = await self.request("session/set_config_option", {"sessionId": self.sid, "configId": key, "value": value})
                self.result[key + "_readback"] = [{"id": x["id"], "value": x.get("currentValue")} for x in response.get("configOptions", [])]
                if not any(x["id"] == key and x.get("currentValue") == value for x in response.get("configOptions", [])):
                    raise RuntimeError(f"adapter did not report the requested {key}")
                if key == "model" and not any(x["id"] == "mode" and x.get("currentValue") == "read-only" for x in response.get("configOptions", [])):
                    raise RuntimeError("adapter did not report read-only mode")
            token = "TEXT-PROBE-" + secrets.token_hex(8)
            phase = await self.prompt("text", "Diagnostic only. Do not use any tools. Reply exactly: " + token, 90)
            phase["passed"] = phase["completed"] and phase["reply"].strip() == token and not phase["tool_updates"]
            if not phase["completed"]:
                raise RuntimeError("text turn did not finish; MCP turn was not started")
            prompt = "Diagnostic only. Call steve_probe_read from the steve MCP server exactly once, using tool search if needed. Reply with the returned diagnostic marker. Do not use shell, files, code interpreters, or other tools. Do not modify anything."
            phase = await self.prompt("mcp", prompt, self.args.timeout)
            protocol = [json.loads(line) for line in fixture_log.read_text().splitlines()] if fixture_log.exists() else []
            phase.update(mcp_outcome(phase, protocol))
        except Exception as error:
            self.result["error"] = str(error)
            self.result["error_type"] = type(error).__name__
        finally:
            if hasattr(self, "process"):
                try:
                    # Observe before cancel, including when the adapter has
                    # already exited. The tracker retains reparented children.
                    await self.owned.observe()
                    if self.process.returncode is None and hasattr(self, "sid"):
                        try:
                            await asyncio.wait_for(self.send({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": self.sid}}), 1)
                        except Exception:
                            pass
                    self.watcher.cancel()
                    try:
                        await self.watcher
                    except asyncio.CancelledError:
                        pass
                    self.result["cleanup"] = await self.owned.cleanup()
                    self.result["owned_pids_at_cleanup"] = sorted(self.owned.records)
                except Exception as error:
                    self.result["cleanup"] = {"passed": False, "error_type": type(error).__name__}
                self.reader.cancel()
                try:
                    await self.reader
                except asyncio.CancelledError:
                    pass
                except Exception as error:
                    self.result["reader_error_type"] = type(error).__name__
                self.result["adapter_exit_code"] = self.process.returncode
            else:
                self.result["cleanup"] = {"passed": True, "processes": [], "scope": "no adapter spawned"}
            if stderr:
                stderr.close()
            if observer:
                observer.shutdown()
                observer.server_close()
            self.result["elapsed_seconds"] = round(time.monotonic() - self.started, 3)
            self.result["passed"] = not self.result.get("error") and self.result["cleanup"]["passed"] and all(
                self.result["phases"].get(p, {}).get("passed") for p in ("session_start", "text", "mcp"))
            dump(self.out / "acp-protocol.json", self.events)
            dump(self.out / "result.json", self.result)
        print(json.dumps({"label": self.args.label, "phases": self.result["phases"], "error": self.result.get("error"),
                          "cleanup_passed": self.result["cleanup"]["passed"], "passed": self.result["passed"]}, ensure_ascii=False), flush=True)
        return self.result["passed"]


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    path = lambda value: Path(value).expanduser().resolve()
    parser.add_argument("--root", type=path, required=True, help="dedicated private diagnostic state root")
    parser.add_argument("--output", type=path, required=True, help="evidence directory, separate from private state")
    parser.add_argument("--source-config", type=path, required=True, help="owner's Codex TOML settings; auth.json is never copied")
    parser.add_argument("--adapter", type=path, required=True, help="installed codex-acp JavaScript entry point")
    parser.add_argument("--label", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--timeout", type=int, default=150)
    parser.add_argument("--trace-provider", action="store_true")
    raise SystemExit(0 if asyncio.run(Probe(parser.parse_args()).run()) else 1)
