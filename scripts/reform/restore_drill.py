#!/usr/bin/env python3
"""Restore an isolated Hub backup (Python 3.11+, standard library only).

Never invokes binary flags: unknown flags can start a Hub in its default home.
Every Hub launch uses a replacement environment and explicit isolated home/port.
Raw logs, copied private data and generated credentials stay under --work.
Existing work/output is never overwritten; use a fresh work directory per run.
"""

import argparse
from contextlib import closing
from datetime import datetime, timezone
import hashlib
import http.cookiejar
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import signal
import sqlite3
import subprocess
import sys
import tarfile
import time
import urllib.request
import uuid

if sys.version_info < (3, 11):
    raise SystemExit("restore_drill requires Python 3.11+; run with a supported interpreter")

# Explicit file import also works with python3 -I, without changing sys.path.
spec = importlib.util.spec_from_file_location("rehearse", Path(__file__).with_name("rehearse.py"))
r = importlib.util.module_from_spec(spec)
spec.loader.exec_module(r)
PARTS = ("data/files", "signing", "secrets", "workspaces", "deliveries", "steve.env")


def boundary(home, port):
    home = home.expanduser().resolve()
    user = Path.home().resolve()
    if home in (user, Path("/")) or (home.is_relative_to(user) and
            home.relative_to(user).parts[0].startswith(".steve")):
        raise ValueError("refusing a production/default home or its descendant")
    if not 1024 <= port <= 65531 or 8440 in (port, port + 1, port + 3, port + 4):
        raise ValueError("unsafe source/target Hub or local Node port")
    return home


class Process(r.Process):
    def __init__(self, binary, env, log, home):
        # Do not use rehearse.Process.__init__: its backup=off assertion is
        # deliberately stronger. Retain its whole-process-group stop routine.
        assert env.get("STEVE_HOME") and Path(env["STEVE_HOME"]) == boundary(home, int(env["STEVE_PORT"]))
        assert env == isolated_environment(home, int(env["STEVE_PORT"]), env["STEVE_BACKUP"])
        assert env["STEVE_BACKUP"] in ("on", "off")
        self.stream = log.open("xb")
        try:
            self.p = subprocess.Popen([str(binary)], env=env, cwd=home,
                                      stdout=self.stream, stderr=subprocess.STDOUT,
                                      start_new_session=True)
        except BaseException:
            self.stream.close()
            raise


def isolated_environment(home, port, backup):
    boundary(home, port)
    env = r.environment(home, port)  # replacement env, equivalent to env -i
    env["STEVE_BACKUP"] = backup
    return env


def api(opener, base, path, payload=None):
    if payload is None:
        result, body = r.request(opener, base, path, 15)
        if result["status"] != 200:
            raise RuntimeError(f"{path} returned {result['status']}")
    else:
        req = urllib.request.Request(base + path, data=json.dumps(payload).encode(),
                                     headers={"Content-Type": "application/json"})
        with opener.open(req, timeout=15) as response:
            if response.status != 200:
                raise RuntimeError(f"{path} returned {response.status}")
            body = response.read()
    return json.loads(body)


def start(binary, home, port, backup, processes, report, label):
    process = Process(binary, isolated_environment(home, port, backup), home / "hub.log", home)
    processes.append((label, process, home / "hub.log"))  # own it before any probe
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), r.NoRedirect(),
                                        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    base = f"http://127.0.0.1:{port}"
    began = time.monotonic()
    while time.monotonic() - began < 120:
        if process.p.poll() is not None:
            raise RuntimeError(f"{label} Hub exited before healthz")
        result, body = r.request(opener, base, "/healthz", 1)
        if result["status"] == 200:
            health = json.loads(body)
            if health.get("ok") is True:
                r.check(report, label + "_healthz", "PASS", {
                    "seconds": r.elapsed(began), **{k: health.get(k) for k in ("ok", "version", "build_commit")}})
                break
        time.sleep(.2)
    else:
        raise TimeoutError(f"{label} healthz deadline exceeded")
    login, _ = r.request(opener, base, "/auth/dev", 15)
    if login["status"] not in (302, 303):
        raise RuntimeError(f"{label} dev login returned {login['status']}")
    r.check(report, label + "_login", "PASS", login)
    return process, opener, base


def script_backup(home, port):
    """Online SQLite backup + Hub's PARTS contract, without shell/default-home risk.

    ops/backup.sh omits deliveries in this revision; use the Hub's full inventory.
    No pruning: every run owns a newly created home with no earlier backups.
    """
    boundary(home, port)
    out = home / "backups"
    out.mkdir(mode=0o700, exist_ok=True)
    day = datetime.now().strftime("%Y%m%d")
    database = out / f"daily-{day}.sqlite3"
    partial = database.with_suffix(".sqlite3.drill.partial")
    with closing(r.db_connect(home / "data/steve.sqlite3")) as source:
        with closing(sqlite3.connect(partial)) as target:
            deadline = time.monotonic() + 180
            def progress(status, remaining, total):
                if time.monotonic() > deadline:
                    raise TimeoutError("online backup exceeded 180 seconds")
            source.backup(target, pages=-1, progress=progress)
            if target.execute("PRAGMA quick_check").fetchall() != [("ok",)]:
                raise RuntimeError("backup quick_check failed")
    partial.rename(database)
    archive = out / f"daily-{day}-files.tar"
    def include(info):
        return None if info.name.rstrip("/") == "signing/keychain-pass" else info
    with tarfile.open(str(archive) + ".partial", "x", dereference=False) as tar:
        for part in PARTS:
            if (home / part).exists():
                tar.add(home / part, arcname=part, filter=include)
    Path(str(archive) + ".partial").rename(archive)
    return database, archive


def backup(home, port, method, process, opener, base, report):
    began = time.monotonic()
    details = {"requested_method": method, "method": method, "trigger": {
        "enabled_value": "on" if method == "hub" else "off", "first_tick_seconds": 300,
        "tick_seconds": 600, "from_local_time": "03:30", "max_wait_seconds": 600,
        "local_time_at_attempt": datetime.now().strftime("%H:%M:%S"),
        "no_resume": True, "job_registered": False,
        "source_contract": "hub-rs/crates/steve-hub/src/wire/mod.rs: if !cfg.no_resume"}}
    report["backup"] = details
    if method == "hub":
        # wire/mod.rs does not register daily_backup at all under NO_RESUME.
        # Never enable execution on a production database copy to get a backup.
        details["fallback_reason"] = "NO_RESUME isolation suppresses the daily backup job; cannot trigger within 600 seconds"
        details["method"] = "script"
    if details["method"] == "script":
        # Freeze only our process group so a pending Hub tick cannot race the
        # fallback files; SQLite's online backup can still read its WAL.
        process.send(signal.SIGSTOP)
        try:
            database, archive = script_backup(home, port)
        finally:
            process.send(signal.SIGCONT)
        details["implementation"] = "Python sqlite3.backup + tarfile; Hub inventory including deliveries"
    details["seconds"] = r.elapsed(began)
    details["files"] = [{"name": p.name, "bytes": p.stat().st_size} for p in (database, archive)]
    status = api(opener, base, "/api/system/backup")
    matched = (status.get("last") or {}).get("name") == database.name and (
        status.get("last") or {}).get("bytes") == database.stat().st_size
    r.check(report, "backup_status_api", "PASS" if matched else "FAIL",
            {"last": status.get("last"), "daily": status.get("daily")})
    return database, archive


def restore(database, archive, target):
    """Restore into a new home; allow only documented parts, never links/devices."""
    target.mkdir(mode=0o700)
    (target / "data").mkdir()
    import shutil
    shutil.copyfile(database, target / "data/steve.sqlite3")
    with tarfile.open(archive, "r:") as tar:
        members = tar.getmembers()
        for item in members:
            path = PurePosixPath(item.name)
            if (path.is_absolute() or ".." in path.parts or
                    not any(str(path) == p or str(path).startswith(p + "/") for p in PARTS) or
                    str(path) == "signing/keychain-pass" or not (item.isfile() or item.isdir())):
                raise ValueError("unsafe or unexpected backup archive member")
        tar.extractall(target, members=members, filter="data")


def fixtures(home, marker):
    # Synthetic files exercise every documented tar root, not private vault keys.
    result = {}
    for part in PARTS:
        path = home / part
        if part != "steve.env":
            path.mkdir(parents=True, exist_ok=True)
            path /= "restore-drill.txt"
        content = ("# " + marker + "\n").encode()
        path.write_bytes(content)
        result[str(path.relative_to(home))] = hashlib.sha256(content).hexdigest()
    (home / "signing/keychain-pass").write_text("synthetic-do-not-back-up\n")
    return result


def state(path):
    # All table counts and quick_check belong to one SQLite read transaction.
    with closing(r.db_connect(path)) as db:
        db.execute("BEGIN")
        tables = {}
        for (name,) in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
            quoted = '"' + name.replace('"', '""') + '"'
            tables[name] = db.execute(f"SELECT count(*) FROM {quoted}").fetchone()[0]
        return {"tables": tables, "quick_check": [row[0] for row in db.execute("PRAGMA quick_check")]}


def render(report):
    lines = [report["verdict"], "", "备份：" + r.cell(report.get("backup", {})), "",
             f"恢复耗时（至目标健康）：{report.get('restore_seconds', 'unavailable')} s", "",
             r.table(["检查项", "结论", "证据"], [(c["name"], c["status"], c["evidence"])
                     for c in report["checks"]]), "", "假设与局限：", ""]
    lines += ["- " + note for note in report["limitations"]]
    backup_tables = report.get("backup_state", {}).get("tables", {})
    restored = report.get("restored_state", {}).get("tables", {})
    live = report.get("target_live_state", {}).get("tables", {})
    lines += ["", r.table(["表", "备份", "恢复后启动前", "目标运行中"],
              [(name, backup_tables.get(name), restored.get(name), live.get(name))
               for name in sorted(backup_tables.keys() | restored.keys() | live.keys())]), ""]
    return "\n".join(lines)


def run(args):
    work = boundary(args.work, args.port)
    out = boundary(args.out_dir, args.port)
    binary, snapshot = (p.expanduser().resolve(strict=True) for p in (args.hub_bin, args.snapshot))
    if any(a == b or a in b.parents or b in a.parents
           for a, b in ((work, out), (work, snapshot), (out, snapshot), (work, binary), (out, binary))):
        raise ValueError("work/output must be separate from each other and input files")
    if out.exists() and any(out.iterdir()):
        raise ValueError("output must be new or empty")
    # Check all four ports even though local Node is disabled, as in rehearse.
    r.safe_work(work, args.port)
    r.safe_work(work, args.port + 1)
    work.mkdir(parents=True, mode=0o700)  # exclusive, never delete prior evidence
    out.mkdir(parents=True, exist_ok=True, mode=0o700)
    report = {"format": 1, "created_utc": datetime.now(timezone.utc).isoformat(), "checks": [],
              "ports": {"source": args.port, "target": args.port + 1}, "limitations": [
                  "只复制数据库快照；附件及签名等使用合成文件验证，未验证生产附件、外部 vault 密钥或解密。",
                  "NO_RESUME、Node off、false adapters、禁用更新；未验证真实会话执行、外部服务及模型。",
                  "全表计数严格比较备份与恢复后启动前的一致事务；启动后的后台写入另列完整计数和差异。",
                  "source_before_backup 是标记写入后的源库计数；精确恢复基线取自已完成的备份库。",
                  "NO_RESUME 不注册每日备份任务（wire/mod.rs）；默认 hub 模式立即回退 Python 等价实现；未实测 Hub 自带备份。",
                  "目标健康后观察 3 秒并检查接口；日志检查覆盖源、目标的整个启动至停止过程。",
                  "每次使用新 work/out，不覆盖旧演练；中断后保留私有数据供排查，再用新目录重跑。"]}
    processes, stopped = [], set()
    def stop(label, process):
        if label not in stopped:
            outcome = process.stop(30)
            stopped.add(label)
            r.check(report, label + "_cleanup", "PASS" if outcome["process_group_gone"] and
                    not outcome["forced"] and not outcome["already_exited"] else "FAIL", outcome)
    try:
        with binary.open("rb") as stream:
            report["binary_sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
        source, target = work / "source", work / "target"
        source.mkdir(mode=0o700)
        db, report["snapshot"] = r.prepare(snapshot, source)
        marker = "restore-drill-" + uuid.uuid4().hex
        files = fixtures(source, marker)
        process, opener, base = start(binary, source, args.port,
                                     "on" if args.method == "hub" else "off", processes, report, "source")
        item = api(opener, base, "/api/access/tokens", {"name": marker, "days": 1})["item"]
        marker_id = item["id"]  # discard response's secret token; never persist it
        report["marker"] = {"name": marker, "id": marker_id}
        report["source_before_backup"] = state(db)
        database, archive = backup(source, args.port, args.method, process, opener, base, report)
        report["backup_state"] = state(database)
        r.check(report, "backup_quick_check", "PASS" if report["backup_state"]["quick_check"] == ["ok"] else "FAIL",
                report["backup_state"]["quick_check"])
        stop("source", process)
        began = time.monotonic()
        restore(database, archive, target)
        report["restore_files_seconds"] = r.elapsed(began)
        report["restored_state"] = state(target / "data/steve.sqlite3")
        r.check(report, "restored_quick_check", "PASS" if report["restored_state"]["quick_check"] == ["ok"] else "FAIL",
                report["restored_state"]["quick_check"])
        r.check(report, "all_table_counts", "PASS" if report["backup_state"]["tables"] ==
                report["restored_state"]["tables"] else "FAIL", {"tables": len(report["backup_state"]["tables"]),
                "stage": "restored before target startup"})
        file_checks = {p: (target / p).is_file() and hashlib.sha256((target / p).read_bytes()).hexdigest() == digest
                       for p, digest in files.items()}
        file_checks["keychain_password_excluded"] = not (target / "signing/keychain-pass").exists()
        r.check(report, "archive_contents", "PASS" if all(file_checks.values()) else "FAIL", file_checks)
        _, opener, base = start(binary, target, args.port + 1, "off", processes, report, "target")
        report["restore_seconds"] = r.elapsed(began)
        tokens = api(opener, base, "/api/access")["tokens"]
        found = any(t.get("id") == marker_id and t.get("name") == marker for t in tokens)
        r.check(report, "marker_api_readback", "PASS" if found else "FAIL", {"found": found})
        endpoints = [r.request(opener, base, path, 15)[0] for path in r.ENDPOINTS]
        r.check(report, "major_endpoints", "PASS" if all(p["status"] == 200 for p in endpoints) else "FAIL", endpoints)
        time.sleep(3)
        report["target_live_state"] = state(target / "data/steve.sqlite3")
        before, after = report["backup_state"]["tables"], report["target_live_state"]["tables"]
        report["startup_count_differences"] = {n: {"backup": before.get(n), "live": after.get(n)}
                                               for n in before.keys() | after.keys() if before.get(n) != after.get(n)}
        r.check(report, "target_live_quick_check", "PASS" if report["target_live_state"]["quick_check"] == ["ok"]
                else "FAIL", report["target_live_state"]["quick_check"])
    except (Exception, KeyboardInterrupt) as exc:
        # Exception messages may carry response bodies/private data: retain type only.
        r.check(report, "execution", "FAIL", {"exception": type(exc).__name__})
    finally:
        for label, process, log in reversed(processes):
            try:
                stop(label, process)
            except Exception as exc:
                r.check(report, label + "_cleanup", "FAIL", {"exception": type(exc).__name__})
            levels = r.log_summary(log)[0]["levels"]
            counts = {name: data["count"] for name, data in levels.items()}
            r.check(report, label + "_logs", "PASS" if not counts["ERROR"] and not counts["panic"] else "FAIL", counts)
    report["verdict"] = "FAIL" if any(c["status"] == "FAIL" for c in report["checks"]) else "PASS"
    report = r.scrub(report)
    (out / "restore_report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    (out / "restore_report.md").write_text(render(report))
    print(report["verdict"] + ": restore_report.json, restore_report.md")
    return int(report["verdict"] != "PASS")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("hub-bin", "snapshot", "work", "out-dir"):
        parser.add_argument("--" + flag, type=Path, required=True)
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--method", choices=("hub", "script"), default="hub")
    args = parser.parse_args()
    os.umask(0o077)
    def interrupted(signum, frame):
        raise KeyboardInterrupt(f"signal {signum}")
    signal.signal(signal.SIGTERM, interrupted)
    try:
        return run(args)
    except (Exception, KeyboardInterrupt) as exc:
        print(f"FAIL: {type(exc).__name__} during preflight", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
