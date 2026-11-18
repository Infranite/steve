#!/usr/bin/env python3
"""Rehearse a Hub upgrade on a fresh, inert snapshot copy (Python 3.11+, stdlib).

--work is the new STEVE_HOME, never an existing installation. Outputs contain no
response bodies or cookies; raw logs and the copied database remain private in
--work. Refuse nonempty WAL/journal sidecars: take a consistent snapshot first.

--endpoints accepts a JSON list of GET paths, or an object with `endpoints` and
`expected_tasks`. Tasks are objects: {"name": "outbox", "log_pattern":
"outbox dispatcher started"}, or {"name": "node-off", "health_path":
"node.enabled", "equals": false}. `expected_tasks` must be nonempty if supplied.
--expect-tasks accepts the same task list separately. --baseline accepts an older
report.json; its observed task names are compared with the current observations.
Explicit task expectations are strict; the built-in source inventory warns when
silent tasks cannot be observed. Observation proves activity, not continued health.

Source contracts checked: steve-store/src/db.rs (schema_version.v, migration log),
steve-hub/src/{main,extension,app,jobs}.rs, wire/{core,modules,mod}.rs,
steve-auth/src/lib.rs, steve-channels/src/{api,personas_api,channels_api}.rs,
steve-modules/src/accounts/connections/api.rs, steve-hub/src/api/{mod,stats,attention}.rs.
The Hub uses tracing text on stdout; we redirect both streams into logs/start-N.log.
"""

import argparse
import hashlib
import http.cookiejar
import json
import math
import os
from pathlib import Path
import re
import signal
import socket
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from contextlib import closing
from datetime import datetime, timezone


ENDPOINTS = ["/healthz", "/api/state", "/api/me", "/api/hub-metadata",
             "/api/personas", "/api/spaces", "/api/channels", "/api/stats/summary",
             "/api/devices", "/api/connections", "/api/me/attention"]
# These tasks are wired even on NO_RESUME copies. Most emit only on work/failure.
# Keep the source inventory separate from observed evidence: quiet != healthy.
TASK_NAMES = """register_hub release_watcher housekeeping workspace_watch stall_watch
judge_watch backfill_kinds lifecycle_tick presence_sweep runtimes_warmup yolo_events
yolo_ticks mcp_idle_sweep node_vitals stats_refresher outbox_dispatcher notifications
trigger_report scheduler alarm_clock digest_bot signal_poller schedule_retention
channel_retention plugin_supervisor calendar_poll mailbox_watch memory_event_sync
github_poll github_effects mail_poll mail_send_queue directory_sync directory_watch
channel_send_queue vault_token_refresh vault_health knowledge_index memory_sweeper
knowledge_scans candidate_expirer chat_documents digest_intake signing_migration
feishu_registration_resume connection_runtime""".split()
TASK_MARKERS = {
    "outbox_dispatcher": r"outbox dispatcher started",
    "housekeeping": r"housekeeping|compacted an archived session",
    "lifecycle_tick": r"lifecycle tick",
    "node_vitals": r"(?:vitals:|keeping vitals:)",
    "stats_refresher": r"stats refresh",
    "knowledge_index": r"kb\.index:",
    "memory_sweeper": r"kb\.sync_mems:",
    "memory_event_sync": r"kb sync:",
    "feishu_registration_resume": r"feishu app registrations resumed:",
}
DISABLED = ["local_node", "remote_node_listener", "daily_backup", "adapter_updates",
            "release_update_check", "price_catalog_sync", "session_resume"]
ANSI = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
MIGRATED = re.compile(r"migrated database.*?\bfrom=(\d+).*?\bto=(\d+)")


def elapsed(start):
    return round(time.monotonic() - start, 4)


def scrub(value):
    """Sanitize report strings, including paths inside third-party log messages."""
    if isinstance(value, dict):
        return {scrub(str(k)): scrub(v) for k, v in value.items()}
    if isinstance(value, list):
        return [scrub(v) for v in value]
    if isinstance(value, str):
        value = value.replace(str(Path.home()), "<user-home>")
        value = re.sub(r"/(?:Users|home|private|tmp|var|Volumes|opt|srv|mnt|root)/[^\s\"'<>]*",
                       "<path>", value)
        value = re.sub(r"(?<![\w:/])/(?!api(?:/|$)|auth(?:/|$)|healthz\b)[^\s\"'<>,;)}]+",
                       "<path>", value)
        return value.replace(socket.gethostname(), "<host>")
    return value


def cell(value):
    if isinstance(value, (dict, list)):
        value = json.dumps(value, ensure_ascii=False, sort_keys=True)
    return str(scrub(value)).replace("|", "\\|").replace("\n", "<br>")


def table(headers, rows):
    return "\n".join(["| " + " | ".join(headers) + " |",
                      "| " + " | ".join(["---"] * len(headers)) + " |"] +
                     ["| " + " | ".join(cell(v) for v in row) + " |" for row in rows])


def check(report, name, status, evidence):
    report["checks"].append({"name": name, "status": status, "evidence": evidence})


def read_json(path):
    return json.loads(Path(path).read_text())


def safe_work(work, port):
    work = work.expanduser().resolve()
    try:
        relative = work.relative_to(Path.home().resolve())
    except ValueError:
        relative = None
    if (work == Path.home().resolve() or work == Path("/") or
            (relative and relative.parts[0].startswith(".steve"))):
        raise ValueError("refusing a production/default home or its descendant")
    if not 1024 <= port <= 65532 or 8440 in (port, port + 3):
        raise ValueError("unsafe Hub/local Node port")
    for candidate in (port, port + 3):
        with socket.socket() as sock:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            sock.bind(("127.0.0.1", candidate))
    return work


def environment(work, port):
    # Replacement env, equivalent to env -i. No inherited service credentials.
    work = work.resolve()
    sandbox = work / "sandbox"
    sandbox.mkdir(exist_ok=True)
    return dict(HOME=str(sandbox), USER="rehearsal", PATH="/usr/bin:/bin:/usr/sbin:/sbin",
                TMPDIR=str(sandbox), STEVE_HOME=str(work), STEVE_HOST="127.0.0.1",
                STEVE_PORT=str(port), STEVE_LOCAL_NODE_PORT=str(port + 3),
                STEVE_PUBLIC_URL=f"http://127.0.0.1:{port}", STEVE_DEV_LOGIN="1",
                STEVE_NO_RESUME="1", STEVE_NODE_MODE="off", STEVE_NODE_LISTEN="0",
                STEVE_VAULT_KEYSTORE="file", STEVE_BACKUP="off",
                STEVE_ADAPTER_UPDATES="off", STEVE_UPDATE_CHECK="0",
                STEVE_PRICE_CATALOG_URL="off", STEVE_ADAPTER_CLAUDE="/usr/bin/false",
                STEVE_ADAPTER_CODEX="/usr/bin/false", STEVE_ADAPTER_PI="/usr/bin/false",
                STEVE_WEB_DIR=str(sandbox / "web"), STEVE_LOG="info", NO_COLOR="1")


def db_connect(path):
    return sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True, timeout=10)


def database_state(path, integrity=False):
    with closing(db_connect(path)) as db:
        db.execute("BEGIN")  # all counts belong to one read snapshot
        names = [r[0] for r in db.execute(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")]
        version = db.execute("SELECT v FROM schema_version LIMIT 1").fetchone() if "schema_version" in names else None
        result = {"schema": version[0] if version else 0, "tables": {}}
        for name in names:
            quoted = '"' + name.replace('"', '""') + '"'
            result["tables"][name] = db.execute(f"SELECT count(*) FROM {quoted}").fetchone()[0]
        if integrity:
            result["quick_check"] = [r[0] for r in db.execute("PRAGMA quick_check")]
            cursor = db.execute("PRAGMA foreign_key_check")
            result["foreign_key_count"] = 0
            result["foreign_key_examples"] = []
            for row in cursor:
                result["foreign_key_count"] += 1
                if len(result["foreign_key_examples"]) < 20:
                    result["foreign_key_examples"].append(list(row))
        return result


def prepare(snapshot, work):
    if not snapshot.is_file():
        raise ValueError("snapshot must be a regular SQLite file")
    sidecars = [Path(str(snapshot) + suffix) for suffix in ("-wal", "-journal")]
    def sidecar_state():
        return [(p.stat().st_size, p.stat().st_mtime_ns) if p.exists() else None for p in sidecars]
    side_before = sidecar_state()
    if any(state and state[0] for state in side_before):
        raise ValueError("snapshot has nonempty WAL/journal; provide a quiescent snapshot")
    (work / "data").mkdir()
    target = work / "data/steve.sqlite3"
    stamp = snapshot.stat()
    digest = hashlib.sha256()
    with snapshot.open("rb") as source, target.open("xb") as destination:
        for chunk in iter(lambda: source.read(4 << 20), b""):
            digest.update(chunk)
            destination.write(chunk)
    after = snapshot.stat()
    if ((stamp.st_size, stamp.st_mtime_ns, stamp.st_ino) != (after.st_size, after.st_mtime_ns, after.st_ino)
            or side_before != sidecar_state()):
        raise ValueError("snapshot changed during copy")
    # Mirror testhub.sh: use the oldest owner; never touch any other subject.
    with closing(sqlite3.connect(target)) as db, db:
        updated = db.execute("UPDATE users SET subject='dev:local' WHERE id=(SELECT id FROM users WHERE role='owner' ORDER BY created LIMIT 1)").rowcount
        if updated != 1:
            raise ValueError("snapshot must contain an owner")
    return target, {"bytes": stamp.st_size, "sha256": digest.hexdigest(), "owner_subject": "dev:local"}


class Process:
    def __init__(self, argv, env, log, cwd):
        # Every binary invocation, including --build-info, has the same boundary.
        # Unknown CLI arguments start a Hub: never rely on a flag being read-only.
        home = Path(env.get("STEVE_HOME", "")).resolve()
        assert env.get("STEVE_HOME") and home == cwd.resolve()
        assert home != Path.home().resolve() and home != Path("/")
        assert not (home.is_relative_to(Path.home().resolve()) and
                    home.relative_to(Path.home().resolve()).parts[0].startswith(".steve"))
        assert env.get("STEVE_PORT") and int(env["STEVE_PORT"]) != 8440
        assert env.get("HOME") == str(home / "sandbox")
        assert env.get("STEVE_NO_RESUME") == "1" and env.get("STEVE_BACKUP") == "off"
        assert not any(k in env for k in ("STEVE_MANAGED", "STEVE_ENV", "AWS_SECRET_ACCESS_KEY"))
        self.stream = log.open("wb")
        try:
            self.p = subprocess.Popen(argv, env=env, cwd=cwd, stdout=self.stream,
                                      stderr=subprocess.STDOUT, start_new_session=True)
        except BaseException:
            self.stream.close()
            raise

    def group_exists(self):
        try:
            os.killpg(self.p.pid, 0)
            return True
        except ProcessLookupError:
            return False

    def send(self, sig):
        try:
            os.killpg(self.p.pid, sig)
        except ProcessLookupError:
            pass

    def stop(self, timeout):
        start = time.monotonic()
        before = self.p.poll()
        self.send(signal.SIGTERM)
        forced = False
        try:
            self.p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            forced = True
            self.send(signal.SIGKILL)
            self.p.wait(timeout=5)
        # A child may survive the leader's graceful exit. Own the entire group.
        self.send(signal.SIGTERM)
        deadline = time.monotonic() + 2
        while self.group_exists() and time.monotonic() < deadline:
            time.sleep(.05)
        if self.group_exists():
            forced = True
            self.send(signal.SIGKILL)
            deadline = time.monotonic() + 2
            while self.group_exists() and time.monotonic() < deadline:
                time.sleep(.05)
        self.stream.close()
        return {"seconds": elapsed(start), "exit_code": self.p.returncode,
                "already_exited": before is not None, "forced": forced,
                "process_group_gone": not self.group_exists()}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None  # never follow an endpoint redirect and mistake HTML 200 for API success


def request(opener, base, path, timeout):
    start = time.monotonic()
    body = b""
    result = {"path": path, "status": None, "bytes": 0}
    try:
        try:
            response = opener.open(base + path, timeout=timeout)
        except urllib.error.HTTPError as exc:
            response = exc
        with response:
            result["status"] = response.status
            body = response.read()
            result["bytes"] = len(body)
    except (OSError, urllib.error.URLError, ValueError) as exc:
        result["error"] = str(exc)
    result["seconds"] = elapsed(start)
    return result, body


def log_summary(path):
    levels = {level: {"count": 0, "examples": []} for level in ("ERROR", "WARN", "panic")}
    migrations, lines = [], []
    with path.open(errors="replace") as stream:
        for raw in stream:
            line = ANSI.sub("", raw).rstrip()
            lines.append(line)
            match = MIGRATED.search(line)
            if match:
                migrations.append({"from": int(match[1]), "to": int(match[2])})
            for level, data in levels.items():
                hit = (re.search(r"\b(?:panicked|panic)(?:\b|!)", line, re.I) if level == "panic"
                       else re.search(rf"(?:^|\s){level}(?:\s|:)", line))
                if hit:
                    data["count"] += 1
                    # Drop only timestamp for deduplication; retain original evidence text.
                    unique = re.sub(r"^\S*T\S+\s+", "", line)
                    sample = unique[:1000]
                    limit = 5 if level == "WARN" else 30
                    if sample not in data["examples"] and len(data["examples"]) < limit:
                        data["examples"].append(sample)
    return {"levels": levels, "migrations": migrations}, "\n".join(lines)


def task_specs(config, path):
    explicit = path is not None or "expected_tasks" in config
    specs = read_json(path) if path else config.get("expected_tasks")
    if not explicit:
        specs = [{"name": name, "log_pattern": TASK_MARKERS.get(name)} for name in TASK_NAMES]
    if not isinstance(specs, list) or not specs:
        raise ValueError("expected tasks must be a nonempty JSON array")
    seen = set()
    for spec in specs:
        if not isinstance(spec, dict) or not isinstance(spec.get("name"), str) or spec["name"] in seen:
            raise ValueError("tasks require unique string names")
        seen.add(spec["name"])
        pattern, health = spec.get("log_pattern"), spec.get("health_path")
        if explicit and not (pattern or health):
            raise ValueError("explicit task requires log_pattern or health_path")
        if pattern:
            re.compile(pattern)
        if health and (not isinstance(health, str) or "equals" not in spec):
            raise ValueError("health_path requires a string path and equals")
    return specs, explicit


def observe_tasks(specs, logs, health):
    observed, missing = [], []
    for spec in specs:
        evidence = []
        if spec.get("log_pattern"):
            matches = re.findall(spec["log_pattern"], logs, re.I | re.M)
            if matches:
                evidence.append({"method": "log", "pattern": spec["log_pattern"], "matches": len(matches)})
        if spec.get("health_path"):
            value = health
            for part in spec["health_path"].split("."):
                value = value.get(part) if isinstance(value, dict) else None
            if value == spec["equals"] and type(value) is type(spec["equals"]):
                evidence.append({"method": "healthz", "field": spec["health_path"], "value": value})
        if evidence:
            observed.append({"name": spec["name"], "evidence": evidence})
        else:
            missing.append(spec["name"])
    return {"observed": observed, "unobserved": missing, "disabled": DISABLED}


def startup(binary, env, work, index, base, timeout, endpoints, observe_seconds, report):
    log = work / f"logs/start-{index}.log"
    run = {"index": index, "healthy": False, "endpoints": [], "log_file": f"logs/start-{index}.log"}
    report["starts"].append(run)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect(),
                                        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    start = time.monotonic()
    process = None
    health = {}
    try:
        process = Process([str(binary)], env, log, work)
        deadline = start + timeout
        while time.monotonic() < deadline:
            if process.p.poll() is not None:
                raise RuntimeError("Hub exited before healthz")
            probe, body = request(opener, base, "/healthz", min(1, max(.01, deadline - time.monotonic())))
            try:
                health = json.loads(body)
            except (ValueError, UnicodeError):
                health = {}
            if probe["status"] == 200 and isinstance(health, dict) and health.get("ok") is True:
                run["healthy"] = True
                break
            time.sleep(min(.1, max(0, deadline - time.monotonic())))
        run["startup_seconds"] = elapsed(start)
        if not run["healthy"]:
            raise TimeoutError("healthz deadline exceeded")
        # Store only public build identity, never private health payloads.
        run["health"] = {k: health.get(k) for k in ("ok", "version", "build_commit")}
        if index == 1:
            report["after"] = database_state(work / "data/steve.sqlite3", integrity=True)
            login, _ = request(opener, base, "/auth/dev", timeout)
            run["login"] = login
            check(report, "dev_login", "PASS" if login["status"] in (302, 303) else "FAIL", login)
            for path in endpoints:
                result, _ = request(opener, base, path, timeout)
                run["endpoints"].append(result)
        time.sleep(observe_seconds)
        if process.p.poll() is not None:
            raise RuntimeError("Hub exited during observation")
    except Exception as exc:
        run["error"] = f"{type(exc).__name__}: {exc}"
    finally:
        run.setdefault("startup_seconds", elapsed(start))
        run["shutdown"] = process.stop(min(timeout, 30)) if process else {
            "exit_code": None, "forced": False, "already_exited": True, "process_group_gone": True, "seconds": 0}
        if not log.exists():
            log.touch()
        run["logs"], logs = log_summary(log)
        run["tasks"] = observe_tasks(report["task_expectations"], logs, health)
    return run


def summarize(report, strict, baseline):
    for run in report["starts"]:
        prefix = f"start_{run['index']}"
        check(report, prefix, "PASS" if run["healthy"] and "error" not in run else "FAIL",
              {k: run[k] for k in ("healthy", "startup_seconds", "error") if k in run})
        shutdown = run["shutdown"]
        check(report, prefix + "_shutdown", "PASS" if shutdown["exit_code"] == 0 and not shutdown["forced"]
              and not shutdown["already_exited"] and shutdown["process_group_gone"] else "FAIL", shutdown)
        levels = run["logs"]["levels"]
        status = "FAIL" if levels["ERROR"]["count"] or levels["panic"]["count"] else (
            "WARN" if levels["WARN"]["count"] else "PASS")
        check(report, prefix + "_logs", status, levels)
        tasks = run["tasks"]
        check(report, prefix + "_tasks", ("FAIL" if strict else "WARN") if tasks["unobserved"] else "PASS", tasks)
    first = report["starts"][0] if report["starts"] else {}
    endpoints = first.get("endpoints", [])
    check(report, "api_200", "PASS" if endpoints and all(r["status"] == 200 for r in endpoints) else "FAIL", endpoints)
    before, after = report.get("before", {}), report.get("after", {})
    target = report.get("build", {}).get("schema")
    if target is None:
        migrations = (first.get("logs", {}).get("migrations", []) or
                      report.get("build_probe", {}).get("logs", {}).get("migrations", []))
        target = migrations[-1]["to"] if migrations else None
        report["schema_target_source"] = "migration log" if target is not None else "unavailable"
    else:
        report["schema_target_source"] = "binary --build-info"
    report["schema_target"] = target
    check(report, "schema_target", "PASS" if target is not None and after.get("schema") == target else "FAIL",
          {"before": before.get("schema"), "after": after.get("schema"), "expected": target,
           "source": report["schema_target_source"]})
    check(report, "quick_check", "PASS" if after.get("quick_check") == ["ok"] else "FAIL", after.get("quick_check"))
    check(report, "foreign_key_check", "PASS" if after.get("foreign_key_count") == 0 else "FAIL",
          {k: after.get(k) for k in ("foreign_key_count", "foreign_key_examples")})
    a, b = before.get("tables", {}), after.get("tables", {})
    delta = {name: {"before": a.get(name), "after": b.get(name), "delta": b.get(name, 0) - a.get(name, 0)}
             for name in sorted(a.keys() | b.keys()) if a.get(name) != b.get(name)}
    report["table_changes"] = {"added": sorted(b.keys() - a.keys()), "removed": sorted(a.keys() - b.keys()), "rows": delta}
    check(report, "table_changes", "FAIL" if not before or not after else (
        "WARN" if any(r["delta"] < 0 for r in delta.values()) else "PASS"), report["table_changes"])
    second = report["starts"][1] if len(report["starts"]) > 1 else {}
    unchanged = second.get("healthy") and not second.get("logs", {}).get("migrations", []) and report.get("restart_schema") == after.get("schema")
    check(report, "restart_without_migration", "PASS" if unchanged else "FAIL",
          {"schema": report.get("restart_schema"), "migrations": second.get("logs", {}).get("migrations", [])})
    if baseline:
        previous = read_json(baseline)
        old = {t["name"] for t in previous["starts"][0]["tasks"]["observed"]}
        new = {t["name"] for t in first.get("tasks", {}).get("observed", [])}
        check(report, "baseline_tasks", "PASS" if old == new else "FAIL",
              {"added": sorted(new - old), "missing": sorted(old - new)})


def render(report):
    def evidence(c):
        name, e = c["name"], c["evidence"]
        if name.endswith("_logs"):
            return "; ".join(f"{k}: {v['count']}" for k, v in e.items()) + "；原文见下方"
        if name.endswith("_tasks") and "observed" in e:
            return f"观察到 {len(e['observed'])} 项，未观察到 {len(e['unobserved'])} 项；清单见下方"
        if name == "api_200":
            return f"{sum(r['status'] == 200 for r in e)}/{len(e)} 返回 200；耗时和字节数见下方"
        if name == "table_changes":
            drops = {n: d["delta"] for n, d in e["rows"].items() if d["delta"] < 0}
            return f"新增 {e['added']}；移除 {e['removed']}；行数下降 {drops}"
        return e
    lines = [report["verdict"], "", f"Label: {cell(report['label'])}; warnings: {report['warning_count']}", "",
             table(["检查项", "结论", "证据"], [(c["name"], c["status"], evidence(c)) for c in report["checks"]]), ""]
    for run in report["starts"]:
        lines += [f"### 第 {run['index']} 次启动", "",
                  table(["接口", "HTTP", "耗时 (s)", "字节"],
                        [(r["path"], r["status"], r["seconds"], r["bytes"]) for r in run["endpoints"]]), ""]
        lines += ["日志证据：", ""]
        for level, data in run["logs"]["levels"].items():
            lines += [f"- {level}: {data['count']}"] + [f"  - {cell(s)}" for s in data["examples"]]
        lines += ["", "后台任务观察：", "", table(["任务", "证据"],
                  [(t["name"], t["evidence"]) for t in run["tasks"]["observed"]]), "",
                  "未观察到（静默或尚未到触发时刻）：" + ", ".join(run["tasks"]["unobserved"]), "",
                  "隔离模式关闭：" + ", ".join(run["tasks"]["disabled"]), ""]
    lines += ["构建证据：" + cell(report.get("build", {})), "",
              "构建 SHA256：" + str(report.get("binary_sha256", "unavailable")), ""]
    lines += ["### 迁移后表与行数", "", table(["表", "升级前", "升级后", "变化"],
              [(name, report.get("before", {}).get("tables", {}).get(name, "—"), count,
                count - report.get("before", {}).get("tables", {}).get(name, 0))
               for name, count in report.get("after", {}).get("tables", {}).items()]), "",
              "### 观察方法与局限", ""] + ["- " + note for note in report["limitations"]]
    return "\n".join(lines) + "\n"


def run(args):
    specs_config = read_json(args.endpoints) if args.endpoints else {}
    config = {"endpoints": specs_config} if isinstance(specs_config, list) else specs_config
    if not isinstance(config, dict):
        raise ValueError("endpoint configuration must be an object or list")
    endpoints = config.get("endpoints", ENDPOINTS)
    if not isinstance(endpoints, list) or not endpoints or not all(isinstance(p, str) and
            p.startswith(("/api/", "/healthz")) and not urllib.parse.urlsplit(p).netloc and
            "#" not in p and not any(c.isspace() for c in p) for p in endpoints):
        raise ValueError("endpoints must be a nonempty list of local GET API paths")
    specs, strict = task_specs(config, args.expect_tasks)
    work = safe_work(args.work, args.port)
    out = args.out_dir.expanduser().resolve()
    binary = args.hub_bin.expanduser().resolve(strict=True)
    snapshot = args.snapshot.expanduser().resolve(strict=True)
    if out == work or work in out.parents or out in work.parents or snapshot == out or out in snapshot.parents:
        raise ValueError("work, output and snapshot locations must be separate")
    if out.exists() and any(out.iterdir()):
        raise ValueError("output directory must be new or empty")
    # Exclusive creation: a repeat uses a new --work, never deletes/overwrites data.
    work.mkdir(parents=True, exist_ok=False, mode=0o700)
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    (work / "logs").mkdir()
    env = environment(work, args.port)
    report = {"format": 1, "label": args.label or "rehearsal", "created_utc": datetime.now(timezone.utc).isoformat(),
              "settings": {"port": args.port, "local_node_port": args.port + 3, "timeout_seconds": args.timeout,
                           "observe_seconds": args.observe_seconds, "strict_task_expectations": strict},
              "checks": [], "starts": [], "task_expectations": specs, "limitations": [
                  "NO_RESUME 隔离副本；Node、恢复会话、备份、更新和价格同步关闭；不验证正常生产模式的任务。",
                  "后台任务以源码接线清单为预期；只把日志匹配或显式 healthz 字段匹配列为观察证据。",
                  "静默任务未观察到记 WARN；自定义 expected_tasks/--expect-tasks 或 --baseline 比对失败记 FAIL。",
                  f"每次健康后额外观察 {args.observe_seconds}s；短窗口不能证明周期任务持续运行或业务正确。",
                  "表计数来自一致读事务，但包括启动后台任务的写入，不能把全部变化归因于迁移。",
                  "日志覆盖启动、接口请求及停止；示例去重、每条最多 1000 字，ERROR/panic 各最多 30 条，WARN 最多 5 条。",
                  "只报告响应大小，不保存业务响应或登录 cookie。原始日志留在私有 work/logs/。",
                  "重启无迁移以迁移日志为空及 schema 未变化为证；不证明没有其他幂等 SQL 写入。"]}
    try:
        with binary.open("rb") as stream:
            report["binary_sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
        db, report["snapshot"] = prepare(snapshot, work)
        report["before"] = database_state(db)
        probe = Process([str(binary), "--build-info"], env, work / "logs/build-info.log", work)
        try:
            probe.p.wait(timeout=min(args.timeout, 10))
        except subprocess.TimeoutExpired:
            report["build_info_error"] = "--build-info timed out; migration log fallback required"
        finally:
            stopped = probe.stop(1)
        probe_logs, _ = log_summary(work / "logs/build-info.log")
        report["build_probe"] = {"shutdown": stopped, "logs": probe_logs}
        try:
            report["build"] = json.loads((work / "logs/build-info.log").read_text())
            if not isinstance(report["build"], dict):
                raise ValueError("build-info is not an object")
        except (ValueError, UnicodeError):
            report["build"] = {}
        levels = probe_logs["levels"]
        check(report, "build_info_probe", "FAIL" if levels["ERROR"]["count"] or levels["panic"]["count"]
              or stopped["forced"] or not stopped["process_group_gone"] else (
                  "PASS" if report["build"].get("schema") is not None else "WARN"), report["build_probe"])
        if not report["build"].get("schema"):
            report["limitations"].append("构建未提供 --build-info schema；探测可能启动过 Hub，已停止整个进程组；目标版本仅能从迁移日志推断。")
        base = f"http://127.0.0.1:{args.port}"
        startup(binary, env, work, 1, base, args.timeout, endpoints, args.observe_seconds, report)
        if report["starts"][0]["healthy"]:
            startup(binary, env, work, 2, base, args.timeout, [], args.observe_seconds, report)
            report["restart_schema"] = database_state(db)["schema"]
    except (Exception, KeyboardInterrupt) as exc:
        check(report, "execution", "FAIL", f"{type(exc).__name__}: {exc}")
    try:
        summarize(report, strict, args.baseline)
    except Exception as exc:
        check(report, "summary", "FAIL", str(exc))
    report["verdict"] = "FAIL" if any(c["status"] == "FAIL" for c in report["checks"]) else "PASS"
    report["warning_count"] = sum(c["status"] == "WARN" for c in report["checks"])
    report = scrub(report)
    (out / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    (out / "report.md").write_text(render(report))
    print(f"{report['verdict']} ({report['warning_count']} warnings): report.json, report.md")
    return 1 if report["verdict"] == "FAIL" else 0


def compare(a, b):
    rows = []
    def add(kind, name, left, right):
        rows.append((kind, name, left, right, "=" if left == right else "DIFF"))
    add("结论", "verdict", a["verdict"], b["verdict"])
    for key in ("build", "binary_sha256", "schema_target", "warning_count"):
        add("构建", key, a.get(key), b.get(key))
    for stage in ("before", "after"):
        for key in ("schema", "quick_check", "foreign_key_count", "foreign_key_examples"):
            add(stage, key, a.get(stage, {}).get(key), b.get(stage, {}).get(key))
    ac, bc = ({c["name"]: c["status"] for c in r["checks"]} for r in (a, b))
    for name in sorted(ac.keys() | bc.keys()):
        add("检查", name, ac.get(name), bc.get(name))
    for i in range(max(len(a["starts"]), len(b["starts"]))):
        left, right = (r["starts"][i] if i < len(r["starts"]) else {} for r in (a, b))
        for key in ("startup_seconds", "shutdown"):
            add(f"启动 {i + 1}", key, left.get(key), right.get(key))
        for level in ("ERROR", "WARN", "panic"):
            add(f"日志 {i + 1}", level, left.get("logs", {}).get("levels", {}).get(level),
                right.get("logs", {}).get("levels", {}).get(level))
        add(f"迁移 {i + 1}", "migrations", left.get("logs", {}).get("migrations"), right.get("logs", {}).get("migrations"))
        for field in ("observed", "unobserved", "disabled"):
            def task_map(run):
                values = run.get("tasks", {}).get(field, [])
                return {v["name"]: v["evidence"] for v in values} if field == "observed" else {v: True for v in values}
            lt, rt = task_map(left), task_map(right)
            for name in sorted(lt.keys() | rt.keys()):
                add(f"任务 {i + 1} {field}", name, lt.get(name), rt.get(name))
        ap, bp = ({p["path"]: p for p in r.get("endpoints", [])} for r in (left, right))
        for path in sorted(ap.keys() | bp.keys()):
            add(f"接口 {i + 1}", path, ap.get(path), bp.get(path))
    at, bt = (r.get("after", {}).get("tables", {}) for r in (a, b))
    for name in sorted(at.keys() | bt.keys()):
        add("迁移后行数", name, at.get(name), bt.get(name))
    return "\n".join(["# 升级演练对比", "", f"A: {cell(a['label'])}; B: {cell(b['label'])}", "",
                       table(["类别", "项目", "A", "B", "差异"], rows), ""])


def positive(value):
    number = float(value)
    if not math.isfinite(number) or number <= 0:
        raise argparse.ArgumentTypeError("must be finite and positive")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    if sys.version_info < (3, 11):
        parser.error("Python 3.11+ is required")
    commands = parser.add_subparsers(dest="command", required=True)
    runner = commands.add_parser("run")
    for flag in ("hub-bin", "snapshot", "work", "out-dir"):
        runner.add_argument("--" + flag, type=Path, required=True)
    runner.add_argument("--port", type=int, default=18760)
    runner.add_argument("--label")
    runner.add_argument("--endpoints", type=Path)
    runner.add_argument("--expect-tasks", type=Path)
    runner.add_argument("--baseline", type=Path)
    runner.add_argument("--timeout", type=positive, default=120)
    runner.add_argument("--observe-seconds", type=positive, default=3)
    diff = commands.add_parser("compare")
    diff.add_argument("a", type=Path)
    diff.add_argument("b", type=Path)
    args = parser.parse_args()
    os.umask(0o077)
    def interrupted(signum, frame):
        raise KeyboardInterrupt(f"signal {signum}")
    signal.signal(signal.SIGTERM, interrupted)
    try:
        if args.command == "compare":
            print(compare(read_json(args.a), read_json(args.b)), end="")
            return 0
        return run(args)
    except (OSError, ValueError, KeyError, TypeError) as exc:
        print(scrub(f"FAIL: {exc}"), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
