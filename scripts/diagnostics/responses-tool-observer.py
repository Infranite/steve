"""Local forwarding observer: saves only tool schemas/call names, never request headers or prompts."""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading
import time
import urllib.error
import urllib.request
import urllib.parse


def start_observer(upstream, log):
    origin = urllib.parse.urlsplit(upstream)
    lock = threading.Lock()
    counter = [0]

    def record(value):
        with lock, log.open("a") as stream:
            stream.write(json.dumps({"at": time.time(), **value}, ensure_ascii=False) + "\n")

    def tool_schema(tool):
        return {key: ([tool_schema(t) for t in value] if key == "tools" else value)
                for key, value in tool.items() if key in ("type", "name", "tools", "parameters")}

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *args):
            pass

        def do_POST(self):
            with lock:
                counter[0] += 1
                request_id = counter[0]
            raw = self.rfile.read(int(self.headers["Content-Length"]))
            decoded = raw
            try:
                encoding = self.headers.get("Content-Encoding")
                if encoding == "zstd":
                    from compression import zstd
                    decoded = zstd.decompress(raw)
                elif encoding == "gzip":
                    import gzip
                    decoded = gzip.decompress(raw)
                payload = json.loads(decoded)
                # Exclude all prompts, headers, URLs, auth, settings and history.
                feedback = [{k: v for k, v in item.items() if k in ("type", "name", "namespace", "call_id", "output")}
                            for item in payload.get("input", []) if item.get("type") in ("function_call", "function_call_output", "custom_tool_call_output")]
                record({"direction": "request", "request_id": request_id, "model": payload.get("model"),
                        "tools": [tool_schema(t) for t in payload.get("tools", [])], "tool_feedback": feedback})
            except Exception as error:
                record({"direction": "observation_error", "request_id": request_id, "type": type(error).__name__})
            headers = {k: v for k, v in self.headers.items() if k.lower() not in ("host", "connection", "transfer-encoding", "content-length", "accept-encoding")}
            headers["Accept-Encoding"] = "identity"
            url = urllib.parse.urlunsplit((origin.scheme, origin.netloc, self.path, "", ""))
            req = urllib.request.Request(url, data=raw, headers=headers, method="POST")
            try:
                response = urllib.request.urlopen(req, timeout=170)
            except urllib.error.HTTPError as error:
                response = error
            except Exception as error:
                record({"direction": "forward_error", "request_id": request_id, "type": type(error).__name__})
                self.send_error(502, "isolated observer forwarding failed")
                return
            with response:
                record({"direction": "status", "request_id": request_id, "status": response.status})
                self.send_response(response.status)
                for key, value in response.headers.items():
                    if key.lower() not in ("content-length", "transfer-encoding", "connection", "set-cookie"):
                        self.send_header(key, value)
                self.send_header("Connection", "close")
                self.end_headers()
                try:
                    for line in response:
                        if line.startswith(b"data:"):
                            try:
                                event = json.loads(line[5:])
                                kind = event.get("type", "")
                                item = event.get("item", {})
                                if kind in ("response.output_item.added", "response.output_item.done"):
                                    record({"direction": "response", "request_id": request_id, "event": kind,
                                            "item": {k: v for k, v in item.items() if k in ("type", "name", "namespace", "call_id", "arguments")}})
                                elif kind in ("response.failed", "error"):
                                    record({"direction": "response", "request_id": request_id, "event": kind})
                            except (ValueError, TypeError):
                                pass
                        self.wfile.write(line)
                        self.wfile.flush()
                except (BrokenPipeError, ConnectionResetError):
                    pass
                self.close_connection = True

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, f"http://127.0.0.1:{server.server_port}{origin.path}"
