# A finite, scripted ACP peer. It touches only files passed by the test, never runs tools or sleeps.
import json, os, sys
steps = json.load(open(sys.argv[1]))
log = sys.argv[2]
for line in sys.stdin:
    req = json.loads(line)
    fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    os.write(fd, (json.dumps({"request": req["method"], "params": req.get("params")}) + "\n").encode())
    os.close(fd)
    if not steps or req["method"] != steps[0]["method"]:
        answer = {"error": {"code": -32099, "message": "unexpected scripted method: " + req["method"]}}
    else:
        step = steps.pop(0)
        answer = {"error": step["error"]} if "error" in step else {"result": step["result"]}
    print(json.dumps({"jsonrpc": "2.0", "id": req["id"], **answer}), flush=True)
