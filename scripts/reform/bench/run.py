#!/usr/bin/env python3
"""Repeatable, isolated Hub benchmark. Python 3.11+, standard library only."""
import argparse
import collections
import ctypes
import hashlib
import http.cookiejar
import http.client
import json
import math
import os
from pathlib import Path
import platform
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

DEFAULT = {
    "endpoints": [{"path": p, "requests": 20} for p in (
        "/api/state", "/api/stats/summary", "/api/hub-metadata", "/api/personas",
        "/api/spaces", "/api/channels", "/api/me/attention", "/api/devices")],
    "request_timeout_s": 10,
    "startup_timeout_s": 90,
    "total_timeout_s": 540,
    "idle_s": 20,
    "load_s": 60,
    "clients": 4,
    "load_path": "/api/state",
    "invalidate_rps": 2,
    "invalidate_path": "/api/notifications/read",
    "invalidate_body": {},
    "lock_interval_ms": 5,
    "process_interval_s": 1,
    "cpu_top_n": 8,
    "cli_runs": 3,
    "cli_timeout_s": 10,
}


def distribution(values):
    """Linear interpolation at (n-1)*p, like statistics.quantiles inclusive."""
    ordered = sorted(values)
    def q(p):
        if not ordered:
            return None
        index = (len(ordered) - 1) * p
        lo, hi = math.floor(index), math.ceil(index)
        return ordered[lo] + (ordered[hi] - ordered[lo]) * (index - lo)
    return {"count": len(ordered), "p50": q(.5), "p90": q(.9),
            "p99": q(.99), "max": ordered[-1] if ordered else None}


def configuration(path):
    config = json.loads(json.dumps(DEFAULT))
    if path:
        override = json.loads(path.read_text())
        if not isinstance(override, dict) or override.keys() - config.keys():
            raise ValueError("config must be an object containing known keys")
        config.update(override)
    for key in ("request_timeout_s", "startup_timeout_s", "total_timeout_s", "idle_s",
                "load_s", "lock_interval_ms", "process_interval_s", "cli_timeout_s"):
        value = config[key]
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
            raise ValueError(f"{key} must be finite and positive")
    for key in ("clients", "cpu_top_n", "cli_runs"):
        if type(config[key]) is not int or config[key] < 1:
            raise ValueError(f"{key} must be a positive integer")
    rate = config["invalidate_rps"]
    if isinstance(rate, bool) or not isinstance(rate, (int, float)) or not math.isfinite(rate) or rate < 0:
        raise ValueError("invalidate_rps must be finite and nonnegative")
    def api_path(value):
        return isinstance(value, str) and value.startswith("/api/") and not any(c in value for c in "\r\n#")
    if not api_path(config["load_path"]) or not api_path(config["invalidate_path"]):
        raise ValueError("load and invalidation paths must start with /api/")
    if not isinstance(config["endpoints"], list) or not config["endpoints"]:
        raise ValueError("endpoints must be a nonempty list")
    paths = []
    for item in config["endpoints"]:
        if not isinstance(item, dict) or set(item) != {"path", "requests"} or not api_path(item["path"]):
            raise ValueError("endpoint needs path and requests")
        if type(item["requests"]) is not int or item["requests"] < 1:
            raise ValueError("endpoint requests must be a positive integer")
        paths.append(item["path"])
    if len(set(paths)) != len(paths):
        raise ValueError("duplicate endpoint paths")
    return config


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Client:
    def __init__(self, base, timeout, cookie=""):
        self.base, self.timeout, self.cookie = base, timeout, cookie
        # Do not inherit proxy settings, or follow redirects into another service.
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect())

    def request(self, path, data=None):
        headers = {"Cookie": self.cookie, "Accept-Encoding": "gzip", "Origin": self.base}
        if data is not None:
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(self.base + path, data=data, headers=headers)
        start = time.perf_counter()
        code, size, error = 0, 0, None
        try:
            with self.opener.open(req, timeout=self.timeout) as response:
                code, body = response.status, response.read()
                size = len(body)
        except urllib.error.HTTPError as exc:
            code, size, error = exc.code, len(exc.read()), "http"
        except (OSError, urllib.error.URLError, TimeoutError, http.client.HTTPException) as exc:
            error = type(exc).__name__
        return {"ms": (time.perf_counter() - start) * 1000,
                "status": code, "bytes": size, "error": error}

    def login(self):
        jar = http.cookiejar.CookieJar()
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}),
                                             urllib.request.HTTPCookieProcessor(jar), NoRedirect())
        try:
            opener.open(self.base + "/auth/dev", timeout=self.timeout).close()
        except urllib.error.HTTPError as exc:
            if exc.code not in (302, 303, 307):
                raise RuntimeError("dev login failed") from None
            exc.close()
        self.cookie = "; ".join(f"{c.name}={c.value}" for c in jar)
        if not self.cookie:
            raise RuntimeError("dev login returned no cookie")


def requests_summary(rows):
    return {"latency_ms": distribution([r["ms"] for r in rows]),
            "response_bytes": distribution([r["bytes"] for r in rows]),
            "total_response_bytes": sum(r["bytes"] for r in rows),
            "status_codes": dict(collections.Counter(str(r["status"]) for r in rows)),
            "errors": sum(r["error"] is not None for r in rows)}


class LockProbe:
    def __init__(self, db, interval):
        self.db, self.interval = db, interval
        self.busy = self.free = 0
        self.streaks, self.errors = [], collections.Counter()
        self.started = None

    def sample(self, stop, deadline):
        with sqlite3.connect(self.db, timeout=0, isolation_level=None) as conn:
            conn.execute("PRAGMA busy_timeout=0")
            while not stop.is_set() and time.perf_counter() < deadline:
                now = time.perf_counter()
                try:
                    conn.execute("BEGIN IMMEDIATE")
                    conn.execute("ROLLBACK")
                    self.free += 1
                    if self.started is not None:
                        self.streaks.append((now - self.started) * 1000)
                        self.started = None
                except sqlite3.OperationalError as exc:
                    # BUSY/LOCKED only: corruption and I/O errors are not lock occupancy.
                    if (getattr(exc, "sqlite_errorcode", 0) & 255) in (5, 6):
                        self.busy += 1
                        if self.started is None:
                            self.started = now
                    else:
                        self.errors[type(exc).__name__] += 1
                stop.wait(self.interval)
        if self.started is not None:
            self.streaks.append((time.perf_counter() - self.started) * 1000)

    def result(self):
        total = self.busy + self.free
        return {"probes": total, "busy_probes": self.busy,
                "busy_ratio": self.busy / total if total else None,
                "busy_segments_ms": distribution(self.streaks),
                "segments_over_1s": sum(v > 1000 for v in self.streaks),
                "probe_errors": dict(self.errors)}


U64 = ctypes.c_uint64


class RUsage(ctypes.Structure):
    # Darwin rusage_info_v4; the layout matches libproc's public ABI.
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [(name, U64) for name in (
        "user", "system", "pkg_idle", "intr", "pageins", "wired", "resident", "phys",
        "start", "exit", "c_user", "c_system", "c_pkg_idle", "c_intr", "c_pageins", "c_elapsed",
        "dread", "dwritten", "q1", "q2", "q3", "q4", "q5", "q6", "q7", "billed_sys",
        "serviced_sys", "logical_writes", "lifetime_max_phys", "instr", "cycles", "billed_e",
        "serviced_e", "interval_max_phys", "runnable")]


def process_table():
    raw = subprocess.check_output(["ps", "-axo", "pid=,ppid=,pcpu=,comm="], text=True, timeout=5)
    rows = []
    for line in raw.splitlines():
        bits = line.strip().split(None, 3)
        if len(bits) == 4:
            try:
                rows.append((int(bits[0]), int(bits[1]), float(bits[2]), Path(bits[3]).name))
            except ValueError:
                continue
    return rows


def live_groups(groups):
    # Darwin may return EPERM to killpg(..., 0) while a detached child is
    # being reaped. Inspect non-zombie members rather than treating that race
    # as a surviving process or trying to signal an already dead process.
    raw = subprocess.check_output(["ps", "-axo", "pgid=,stat="], text=True, timeout=5)
    return {int(bits[0]) for line in raw.splitlines()
            if len(bits := line.split()) == 2 and int(bits[0]) in groups and not bits[1].startswith("Z")}


class DiskMeter:
    def __init__(self, pid):
        self.pid, self.records, self.errors = pid, {}, collections.Counter()
        self.lib = None
        if sys.platform == "darwin":
            self.lib = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
            self.lib.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
            self.lib.proc_pid_rusage.restype = ctypes.c_int

    def sample(self, table, baseline=False):
        if self.lib is None:
            return
        descendants = {self.pid}
        while True:
            found = {p for p, parent, _, _ in table if parent in descendants}
            if found <= descendants:
                break
            descendants.update(found)
        names = {p: name for p, _, _, name in table}
        for pid in descendants:
            r = RUsage()
            if self.lib.proc_pid_rusage(pid, 4, ctypes.byref(r)) != 0:
                self.errors["rusage_unavailable"] += 1
                continue
            key = (pid, r.start)
            current = (r.dwritten, r.logical_writes)
            if key not in self.records:
                # A child born after phase start contributes its lifetime counters.
                self.records[key] = {"pid": pid, "name": names.get(pid, "unknown"),
                                     "first": current if baseline else (0, 0),
                                     "last": current, "present_at_end": False}
            self.records[key]["last"] = current
            self.records[key]["present_at_end"] = True
        for key, record in self.records.items():
            if key[0] not in descendants:
                record["present_at_end"] = False

    def result(self, seconds):
        rows = []
        for record in self.records.values():
            device, logical = [max(0, b - a) for a, b in zip(record["first"], record["last"])]
            rows.append({k: record[k] for k in ("pid", "name", "present_at_end")} |
                        {"device_bytes": device, "logical_bytes": logical,
                         "device_Bps": device / seconds, "logical_Bps": logical / seconds})
        available = self.lib is not None and bool(rows)
        return {"available": available, "method": "proc_pid_rusage_v4" if self.lib else "unsupported",
                "processes": rows, "sampling_errors": dict(self.errors),
                "device_bytes": sum(r["device_bytes"] for r in rows) if available else None,
                "logical_bytes": sum(r["logical_bytes"] for r in rows) if available else None,
                "device_Bps": sum(r["device_Bps"] for r in rows) if available else None,
                "logical_Bps": sum(r["logical_Bps"] for r in rows) if available else None}


class Processes:
    def __init__(self):
        self.children = []
        self.lock = threading.Lock()
        self.home = None

    def start(self, args, env, **kwargs):
        # No inherited environment is ever passed to the binary, including CLI.
        assert "STEVE_HOME" in env and "STEVE_PORT" in env
        assert safe_work(Path(env["STEVE_HOME"])) == Path(env["STEVE_HOME"])
        assert 8440 not in (int(env["STEVE_PORT"]), int(env["STEVE_LOCAL_NODE_PORT"]))
        assert env["STEVE_NO_RESUME"] == "1" and env["STEVE_BACKUP"] == "off"
        assert "STEVE_CONFIG" not in env and "INVOCATION_ID" not in env
        self.home = Path(env["STEVE_HOME"])
        with self.lock:
            child = subprocess.Popen(["/usr/bin/env", "-i", *[f"{k}={v}" for k, v in env.items()], *args],
                                     env={}, start_new_session=True, **kwargs)
            self.children.append(child)
        return child

    @staticmethod
    def kill_group(child, sig):
        try:
            os.killpg(child.pid, sig)
        except ProcessLookupError:
            pass

    def cleanup(self):
        with self.lock:
            roots = {p.pid for p in self.children if p.poll() is None}
            # Local Node deliberately calls setsid(), so the Hub group alone is
            # insufficient. The fresh home's PID file survives Hub failure. Verify
            # its inherited isolation variables before accepting an orphan's PID.
            if self.home:
                try:
                    pid = int((self.home / "data/node/service.pid").read_text())
                    command = subprocess.check_output(
                        ["ps", "eww", "-p", str(pid), "-o", "command="], text=True, timeout=5)
                    if "--local-node" in command and f"STEVE_HOME={self.home} " in command:
                        roots.add(pid)
                except (OSError, ValueError, subprocess.SubprocessError):
                    pass
            groups = {p.pid for p in self.children}
            frozen = set()
            # Freeze parents before enumerating descendants, preventing respawn
            # between tree discovery and termination (including detached sessions).
            while roots - frozen:
                for pid in roots - frozen:
                    try:
                        os.kill(pid, signal.SIGSTOP)
                        group = os.getpgid(pid)
                        if group != os.getpgrp():
                            groups.add(group)
                    except (ProcessLookupError, PermissionError):
                        pass
                    frozen.add(pid)
                table = process_table()
                roots.update(pid for pid, parent, _, _ in table if parent in roots)
            def send(group, sig):
                try:
                    os.killpg(group, sig)
                except (ProcessLookupError, PermissionError):
                    pass
            for group in groups:
                send(group, signal.SIGTERM)
                send(group, signal.SIGCONT)
            limit = time.perf_counter() + 3
            remaining = set(groups)
            while remaining and time.perf_counter() < limit:
                for child in self.children:
                    child.poll()
                remaining = live_groups(remaining)
                if remaining:
                    time.sleep(.05)
            for group in remaining:
                send(group, signal.SIGKILL)
            for child in self.children:
                child.wait(timeout=5)
            # Detached Nodes are reaped by the OS, not Popen.wait().
            for _ in range(50):
                remaining = live_groups(remaining)
                if not remaining:
                    break
                time.sleep(.05)
            return {"process_groups_remaining": sorted(remaining)}


def phase(name, seconds, hub, db, client, config, env, processes, binary):
    start = time.perf_counter()
    deadline = start + seconds
    stop = threading.Event()
    probe = LockProbe(db, config["lock_interval_ms"] / 1000)
    disk = DiskMeter(hub.pid)
    disk.sample(process_table(), baseline=True)
    reads, writes, cli, thread_errors = [], [], [], []
    def get_loop():
        local = Client(client.base, client.timeout, client.cookie)
        while not stop.is_set() and time.perf_counter() < deadline:
            reads.append(local.request(config["load_path"]))
    def invalidate_loop():
        local = Client(client.base, client.timeout, client.cookie)
        rate, tick = config["invalidate_rps"], start
        body = json.dumps(config["invalidate_body"]).encode()
        while rate > 0 and not stop.is_set() and tick < deadline:
            if stop.wait(max(0, tick - time.perf_counter())) or time.perf_counter() >= deadline:
                break
            writes.append(local.request(config["invalidate_path"], body))
            # Skip missed slots: slow writes must not cause a catch-up burst.
            tick = max(tick + 1 / rate, time.perf_counter())
    def cli_loop():
        for index in range(config["cli_runs"]):
            target = start + seconds * (index + 1) / (config["cli_runs"] + 1)
            if stop.wait(max(0, target - time.perf_counter())) or time.perf_counter() >= deadline:
                break
            before = time.perf_counter()
            child = processes.start([str(binary), "--access", "token", "create", "--name",
                                     f"bench-{index}", "--json"], env,
                                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            timed_out = False
            try:
                child.wait(timeout=min(config["cli_timeout_s"], max(.001, deadline - before)))
            except subprocess.TimeoutExpired:
                timed_out = True
                processes.kill_group(child, signal.SIGKILL)
                child.wait(timeout=5)
            cli.append({"ms": (time.perf_counter() - before) * 1000,
                        "returncode": child.returncode, "timed_out": timed_out})
    def guarded(fn):
        try:
            fn()
        except Exception as exc:
            thread_errors.append(type(exc).__name__)
            stop.set()
    funcs = [lambda: probe.sample(stop, deadline)]
    if name == "load":
        funcs += [get_loop] * config["clients"] + [invalidate_loop, cli_loop]
    threads = [threading.Thread(target=guarded, args=(fn,), daemon=True) for fn in funcs]
    try:
        for thread in threads:
            thread.start()
        while time.perf_counter() < deadline and not stop.is_set():
            if hub.poll() is not None:
                raise RuntimeError("Hub exited during measurement")
            table = process_table()
            disk.sample(table)
            stop.wait(min(config["process_interval_s"], max(0, deadline - time.perf_counter())))
    finally:
        stop.set()
        for thread in threads:
            thread.join(timeout=config["request_timeout_s"] + 6)
    duration = time.perf_counter() - start
    disk.sample(process_table())
    result = {"target_s": seconds, "elapsed_s": duration, "lock": probe.result(),
              "disk": disk.result(duration), "worker_errors": thread_errors}
    if name == "load":
        result.update({"reads": requests_summary(reads), "writes": requests_summary(writes),
                       "read_requests_per_s": len(reads) / duration,
                       "write_requests_per_s": len(writes) / duration,
                       "successful_reads_per_s": sum(r["error"] is None for r in reads) / duration,
                       "errors": sum(r["error"] is not None for r in reads + writes),
                       "cli": {"runs": cli, "latency_ms": distribution([r["ms"] for r in cli]),
                               "errors": sum(r["returncode"] != 0 for r in cli)}})
    return result


def load_average():
    # uptime is read for the requested source, but never save its user/host details.
    raw = subprocess.check_output(["uptime"], text=True, timeout=5)
    import re
    match = re.search(r"load averages?:\s*(.*)", raw)
    if not match:
        raise RuntimeError("uptime has no load averages")
    return [float(x) for x in re.findall(r"\d+(?:\.\d+)?", match[1])[:3]]


def host_info():
    info = {"system": platform.system(), "release": platform.release(),
            "architecture": platform.machine(), "logical_cpus": os.cpu_count(),
            "model": None, "memory_bytes": None}
    if sys.platform == "darwin":
        for key, sysctl in (("model", "hw.model"), ("memory_bytes", "hw.memsize")):
            value = subprocess.check_output(["sysctl", "-n", sysctl], text=True, timeout=5).strip()
            info[key] = int(value) if key == "memory_bytes" else value
    else:
        info["model"] = platform.machine()
        try:
            info["memory_bytes"] = os.sysconf("SC_PAGE_SIZE") * os.sysconf("SC_PHYS_PAGES")
        except (ValueError, OSError):
            pass
    return info


def safe_work(path):
    path = path.expanduser().resolve()
    user_home = Path.home().resolve()
    # Reject production homes (including symlink aliases) and their descendants.
    if path == user_home or path == Path("/") or any(
            p.parent == user_home and p.name.startswith(".steve") for p in (path, *path.parents)):
        raise ValueError("work directory must be outside production homes")
    return path


def isolated_environment(home, port):
    # A private HOME also isolates any future subprocess's default config discovery.
    private_home = home / "user-home"
    private_home.mkdir()
    return {"HOME": str(private_home), "USER": "benchmark", "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "TMPDIR": str(home / "tmp"), "STEVE_HOME": str(home), "STEVE_HOST": "127.0.0.1",
            "STEVE_PORT": str(port), "STEVE_LOCAL_NODE_PORT": str(port + 3),
            "STEVE_PUBLIC_URL": f"http://127.0.0.1:{port}", "STEVE_DEV_LOGIN": "1",
            "STEVE_NO_RESUME": "1", "STEVE_VAULT_KEYSTORE": "file", "STEVE_BACKUP": "off",
            "STEVE_ADAPTER_UPDATES": "off", "STEVE_ADAPTER_CLAUDE": "/usr/bin/false",
            "STEVE_ADAPTER_CODEX": "/usr/bin/false", "STEVE_ADAPTER_PI": "/usr/bin/false",
            "NO_COLOR": "1", "RUST_LOG": "warn"}


def health(client, hub, timeout):
    start = time.perf_counter()
    while time.perf_counter() - start < timeout:
        if hub.poll() is not None:
            raise RuntimeError("Hub exited before health check")
        try:
            with client.opener.open(client.base + "/healthz", timeout=min(2, timeout)) as response:
                if response.status == 200:
                    value = json.loads(response.read())
                    # Whitelist fields: health payloads may grow private diagnostics later.
                    return {key: value.get(key) for key in ("version", "build_commit")} | {
                        "local_node_enabled": (value.get("node") or {}).get("enabled")}
        except (OSError, urllib.error.URLError, ValueError):
            pass
        time.sleep(.1)
    raise TimeoutError("Hub startup timeout")


def write_result(path, result):
    path.parent.mkdir(parents=True, exist_ok=True)
    temp = path.with_name(path.name + ".tmp")
    temp.write_text(json.dumps(result, indent=2, ensure_ascii=False, allow_nan=False) + "\n")
    temp.replace(path)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("hub-bin", "snapshot", "work", "out"):
        parser.add_argument("--" + flag, required=True, type=Path)
    parser.add_argument("--port", required=True, type=int)
    parser.add_argument("--config", type=Path)
    parser.add_argument("--label", default="")
    args = parser.parse_args(argv)
    if sys.version_info < (3, 11):
        parser.error("Python 3.11+ is required")
    config = configuration(args.config)
    work = safe_work(args.work)
    if not 1024 <= args.port <= 65532 or 8440 in (args.port, args.port + 3):
        parser.error("unsafe Hub/Node port")
    # Both ports must be free; never authenticate against an existing process.
    for port in (args.port, args.port + 3):
        with socket.socket() as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(("127.0.0.1", port))
    binary, snapshot, out = [p.expanduser().resolve() for p in (args.hub_bin, args.snapshot, args.out)]
    if not binary.is_file() or not os.access(binary, os.X_OK) or not snapshot.is_file():
        parser.error("binary must be executable and snapshot must exist")
    safe_work(out)
    if out in (binary, snapshot) or out.is_relative_to(Path(__file__).resolve().parent):
        parser.error("output must not overwrite inputs or be in tool directory")
    with binary.open("rb") as stream:
        binary_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
    processes, cpu_peaks = Processes(), {}
    result = {"schema_version": 1, "label": args.label, "config": config,
              "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
              "host": host_info(), "load_average_before": load_average(),
              "binary": {"name": binary.name, "sha256": binary_sha256},
              "snapshot": {"name": snapshot.name, "size_bytes": snapshot.stat().st_size},
              "status": "running"}
    started = time.perf_counter()
    cpu_stop = threading.Event()
    def observe_cpu():
        while not cpu_stop.is_set():
            try:
                for _, _, cpu, name in process_table():
                    cpu_peaks[name] = max(cpu, cpu_peaks.get(name, 0))
            except (OSError, subprocess.SubprocessError):
                pass
            cpu_stop.wait(config["process_interval_s"])
    cpu_thread = threading.Thread(target=observe_cpu, daemon=True)
    cpu_thread.start()
    def interrupted(signum, frame):
        raise TimeoutError("total benchmark timeout") if signum == signal.SIGALRM else KeyboardInterrupt()
    old_handlers = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGALRM)}
    signal.setitimer(signal.ITIMER_REAL, config["total_timeout_s"])
    rc = 1
    try:
        work.mkdir(parents=True, exist_ok=True)
        home = Path(tempfile.mkdtemp(prefix="run-", dir=work))
        os.chmod(home, 0o700)
        result["run_directory"] = home.name
        for directory in ("data", "logs", "tmp"):
            (home / directory).mkdir()
        db = home / "data" / "steve.sqlite3"
        before = time.perf_counter()
        shutil.copyfile(snapshot, db)  # Never opens the source through writable SQLite.
        result["snapshot_copy_s"] = time.perf_counter() - before
        with sqlite3.connect(db) as conn:
            changed = conn.execute("UPDATE users SET subject='dev:local' WHERE id=("
                                   "SELECT id FROM users WHERE role='owner' ORDER BY created, id LIMIT 1)").rowcount
            if changed != 1:
                raise RuntimeError("snapshot must contain an owner")
        env = isolated_environment(home, args.port)
        client = Client(f"http://127.0.0.1:{args.port}", config["request_timeout_s"])
        with (home / "logs" / "hub.log").open("wb") as log:
            before = time.perf_counter()
            hub = processes.start([str(binary)], env, stdout=log, stderr=subprocess.STDOUT)
            result["health"] = health(client, hub, config["startup_timeout_s"])
            result["startup_to_health_ms"] = (time.perf_counter() - before) * 1000
            client.login()
            result["endpoints"] = {}
            for endpoint in config["endpoints"]:
                cold = client.request(endpoint["path"])
                rows = [client.request(endpoint["path"]) for _ in range(endpoint["requests"])]
                result["endpoints"][endpoint["path"]] = {"cold": cold, "warm": requests_summary(rows)}
            for name in ("idle", "load"):
                result[name] = phase(name, config[name + "_s"], hub, db, client, config,
                                     env, processes, binary)
            if hub.poll() is not None:
                raise RuntimeError("Hub exited during measurement")
        failed = any(e["cold"]["error"] or e["warm"]["errors"] for e in result["endpoints"].values())
        failed |= any(result[p]["worker_errors"] or result[p]["lock"]["probe_errors"] for p in ("idle", "load"))
        failed |= bool(result["load"]["errors"] or result["load"]["cli"]["errors"])
        failed |= len(result["load"]["cli"]["runs"]) != config["cli_runs"]
        result["status"] = "measurement_errors" if failed else "ok"
        rc = 2 if failed else 0
    except (Exception, KeyboardInterrupt) as exc:
        # Preserve partial evidence without leaking paths, cookies, token output or DB content.
        result["status"], result["failure_type"] = "failed", type(exc).__name__
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        # Cleanup itself must not be interrupted by a second Ctrl-C.
        for sig in (signal.SIGTERM, signal.SIGINT):
            signal.signal(sig, signal.SIG_IGN)
        cpu_stop.set()
        cpu_thread.join(timeout=6)
        result["cleanup"] = processes.cleanup()
        result["elapsed_s"] = time.perf_counter() - started
        result["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        result["load_average_after"] = load_average()
        result["cpu_top"] = [{"name": name, "cpu_percent": value} for name, value in
                             sorted(cpu_peaks.items(), key=lambda item: (-item[1], item[0]))[:config["cpu_top_n"]]]
        if result["cleanup"]["process_groups_remaining"]:
            result["status"], rc = "cleanup_failed", 1
        write_result(out, result)
        for sig, handler in old_handlers.items():
            signal.signal(sig, handler)
    print(json.dumps({"status": result["status"], "elapsed_s": result["elapsed_s"]}))
    return rc


if __name__ == "__main__":
    sys.exit(main())
