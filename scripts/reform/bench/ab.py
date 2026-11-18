#!/usr/bin/env python3
"""Isolated same-binary A/B benchmark (Python 3.11+, stdlib).

Run with python3 -I. Never invoke Hub --version/--help: unknown arguments start
it. Every server/CLI launch uses env -i with asserted STEVE_HOME/STEVE_PORT.
Every round clones one checkpointed golden snapshot; only --var differs.
Defaults preserve run.py's original mixed load. --workload read/write disables
invalidation; warmup_s defaults to 5, before timing. Write performs two strict
200/{"seen":1} smoke requests. auth=bearer creates an isolated owner token before
startup, exchanges it via /auth/token, sets X-Steve-Ws and keeps secrets in RAM.

--env K=V is repeatable. Isolation keys are reserved. --a-unset omits the switch.
--probe records unauthenticated 401/refused without following redirects.
--scrape URL requires auth=bearer: A must refuse, B must return owner 200/plain
text and first-B anonymous 401. Full before/after text goes beside --out; JSON
contains filenames/statuses and numeric differences, never credentials.

Order: ABBA ABBA...; --rounds defaults to 3 per variant. --smoke allows 1+1 but
cannot produce a performance acceptance. Each round calculates throughput and
p50/p95 from full-response samples; primary comparisons use medians of rounds.
Throughput overhead = 100*(1-B/A); latency overhead = 100*(B/A-1). Improvements
are negative. Independent whole-round bootstrap: 5000 draws, seed 20261119.
PASS requires medians and CI upper bounds < --threshold-pct (default 1).
Median >= threshold: FAIL; median below but CI uncertain:
PASS(median)/INCONCLUSIVE(CI). HTTP/timeout errors invalidate a round.
Auxiliary pooled latency, process-tree CPU, disk and SQLite probe data remain.

Each lifecycle has total_timeout_s. All own process groups, including detached
Nodes, are stopped on exit/signals/errors. Copied data is removed after safe
shutdown; raw samples and private logs remain. Never touch production homes or
8440. No push, release build, or production process management is performed.
Exit 0: collected; 2: measurement errors; 1: execution/cleanup failure.
Numeric FAIL/INCONCLUSIVE alone does not change the collection exit code.
"""
import sys
if sys.version_info < (3, 11):
    raise SystemExit("Python 3.11+ is required (on this host use /opt/homebrew/bin/python3.13)")

import argparse
import collections
from contextlib import closing
import ctypes
import errno
import hashlib
import http.client
import importlib.util
import ipaddress
import itertools
import json
import math
import os
from pathlib import Path
import random
import re
import shutil
import signal
import socket
import statistics
import sqlite3
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

# -I excludes the script directory from sys.path; load the sibling explicitly.
_spec = importlib.util.spec_from_file_location("bench_run", Path(__file__).with_name("run.py"))
run = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(run)
SEED, REPLICATES = 20261119, 5000
_spec = importlib.util.spec_from_file_location("bench_telemetry", Path(__file__).with_name("telemetry.py"))
telemetry = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(telemetry)
PRIMARY = ("load.requests_per_s", "load.p50_ms", "load.p95_ms")


def order(rounds):
    return list(itertools.islice(itertools.cycle("ABBA"), rounds * 2))


def quantile(ordered, p):
    if not ordered:
        return None
    index = (len(ordered) - 1) * p
    lo, hi = math.floor(index), math.ceil(index)
    return ordered[lo] + (ordered[hi] - ordered[lo]) * (index - lo)


def percentiles(values):
    ordered = sorted(values)
    return {f"p{p}_ms": quantile(ordered, p / 100) for p in (50, 95, 99)}


class CpuMeter(run.DiskMeter):
    def __init__(self, pid):
        super().__init__(pid)
        self.first = self.last = None

    def sample(self, table, baseline=False):
        if self.lib is None:
            return
        descendants = {self.pid}
        while True:
            found = {p for p, parent, _, _ in table if parent in descendants}
            if found <= descendants:
                break
            descendants.update(found)
        total = 0
        for pid in descendants:
            usage = run.RUsage()
            if self.lib.proc_pid_rusage(pid, 4, ctypes.byref(usage)) != 0:
                self.errors["rusage_unavailable"] += 1
                self.last = None
                return
            total += usage.user + usage.system + usage.c_user + usage.c_system
        if baseline:
            self.first = total
        self.last = total

    def result(self, requests):
        delta = None if self.first is None or self.last is None else self.last - self.first
        available = delta is not None and delta >= 0
        return {"available": available, "method": "proc_pid_rusage_v4_tree_ns",
                "user_system_ns": delta if available else None, "http_requests": requests,
                "us_per_request": delta / 1000 / requests if available and requests else None,
                "sampling_errors": dict(self.errors)}


def clone(source, target):
    if target.exists() or target.is_symlink():
        raise FileExistsError("clone destination must be new")
    if sys.platform == "darwin":
        copied = subprocess.run(["/bin/cp", "-cR", str(source), str(target)],
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        if copied.returncode == 0:
            return "apfs_clone"
        # cp may leave a partial, newly-created destination on unsupported FS.
        if target.is_dir():
            shutil.rmtree(target)
        elif target.exists():
            target.unlink()
    if source.is_dir():
        shutil.copytree(source, target)
    else:
        shutil.copyfile(source, target)
    return "copy"


def loopback(host):
    return host == "localhost" or ipaddress.ip_address(host).is_loopback


def address(value):
    parsed = urllib.parse.urlsplit(value if "://" in value else "//" + value)
    if parsed.username or parsed.password or not parsed.hostname or not loopback(parsed.hostname):
        raise ValueError("only explicit loopback addresses are allowed")
    port = parsed.port
    if port is None or not 1024 <= port <= 65535 or port == 8440:
        raise ValueError("unsafe or missing port")
    return parsed.hostname, port


def environment_options(items, variable):
    extra, ports = {}, []
    # Every fixed isolation key is reserved, including adapters, HOME and PATH.
    with tempfile.TemporaryDirectory() as tmp:
        reserved = set(run.isolated_environment(Path(tmp), 18770)) | {"STEVE_CONFIG", "INVOCATION_ID"}
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", variable) or variable in reserved:
        raise ValueError("invalid or reserved --var")
    for item in items:
        key, sep, value = item.partition("=")
        if not sep or not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", key) or key in reserved:
            raise ValueError("invalid or reserved environment variable")
        if key == variable:
            raise ValueError("--env cannot override --var")
        if key in extra:
            raise ValueError("duplicate --env key")
        ports.extend(environment_ports(key, value))
        extra[key] = value
    return extra, ports


def environment_ports(key, value):
    if value.startswith("unix:"):
        if not value[5:] or not Path(value[5:]).is_absolute():
            raise ValueError("Unix listener needs an absolute isolated path")
        run.safe_work(Path(value[5:]))
    elif ":" in value or key.endswith(("_PORT", "_LISTEN", "_ADDR", "_ADDRESS", "_URL")):
        return [address("127.0.0.1:" + value if value.isdecimal() else value)]
    return []


def probe_url(value, port):
    if value.startswith("/") and not value.startswith("//"):
        value = f"http://127.0.0.1:{port}" + value
    parsed = urllib.parse.urlsplit(value)
    if parsed.scheme != "http" or parsed.fragment or any(c in value for c in "\r\n"):
        raise ValueError("probe must be a local path or loopback HTTP URL")
    address(value)
    return value


def probe(url, timeout):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), run.NoRedirect())
    try:
        with opener.open(url, timeout=timeout) as response:
            return response.status
    except urllib.error.HTTPError as exc:
        code = exc.code
        exc.close()
        return code
    except (OSError, urllib.error.URLError, http.client.HTTPException) as exc:
        reason = getattr(exc, "reason", exc)
        return "refused" if getattr(reason, "errno", None) == errno.ECONNREFUSED else type(reason).__name__


def collect(home, args, config, variant, extra, row, golden=False):
    """One bounded lifecycle; every Hub/CLI launch uses Processes' assertions."""
    processes, peaks, stop = run.Processes(), {}, threading.Event()
    started = time.perf_counter()
    def observe():
        while not stop.is_set():
            try:
                for _, _, cpu, name in run.process_table():
                    peaks[name] = max(cpu, peaks.get(name, 0))
            except (OSError, subprocess.SubprocessError):
                pass
            stop.wait(config["process_interval_s"])
    def interrupted(signum, frame):
        raise TimeoutError() if signum == signal.SIGALRM else KeyboardInterrupt()
    handlers = {s: signal.signal(s, interrupted) for s in (signal.SIGINT, signal.SIGTERM, signal.SIGALRM)}
    signal.setitimer(signal.ITIMER_REAL, config["total_timeout_s"])
    observer = threading.Thread(target=observe, daemon=True)
    observer.start()
    try:
        row.update(status="running", load_average_before=run.load_average(), run_directory=home.name)
        for name in ("logs", "tmp"):
            (home / name).mkdir()
        env = run.isolated_environment(home, args.port) | extra
        if variant == "B" or not args.a_unset:
            env[args.var] = args.b if variant == "B" else args.a
        db = home / "data/steve.sqlite3"
        client = run.Client(f"http://127.0.0.1:{args.port}", config["request_timeout_s"])
        row["stage"] = "owner_token"
        token = run.access_token(processes, args.hub_bin, env, config["cli_timeout_s"]) if config["auth"] == "bearer" else None
        with (home / "logs/hub.log").open("wb") as log:
            before = time.perf_counter()
            hub = processes.start([str(args.hub_bin)], env, stdout=log, stderr=subprocess.STDOUT)
            row["health"] = run.health(client, hub, config["startup_timeout_s"])
            row["startup_to_health_ms"] = (time.perf_counter() - before) * 1000
            row["stage"] = "login"
            client.login(token)
            row["post_start"] = run.phase("post_start", config["settle_s"], hub, db, client, config, env, processes, args.hub_bin)
            if not golden:
                row["endpoints"], row["samples"] = {}, {"warm_ms": {}}
                for endpoint in config["endpoints"]:
                    path = endpoint["path"]
                    cold = client.request(path)
                    rows = [client.request(path) for _ in range(endpoint["requests"])]
                    row["samples"]["warm_ms"][path] = [r["ms"] for r in rows]
                    row["endpoints"][path] = {"cold": cold, "warm": run.requests_summary(rows)}
                for name in ("idle", "load"):
                    if name == "load":
                        row["stage"] = "write_smoke_and_warmup"
                        row["warmup"] = run.warmup(hub, db, client, config, env, processes, args.hub_bin)
                        if getattr(args, "scrape", None):
                            row["scrapes"] = {}
                            filename = f"round-{row['index']:02d}-{variant}-before.prom"
                            row["scrapes"]["before"] = telemetry.scrape(client, args.scrape, args.raw / filename)
                            if variant == "B" and row.get("check_scrape_auth"):
                                row["scrapes"]["unauthenticated"] = telemetry.scrape(client, args.scrape, None, False)
                    row["stage"] = name
                    row[name] = run.phase(name, config[name + "_s"], hub, db, client, config, env, processes,
                                          args.hub_bin, row["samples"], CpuMeter(hub.pid) if name == "load" else None)
                scrape_failed = False
                if getattr(args, "scrape", None):
                    filename = f"round-{row['index']:02d}-{variant}-after.prom"
                    row["scrapes"]["after"] = telemetry.scrape(client, args.scrape, args.raw / filename)
                    captures = row["scrapes"]
                    if variant == "B":
                        scrape_failed = any(captures[k]["status"] != 200 or not
                            captures[k].get("content_type", "").startswith("text/plain") for k in ("before", "after"))
                        scrape_failed |= captures.get("unauthenticated", {"status": 401})["status"] != 401
                        if not scrape_failed:
                            row["metrics_delta"] = telemetry.difference(
                                (args.raw / captures["before"]["file"]).read_text(),
                                (args.raw / captures["after"]["file"]).read_text(),
                                config["load_path"], "POST" if config["workload"] == "write" else "GET")
                            scrape_failed |= bool(row["metrics_delta"]["missing"])
                    else:
                        scrape_failed = any(captures[k]["status"] != "refused" for k in ("before", "after"))
                row["probes"] = {p: probe(probe_url(p, args.port), config["request_timeout_s"]) for p in args.probe}
                row["invalid_reasons"] = (["metrics_scrape_contract"] if scrape_failed else [])
                if row["load"]["errors"]:
                    row["invalid_reasons"].append("HTTP_error_or_timeout")
                failed = scrape_failed or any(e["cold"]["error"] or e["warm"]["errors"] for e in row["endpoints"].values())
                failed |= bool(row["load"]["errors"] or row["load"]["cli"]["errors"])
                failed |= len(row["load"]["cli"]["runs"]) != config["cli_runs"]
            else:
                failed = False
            failed |= any(row[n]["worker_errors"] or row[n]["lock"]["probe_errors"]
                          for n in (("post_start",) if golden else ("post_start", "idle", "load")))
            if hub.poll() is not None:
                raise RuntimeError("Hub exited")
            row["status"] = "measurement_errors" if failed else "ok"
    except (Exception, KeyboardInterrupt) as exc:
        row.update(status="failed", failure_type=type(exc).__name__)
        raise
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        for s in (signal.SIGINT, signal.SIGTERM):
            signal.signal(s, signal.SIG_IGN)
        try:
            stop.set()
            observer.join(timeout=6)
            row["cleanup"] = processes.cleanup()
            row["elapsed_s"] = time.perf_counter() - started
            row["load_average_after"] = run.load_average()
            row["cpu_top"] = [{"name": n, "cpu_percent": v} for n, v in
                              sorted(peaks.items(), key=lambda x: (-x[1], x[0]))[:config["cpu_top_n"]]]
            if row["cleanup"]["process_groups_remaining"]:
                raise RuntimeError("cleanup failed")
        finally:
            for s, handler in handlers.items():
                signal.signal(s, handler)


def aggregate(rows):
    metrics = {}
    for path in rows[0]["samples"]["warm_ms"]:
        pooled = [v for r in rows for v in r["samples"]["warm_ms"][path]]
        metrics.update({f"warm.{path}.{k}": v for k, v in percentiles(pooled).items()})
        metrics[f"warm.{path}.requests_per_s"] = len(pooled) * 1000 / sum(pooled) if sum(pooled) else None
    metrics.update({"load.read." + k: v for k, v in percentiles(
        [v for r in rows for v in r["samples"]["reads_ms"]]).items()})
    loads = [r["load"] for r in rows]
    seconds = sum(r["elapsed_s"] for r in loads)
    reads = sum(r["reads"]["latency_ms"]["count"] for r in loads)
    requests = sum(r["cpu"]["http_requests"] for r in loads)
    metrics["load.read_requests_per_s"] = reads / seconds
    cpu = [r["cpu"]["user_system_ns"] for r in loads]
    metrics["load.cpu_us_per_request"] = sum(cpu) / 1000 / requests if all(v is not None for v in cpu) and requests else None
    probes = sum(r["lock"]["probes"] for r in loads)
    metrics["load.lock_busy_ratio"] = sum(r["lock"]["busy_probes"] for r in loads) / probes if probes else None
    for field in ("device_bytes", "logical_bytes"):
        values = [r["disk"][field] for r in loads]
        metrics["load.disk_" + field] = sum(values) if all(v is not None for v in values) else None
    # Thresholds use the median of per-round measurements, never pooled samples.
    per_round = [{"load.requests_per_s": len(r["samples"].get("workload_ms", r["samples"]["reads_ms"])) / r["load"]["elapsed_s"],
                  **{"load." + k: v for k, v in percentiles(r["samples"].get("workload_ms", r["samples"]["reads_ms"])).items()}}
                 for r in rows]
    for key in PRIMARY:
        values = [r[key] for r in per_round]
        metrics[key] = statistics.median(values) if all(v is not None for v in values) else None
    return metrics


def relative(a, b):
    return None if a is None or b is None or a == 0 else (b - a) / a * 100


def verdict(interval, threshold):
    if interval is None:
        return "INCONCLUSIVE"
    lo, hi = interval
    return "PASS" if hi < threshold else "FAIL" if lo > threshold else "INCONCLUSIVE"


def change(key, a, b):
    value = relative(a, b)
    return -value if value is not None and key == "load.requests_per_s" else value


def median_verdict(overhead, interval, threshold):
    if overhead is None:
        return "INCONCLUSIVE"
    if overhead >= threshold:
        return "FAIL"
    return "PASS" if interval is not None and interval[1] < threshold else "PASS(median)/INCONCLUSIVE(CI)"


def bootstrap(a, b, threshold, replicates=REPLICATES, seed=SEED):
    rng, cache = random.Random(seed), {}
    left, right = aggregate(a), aggregate(b)
    draws = {key: [] for key in left}
    def resample(rows, label):
        indices = tuple(sorted(rng.randrange(len(rows)) for _ in rows))
        key = label, indices
        if key not in cache:
            cache[key] = aggregate([rows[i] for i in indices])
        return cache[key]
    for _ in range(replicates):
        av, bv = resample(a, "A"), resample(b, "B")
        for key in draws:
            value = change(key, av[key], bv[key])
            if value is not None:
                draws[key].append(value)
    result = {}
    for key, values in draws.items():
        values.sort()
        # Undefined zero/missing denominators must not become a selective CI.
        interval = [quantile(values, .025), quantile(values, .975)] if len(values) == replicates else None
        result[key] = {"a": left[key], "b": right[key], "overhead_pct": change(key, left[key], right[key]),
                       "ci95_pct": interval, "ci_width_pct_points": interval[1] - interval[0] if interval else None}
        if key in PRIMARY:
            result[key]["median_verdict"] = "PASS" if result[key]["overhead_pct"] is not None and result[key]["overhead_pct"] < threshold else "FAIL"
            result[key]["ci_verdict"] = verdict(interval, threshold)
            result[key]["verdict"] = median_verdict(result[key]["overhead_pct"], interval, threshold)
    return result


def summarize(result, args):
    sides = {v: [r for r in result["rounds"] if r["variant"] == v and "load" in r] for v in "AB"}
    if any(len(rows) != args.rounds for rows in sides.values()):
        result["overall"] = "INCONCLUSIVE"
        return
    result["variants"] = {v: aggregate(rows) for v, rows in sides.items()}
    result["comparison"] = bootstrap(sides["A"], sides["B"], args.threshold_pct)
    for path in args.probe:
        statuses = {v: dict(collections.Counter(str(r["probes"][path]) for r in rows)) for v, rows in sides.items()}
        result.setdefault("probe_summary", {})[path] = statuses
        if statuses["A"] == statuses["B"]:
            result["warnings"].append(f"PROBE IDENTICAL: {path}: A/B {statuses['A']}; switch effect is unproven")
    decisions = [result["comparison"][key]["verdict"] for key in PRIMARY]
    result["overall"] = "FAIL" if "FAIL" in decisions else "INCONCLUSIVE" if "INCONCLUSIVE" in decisions else "PASS(median)/INCONCLUSIVE(CI)" if "PASS(median)/INCONCLUSIVE(CI)" in decisions else "PASS"
    if any(r["status"] != "ok" for r in result["rounds"]) or result["status"] != "ok":
        result["overall"] = "INCONCLUSIVE"
    if "INCONCLUSIVE" in result["overall"]:
        result["warnings"].append("Noise/uncertainty exceeds the threshold: use more rounds or a quieter machine; inspect missing data/errors.")


def markdown(result):
    def cell(value):
        return str(value).replace("|", "\\|").replace("\n", " ")
    def number(v):
        return "n/a" if v is None else f"{v:.6g}"
    lines = [f"{result['overall']}: threshold {result['threshold_pct']}%; " +
             ", ".join(f"{k}={v.get('verdict')}" for k, v in result.get("comparison", {}).items() if k in PRIMARY), ""]
    lines += [f"**WARNING: {cell(w)}**\n" for w in result["warnings"]]
    lines += [f"Collection: {result['status']}; elapsed_s={result.get('elapsed_s')}; "
              f"failure_type={result.get('failure_type', 'none')}",
              f"Variable: {cell(result['variable'])}; A={cell(result['a'])} (None = unset); B={cell(result['b'])}",
              f"Binary SHA256: {result['binary']['sha256']}; snapshot bytes={result['snapshot']['size_bytes']}", ""]
    lines += [f"Order: {''.join(result['order'])}; whole-round bootstrap, seed {SEED}, {REPLICATES} replicates.",
              "Resample whole rounds independently to preserve within-round correlation and expose between-round noise.",
              "CPU/request includes all load reads+writes and background work; CLI is excluded. Disk is summed load bytes.",
              "", "Config: `" + json.dumps(result["config"], sort_keys=True) + "`", "",
              "| Metric | A (primary: round median) | B (primary: round median) | Overhead % | 95% CI % | Width pp | Verdict |",
              "| --- | ---: | ---: | ---: | --- | ---: | --- |"]
    for key, value in result.get("comparison", {}).items():
        ci = value["ci95_pct"]
        lines.append(f"| {cell(key)} | {number(value['a'])} | {number(value['b'])} | {number(value['overhead_pct'])} | "
                     f"{('[' + ', '.join(number(v) for v in ci) + ']') if ci else 'n/a'} | "
                     f"{number(value['ci_width_pct_points'])} | {value.get('verdict', '')} |")
    lines += ["", "| Round | Variant | Metric | p50 ms | p95 ms | p99 ms |", "| --- | --- | --- | ---: | ---: | ---: |"]
    for row in result["rounds"]:
        if "samples" in row and "reads_ms" in row["samples"]:
            for name, samples in {"load.workload": row["samples"].get("workload_ms", row["samples"]["reads_ms"]), **row["samples"]["warm_ms"]}.items():
                q = percentiles(samples)
                lines.append(f"| {row['index']} | {row['variant']} | {cell(name)} | " +
                             " | ".join(number(q[f"p{p}_ms"]) for p in (50, 95, 99)) + " |")
    for row in result["rounds"]:
        lines += ["", f"Round {row['index']} {row['variant']}: status={row['status']}; "
                  f"load before={row.get('load_average_before')}; after={row.get('load_average_after')}; "
                  f"cpu_top={cell(row.get('cpu_top'))}; probes={cell(row.get('probes'))}; "
                  f"HTTP errors={row.get('load', {}).get('errors')}; invalid={cell(row.get('invalid_reasons'))}; "
                  f"stage={row.get('stage')}; failure_type={row.get('failure_type')}; "
                  f"scrapes={cell(row.get('scrapes'))}", ""]
    return "\n".join(lines) + "\n"


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for flag in ("hub-bin", "snapshot", "work", "out"):
        parser.add_argument("--" + flag, type=Path, required=True)
    for flag in ("var", "b"):
        parser.add_argument("--" + flag, required=True)
    parser.add_argument("--a", default="")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--smoke", action="store_true", help="allow one round per side; no acceptance conclusion")
    parser.add_argument("--workload", choices=("read", "write"))
    parser.add_argument("--scrape", help="owner-authenticated loopback metrics URL (B only)")
    parser.add_argument("--a-unset", action="store_true")
    parser.add_argument("--config", type=Path)
    parser.add_argument("--md", type=Path)
    parser.add_argument("--probe", action="extend", nargs="+", default=[])
    parser.add_argument("--env", action="append", default=[])
    parser.add_argument("--threshold-pct", type=float, default=1)
    args = parser.parse_args(argv)
    os.umask(0o077)
    args.probe = list(dict.fromkeys(args.probe))
    if args.config:
        args.config = args.config.expanduser().resolve()
    config = run.configuration(args.config, args.workload)
    args.work = run.safe_work(args.work)
    args.hub_bin, args.snapshot = (p.expanduser().resolve() for p in (args.hub_bin, args.snapshot))
    if args.rounds < (1 if args.smoke else 3) or not math.isfinite(args.threshold_pct) or args.threshold_pct <= 0:
        parser.error("need >=3 rounds per variant (or --smoke) and a finite positive threshold")
    try:
        if args.scrape:
            args.scrape = probe_url(args.scrape, args.port)
            if config["auth"] != "bearer":
                raise ValueError("--scrape requires config auth=bearer")
        extra, ports = environment_options(args.env, args.var)
        # Validate variable values for listeners/ports as well as shared --env.
        for value in (args.a, args.b):
            ports.extend(environment_ports(args.var, value))
        if not 1024 <= args.port <= 65532 or 8440 in (args.port, args.port + 3):
            raise ValueError("unsafe Hub/Node port")
        ports += [("127.0.0.1", args.port), ("127.0.0.1", args.port + 3)]
        for p in args.probe:
            probe_url(p, args.port)
        for host, port in set(ports):
            with socket.socket(socket.AF_INET6 if ":" in host else socket.AF_INET) as sock:
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                sock.bind((host, port))
        for flag in ("out", "md"):
            path = getattr(args, flag)
            if path is not None:
                path = run.safe_work(path)
                setattr(args, flag, path)
                if path in (args.snapshot, args.hub_bin, args.config) or path.is_relative_to(Path(__file__).resolve().parent):
                    raise ValueError("output must not overwrite inputs or tool directory")
        if args.out == args.md:
            raise ValueError("JSON and Markdown need different paths")
        if not args.hub_bin.is_file() or not os.access(args.hub_bin, os.X_OK) or not args.snapshot.is_file():
            raise ValueError("need executable Hub and static snapshot")
        if any(p.exists() and p.stat().st_size for p in
               (Path(str(args.snapshot) + suffix) for suffix in ("-wal", "-journal"))):
            raise ValueError("snapshot must have no active WAL/journal")
    except (ValueError, OSError) as exc:
        parser.error(str(exc))
    with args.hub_bin.open("rb") as stream:
        sha = hashlib.file_digest(stream, "sha256").hexdigest()
    result = {"schema_version": 1, "mode": "environment_ab", "status": "running", "overall": "INCONCLUSIVE",
              "binary": {"name": args.hub_bin.name, "sha256": sha}, "host": run.host_info(),
              "snapshot": {"name": args.snapshot.name, "size_bytes": args.snapshot.stat().st_size},
              "config": config, "variable": args.var, "a": None if args.a_unset else args.a, "b": args.b,
              "env": extra, "threshold_pct": args.threshold_pct, "order": order(args.rounds),
              "bootstrap": {"unit": "round", "seed": SEED, "replicates": REPLICATES, "confidence": .95},
              "warnings": [], "rounds": [], "started_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())}
    started = time.perf_counter()
    rc, session = 1, None
    try:
        args.work.mkdir(parents=True, exist_ok=True)
        session = Path(tempfile.mkdtemp(prefix="ab-", dir=args.work))
        result["run_directory"] = session.name
        if args.scrape:
            args.out.parent.mkdir(parents=True, exist_ok=True)
            args.raw = Path(tempfile.mkdtemp(prefix=args.out.stem + "-raw-", dir=args.out.parent))
            result["raw_directory"] = args.raw.name
        golden = session / "golden"
        (golden / "data").mkdir(parents=True)
        db = golden / "data/steve.sqlite3"
        result["snapshot_copy_method"] = clone(args.snapshot, db)
        with closing(sqlite3.connect(db)) as conn:
            count = conn.execute("UPDATE users SET subject='dev:local' WHERE id=(SELECT id FROM users WHERE role='owner' ORDER BY created,id LIMIT 1)").rowcount
            if count != 1:
                raise RuntimeError("snapshot must contain one owner")
            conn.commit()
        result["golden"] = {}
        collect(golden, args, config, "A", extra, result["golden"], golden=True)
        if result["golden"]["status"] != "ok":
            raise RuntimeError("golden preparation failed")
        with closing(sqlite3.connect(db)) as conn:
            if conn.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()[0] != 0:
                raise RuntimeError("golden checkpoint busy")
        with db.open("rb") as stream:
            result["golden"]["db_sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
        for index, variant in enumerate(result["order"], 1):
            home = session / f"round-{index:02d}-{variant}"
            home.mkdir(mode=0o700)
            row = {"index": index, "variant": variant, "status": "copying",
                   "check_scrape_auth": variant == "B" and not any(r["variant"] == "B" for r in result["rounds"])}
            result["rounds"].append(row)
            row["copy_method"] = clone(golden / "data", home / "data")
            collect(home, args, config, variant, extra, row)
            row["metrics"] = aggregate([row])
            run.write_result(args.out, result)
            print(json.dumps({"round": index, "variant": variant, "status": row["status"], "elapsed_s": row["elapsed_s"]}), flush=True)
        result["status"] = "ok" if all(r["status"] == "ok" for r in result["rounds"]) else "measurement_errors"
        rc = 0 if result["status"] == "ok" else 2
        summarize(result, args)
        if args.smoke:
            result["overall"] = "INCONCLUSIVE(smoke)"
            result["warnings"].append("Short smoke run: not a 3+3 performance acceptance.")
    except (Exception, KeyboardInterrupt) as exc:
        result.update(status="failed", failure_type=type(exc).__name__, overall="INCONCLUSIVE")
        rc = 1
    finally:
        if session is not None:
            cleaned = []
            for item in [result.get("golden", {}), *result["rounds"]]:
                if item.get("cleanup", {}).get("process_groups_remaining") == []:
                    home = session / item.get("run_directory", "golden")
                    if (home / "data").exists():
                        shutil.rmtree(home / "data")
                        cleaned.append(home.name)
            result["removed_data_copies"] = cleaned
        result["elapsed_s"] = time.perf_counter() - started
        result["finished_at"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())
        run.write_result(args.out, result)
        if args.md:
            args.md.parent.mkdir(parents=True, exist_ok=True)
            args.md.write_text(markdown(result))
    print(json.dumps({k: result[k] for k in ("status", "overall", "elapsed_s")}))
    return rc


if __name__ == "__main__":
    sys.exit(main())
