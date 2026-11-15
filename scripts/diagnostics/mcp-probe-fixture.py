"""Isolated, read-only stdio MCP fixture. Never connects to a Hub."""
import json
import pathlib
import secrets
import sys
import time

log = pathlib.Path(sys.argv[1])
marker = "MCP-PROBE-" + secrets.token_hex(16)


def record(direction, message):
    with log.open("a") as stream:
        stream.write(json.dumps({"at": time.time(), "direction": direction, "message": message}) + "\n")


for line in sys.stdin:
    request = json.loads(line)
    record("receive", request)
    if "id" not in request:
        continue
    method = request.get("method")
    if method == "initialize":
        result = {"protocolVersion": request["params"]["protocolVersion"],
                  "capabilities": {"tools": {}},
                  "serverInfo": {"name": "isolated-read-only-fixture", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [{"name": "steve_probe_read", "description": "Return an isolated diagnostic marker. No real Steve data access.",
                             "inputSchema": {"type": "object", "properties": {}, "additionalProperties": False},
                             "annotations": {"readOnlyHint": True, "destructiveHint": False}}]}
    elif method == "tools/call" and request.get("params", {}).get("name") == "steve_probe_read":
        result = {"content": [{"type": "text", "text": marker}], "isError": False}
    else:
        response = {"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "fixture method/tool unsupported"}}
        record("send", response)
        print(json.dumps(response), flush=True)
        continue
    response = {"jsonrpc": "2.0", "id": request["id"], "result": result}
    record("send", response)
    print(json.dumps(response), flush=True)
