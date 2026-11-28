#!/usr/bin/env python3
"""Unit and simulated end-to-end tests for ops/rehearse-upgrade.py."""

from __future__ import annotations

import http.server
import importlib.util
import io
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

OPS = Path(__file__).resolve().parent


def load_module():
    spec = importlib.util.spec_from_file_location("rehearse_upgrade", OPS / "rehearse-upgrade.py")
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules["rehearse_upgrade"] = module
    spec.loader.exec_module(module)
    return module


REH = load_module()

FAKE_HUB_CODE = r'''#!/usr/bin/env python3
import functools
import http.server
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import threading
import time
import urllib.parse

BUNDLE = Path(__file__).resolve().parents[1]
MANIFEST = json.loads((BUNDLE / "release.json").read_text(encoding="utf-8"))
VERSION = MANIFEST["version"]
COMMIT = MANIFEST["commit"]
TARGET = os.environ.get("FAKE_TARGET", "aarch64-apple-darwin")
LEGACY_POLICY = bool(MANIFEST.get("fake_legacy_policy", False))
HAS_LEGACY_OPS = (BUNDLE / "ops/run.sh").is_file()
PID_LOG_ENV_VARS = ("FAKE_REHEARSAL_PID_LOG", "STEVE_TEST_PID_LOG")
NODE_BOOT_DELAY_SECS = 0.2
SUPERVISOR_POLL_SECS = 0.2
PROC_TERM_WAIT_SECS = 3.0
PROC_KILL_WAIT_SECS = 2.0
PID_POLL_INTERVAL_SECS = 0.1


def record_pid(role: str, pid: int) -> None:
    log_path = next(
        (os.environ.get(name, "").strip() for name in PID_LOG_ENV_VARS if os.environ.get(name, "").strip()),
        "",
    )
    if not log_path:
        return
    try:
        fd = os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    except OSError:
        return
    try:
        os.write(fd, f"{pid}\t{role}\n".encode("utf-8"))
    except OSError:
        pass
    finally:
        os.close(fd)


def stop_proc(proc: subprocess.Popen | None, pid_file: Path | None = None) -> None:
    if proc is None:
        return
    if proc.poll() is None:
        try:
            proc.terminate()
        except ProcessLookupError:
            pass
        try:
            proc.wait(timeout=PROC_TERM_WAIT_SECS)
        except subprocess.TimeoutExpired:
            try:
                proc.kill()
            except ProcessLookupError:
                pass
            try:
                proc.wait(timeout=PROC_KILL_WAIT_SECS)
            except subprocess.TimeoutExpired:
                pass
    if pid_file is not None and pid_file.is_file():
        try:
            lines = pid_file.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            lines = []
        if lines and lines[0].strip() == str(proc.pid):
            pid_file.unlink(missing_ok=True)


def parse_env(path: Path) -> dict[str, str]:
    out = {}
    if not path.is_file():
        return out
    for raw in path.read_text(encoding="utf-8").splitlines():
        s = raw.strip()
        if not s or s.startswith("#") or "=" not in s:
            continue
        k, v = s.split("=", 1)
        v = v.strip()
        if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
            v = v[1:-1]
        out[k.strip()] = v
    return out


def db_path(home: Path) -> Path:
    return home / "data/fake-db.json"


def load_db(home: Path) -> dict:
    p = db_path(home)
    if not p.is_file():
        return {
            "workspace": "ws-rehearsal",
            "bots": {},
            "memories": {},
            "tags": {},
            "items": {},
            "approval_policy": {"mode": "ask_owner", "revision": 1},
            "permission_policy": {"mode": "ask", "revision": 1},
            "seq": 1,
        }
    return json.loads(p.read_text(encoding="utf-8"))


def save_db(home: Path, db: dict) -> None:
    p = db_path(home)
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = p.with_suffix(".tmp")
    tmp.write_text(json.dumps(db, indent=2), encoding="utf-8")
    os.replace(tmp, p)


def ensure_sqlite_marker(home: Path) -> None:
    db_file = home / "data/steve.sqlite3"
    db_file.parent.mkdir(parents=True, exist_ok=True)
    if not db_file.exists():
        db_file.write_bytes(b"SQLite format 3\x00fake")
    if not db_path(home).exists():
        save_db(home, load_db(home))


def plist_path_for(home: Path, label: str) -> Path:
    return Path.home() / "Library/LaunchAgents" / f"{label}.plist"


def stop_pid_file(pid_file: Path) -> None:
    if not pid_file.is_file():
        return
    lines = pid_file.read_text(encoding="utf-8", errors="replace").splitlines()
    if not lines:
        pid_file.unlink(missing_ok=True)
        return
    first = lines[0].strip()
    try:
        pid = int(first)
    except ValueError:
        return
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pid_file.unlink(missing_ok=True)
        return
    for _ in range(int(PROC_TERM_WAIT_SECS / PID_POLL_INTERVAL_SECS)):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(PID_POLL_INTERVAL_SECS)
    else:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        for _ in range(int(PROC_KILL_WAIT_SECS / PID_POLL_INTERVAL_SECS)):
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(PID_POLL_INTERVAL_SECS)
    pid_file.unlink(missing_ok=True)


def spawn_fake_node(home: Path, bundle: Path) -> subprocess.Popen:
    node_dir = home / "data/node"
    node_dir.mkdir(parents=True, exist_ok=True)
    node_exe = str(home / "current/bin/steve-node")
    env = os.environ.copy()
    env["STEVE_HOME"] = str(home)
    real_proc = subprocess.Popen(
        [node_exe, "--local-node"],
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        start_new_session=True,
    )
    record_pid("fake-node", real_proc.pid)
    (node_dir / "service.pid").write_text(f"{real_proc.pid}\n{VERSION}\n", encoding="utf-8")
    return real_proc


def start_detached_hub(home: Path, bundle: Path, label: str | None = None, supervise: bool = False) -> subprocess.Popen:
    log_dir = home / "logs"
    log_dir.mkdir(parents=True, exist_ok=True)
    env = os.environ.copy()
    env.update(parse_env(home / "steve.env"))
    env["STEVE_HOME"] = str(home)
    env["STEVE_WEB_DIR"] = str(home / "web/current") if (home / "web/current").exists() else str(bundle / "web/dist")
    if label:
        env["FAKE_LABEL"] = label
    args = [str(bundle / "bin/steve-hub")]
    if supervise:
        args.append("--supervise")
    with (log_dir / "hub.log").open("a", encoding="utf-8") as log_f:
        proc = subprocess.Popen(
            args,
            cwd=str(home),
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=log_f,
            stderr=log_f,
            start_new_session=True,
        )
    record_pid("fake-supervisor" if supervise else "fake-hub", proc.pid)
    pid_name = "fake-supervisor.pid" if supervise else "fake-hub.pid"
    (home / pid_name).write_text(f"{proc.pid}\n", encoding="utf-8")
    return proc


def serve_hub(home: Path, supervise: bool = False) -> int:
    record_pid("fake-supervisor" if supervise else "fake-hub", os.getpid())
    env_map = parse_env(home / "steve.env")
    port = int(os.environ.get("STEVE_PORT") or env_map["STEVE_PORT"])
    web_port = int(os.environ.get("STEVE_WEB_PORT") or env_map.get("STEVE_WEB_PORT") or str(port + 1))
    web_dir = Path(os.environ.get("STEVE_WEB_DIR") or env_map.get("STEVE_WEB_DIR") or str(BUNDLE / "web/dist"))
    no_resume = os.environ.get("STEVE_NO_RESUME") == "1"
    ensure_sqlite_marker(home)
    lock = threading.Lock()

    if supervise:
        stop_flag = threading.Event()
        hub_lock = threading.Lock()
        hub_state: dict[str, subprocess.Popen | None] = {
            "proc": start_detached_hub(home, BUNDLE, supervise=False)
        }

        def _stop_supervisor(*_args):
            stop_flag.set()

        signal.signal(signal.SIGTERM, _stop_supervisor)
        signal.signal(signal.SIGINT, _stop_supervisor)

        def _watch_updates():
            while not stop_flag.is_set():
                active = home / "updates/active.json"
                if active.is_file():
                    op_id = json.loads(active.read_text(encoding="utf-8")).get("id")
                    op_file = home / "updates/operations" / f"{op_id}.json"
                    if op_file.is_file():
                        op = json.loads(op_file.read_text(encoding="utf-8"))
                        if op.get("phase") == "ready":
                            target = Path(op["target"])
                            prev = (home / "current").resolve()
                            op["previous"] = str(prev)
                            op["backup_snapshot"] = "snap-linux-1"
                            (home / "backups/snap-linux-1.json").parent.mkdir(parents=True, exist_ok=True)
                            (home / "backups/snap-linux-1.json").write_text(
                                json.dumps(load_db(home)), encoding="utf-8"
                            )
                            with hub_lock:
                                cur_proc = hub_state["proc"]
                                hub_state["proc"] = None
                                stop_proc(cur_proc, home / "fake-hub.pid")
                                stop_pid_file(home / "fake-hub.pid")
                                stop_pid_file(home / "data/node/service.pid")
                                for lp, tgt in ((home / "current", target), (home / "web/current", target / "web/dist")):
                                    tmp = lp.parent / f".{lp.name}.tmp"
                                    tmp.unlink(missing_ok=True)
                                    os.symlink(tgt, tmp)
                                    os.replace(tmp, lp)
                                unit = Path.home() / ".config/systemd/user" / f"{os.environ.get('FAKE_LABEL', 'steve')}.service"
                                if unit.is_file():
                                    prev_unit = unit.with_suffix(".service.previous")
                                    prev_unit.write_text(unit.read_text(encoding="utf-8"), encoding="utf-8")
                                    unit.write_text(f"[Service]\nExecStart={home}/current/bin/steve-hub --supervise\n", encoding="utf-8")
                                if not stop_flag.is_set():
                                    hub_state["proc"] = start_detached_hub(home, target, supervise=False)
                                    op["phase"] = "succeeded"
                                    op_file.write_text(json.dumps(op), encoding="utf-8")
                stop_flag.wait(SUPERVISOR_POLL_SECS)

        watcher = threading.Thread(target=_watch_updates, daemon=True)
        watcher.start()
        while not stop_flag.is_set():
            stop_flag.wait(SUPERVISOR_POLL_SECS)
        watcher.join(timeout=5.0)
        with hub_lock:
            cur_proc = hub_state["proc"]
            hub_state["proc"] = None
        stop_proc(cur_proc, home / "fake-hub.pid")
        stop_pid_file(home / "fake-hub.pid")
        stop_pid_file(home / "data/node/service.pid")
        return 0

    node_lock = threading.Lock()
    node_state: dict[str, object] = {"linked": False, "version": None, "proc": None, "closed": False}

    class WebHandler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            html = (web_dir / "index.html").read_bytes()
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            self.send_header("Content-Length", str(len(html)))
            self.end_headers()
            self.wfile.write(html)

        def log_message(self, *_args):
            pass

    class ApiHandler(http.server.BaseHTTPRequestHandler):
        def _send(self, status: int, body: object) -> None:
            raw = json.dumps(body).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def _send_text(self, status: int, text: str) -> None:
            raw = text.encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(raw)))
            self.end_headers()
            self.wfile.write(raw)

        def _body(self) -> dict:
            length = int(self.headers.get("Content-Length", "0"))
            raw = self.rfile.read(length) if length > 0 else b"{}"
            return json.loads(raw.decode("utf-8"))

        def do_GET(self):
            path = urllib.parse.urlparse(self.path).path
            if path == "/healthz":
                with node_lock:
                    node_linked = bool(node_state["linked"])
                node_obj = {"enabled": not no_resume, "reconciled": node_linked}
                if not LEGACY_POLICY:
                    node_obj["linked"] = node_linked
                self._send(200, {
                    "ok": True,
                    "version": VERSION,
                    "build_commit": COMMIT,
                    "web_version": VERSION,
                    "drain": False,
                    "recovery": {"state": "ok"},
                    "node": node_obj,
                })
                return
            if path == "/api/nodes":
                with node_lock:
                    node_linked = bool(node_state["linked"])
                    node_version = node_state["version"]
                self._send(200, {
                    "nodes": [{
                        "node_id": "local",
                        "local": True,
                        "linked": node_linked,
                        "state": "active" if node_linked else "offline",
                        "node_version": node_version,
                    }]
                })
                return
            with lock:
                db = load_db(home)
                if path == "/api/me":
                    self._send(200, {"workspace": db["workspace"], "version": VERSION, "web_version": VERSION})
                    return
                if path == "/api/shell":
                    self._send(200, {"version": VERSION, "bots": list(db["bots"].values())})
                    return
                if path.startswith("/api/bots/") and path.endswith("/memories"):
                    bot_id = path.split("/")[3]
                    self._send(200, db["memories"].get(bot_id, []))
                    return
                if path == "/api/tags":
                    self._send(200, {"tags": list(db["tags"].values()), "uses": None})
                    return
                if path.startswith("/api/items/"):
                    item_id = path.split("/")[3]
                    if (home / "inject-verify-fail").exists():
                        self._send(404, {"error": "injected missing item"})
                        return
                    item = db["items"].get(item_id)
                    if item is None:
                        self._send(404, {"error": "not found"})
                    else:
                        self._send(200, item)
                    return
                if path == "/api/approval-policy":
                    if not LEGACY_POLICY:
                        self._send_text(404, "404 page not found\n")
                        return
                    self._send(200, {"policy": db["approval_policy"], "editable": True})
                    return
                if path == "/api/permission-policy":
                    if LEGACY_POLICY:
                        self._send_text(404, "404 page not found\n")
                        return
                    self._send(200, {"policy": db["permission_policy"], "editable": True})
                    return
            self._send_text(404, "404 page not found\n")

        def do_POST(self):
            path = urllib.parse.urlparse(self.path).path
            body = self._body()
            if path == "/auth/token":
                self._send(200, {"client_id": "cli-1", "credential": "cred-token"})
                return
            with lock:
                db = load_db(home)
                seq = db["seq"]
                db["seq"] = seq + 1
                if path == "/api/bots":
                    bot_id = f"bot-{seq}"
                    bot = {"id": bot_id, **body}
                    db["bots"][bot_id] = bot
                    save_db(home, db)
                    self._send(200, bot)
                    return
                if path.startswith("/api/bots/") and path.endswith("/memories"):
                    bot_id = path.split("/")[3]
                    mem_id = f"mem-{seq}"
                    entry = {"id": mem_id, "text": body["text"]}
                    db["memories"].setdefault(bot_id, []).append(entry)
                    save_db(home, db)
                    self._send(200, {"id": mem_id})
                    return
                if path == "/api/tags":
                    tag_id = f"tag-{seq}"
                    tag = {"id": tag_id, **body}
                    db["tags"][tag_id] = tag
                    save_db(home, db)
                    self._send(200, tag)
                    return
                if path == "/api/items":
                    item_id = f"item-{seq}"
                    item = {"id": item_id, **body}
                    db["items"][item_id] = item
                    save_db(home, db)
                    self._send(200, item)
                    return
            self._send(404, {"error": f"unknown POST {path}"})

        def do_PUT(self):
            path = urllib.parse.urlparse(self.path).path
            body = self._body()
            with lock:
                db = load_db(home)
                if path == "/api/approval-policy" and LEGACY_POLICY:
                    mode = body["mode"]
                    rev = db["approval_policy"]["revision"] + 1
                    db["approval_policy"] = {"mode": mode, "revision": rev}
                    db["permission_policy"] = {
                        "mode": "auto" if mode == "harness_auto" else "ask",
                        "revision": rev,
                    }
                    save_db(home, db)
                    self._send(200, {"policy": db["approval_policy"], "editable": True})
                    return
                if path == "/api/permission-policy" and not LEGACY_POLICY:
                    mode = body["mode"]
                    if mode == "full_access" and not body.get("confirmed"):
                        self._send(400, {"error": "confirmed required"})
                        return
                    rev = db["permission_policy"]["revision"] + 1
                    db["permission_policy"] = {"mode": mode, "revision": rev}
                    save_db(home, db)
                    self._send(200, {"policy": db["permission_policy"], "editable": True})
                    return
            self._send(404, {"error": f"unknown PUT {path}"})

        def log_message(self, *_args):
            pass

    api_srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), ApiHandler)
    api_srv.daemon_threads = True
    web_srv = http.server.ThreadingHTTPServer(("127.0.0.1", web_port), WebHandler)
    web_srv.daemon_threads = True
    stop_evt = threading.Event()

    def _shutdown(*_args):
        if stop_evt.is_set():
            return
        stop_evt.set()
        threading.Thread(target=api_srv.shutdown, daemon=True).start()
        threading.Thread(target=web_srv.shutdown, daemon=True).start()

    signal.signal(signal.SIGTERM, _shutdown)
    signal.signal(signal.SIGINT, _shutdown)
    threading.Thread(target=web_srv.serve_forever, daemon=True).start()
    boot_thread: threading.Thread | None = None
    try:
        if not no_resume and not supervise:
            def _boot_node() -> None:
                if stop_evt.wait(NODE_BOOT_DELAY_SECS):
                    return
                with node_lock:
                    if node_state["closed"] or stop_evt.is_set():
                        return
                    proc = spawn_fake_node(home, BUNDLE)
                    node_state["proc"] = proc
                    node_state["version"] = VERSION
                    node_state["linked"] = True

            boot_thread = threading.Thread(target=_boot_node, daemon=True)
            boot_thread.start()
        api_srv.serve_forever()
    finally:
        stop_evt.set()
        api_srv.server_close()
        web_srv.server_close()
        with node_lock:
            node_state["closed"] = True
            node_proc = node_state["proc"]
            node_state["proc"] = None
        if boot_thread is not None:
            boot_thread.join(timeout=2.0)
        stop_proc(node_proc, home / "data/node/service.pid")
    return 0


def main() -> int:
    args = sys.argv[1:]
    if args == ["--build-info"]:
        print(json.dumps({"version": VERSION, "commit": COMMIT, "target": TARGET, "schema": 148}))
        return 0
    if args[:1] == ["--admin"] and len(args) >= 4 and args[1] == "restore" and args[3] == "--database-only":
        home = Path(os.environ["STEVE_HOME"])
        snap = args[2]
        snap_file = home / "backups" / f"{snap}.json"
        if snap_file.is_file():
            save_db(home, json.loads(snap_file.read_text(encoding="utf-8")))
        print(snap)
        return 0
    if args[:1] == ["--local-release"]:
        sub = args[1]
        if sub == "upgrade-macos":
            cand = Path(args[2]).resolve()
            home = Path(args[args.index("--home") + 1]).resolve()
            label = args[args.index("--label") + 1]
            if (home / "inject-upgrade-fail").exists():
                print(json.dumps({"ok": False, "error": "injected upgrade failure"}), file=sys.stderr)
                return 1
            cand_m = json.loads((cand / "release.json").read_text(encoding="utf-8"))
            cand_ver = cand_m["version"]
            prev = (home / "current").resolve()
            prev_m = json.loads((prev / "release.json").read_text(encoding="utf-8"))
            staged = home / "releases" / f"{cand_ver}-{cand_m['commit'][:8]}"
            if not staged.exists():
                shutil.copytree(cand, staged)
            snap_file = home / "backups/snap-macos-1.json"
            snap_file.parent.mkdir(parents=True, exist_ok=True)
            snap_file.write_text(json.dumps(load_db(home)), encoding="utf-8")
            # Migrate approval_policy -> permission_policy on DB upgrade
            db = load_db(home)
            if db.get("approval_policy", {}).get("mode") == "harness_auto":
                db["permission_policy"] = {"mode": "auto", "revision": db["permission_policy"]["revision"]}
                save_db(home, db)
            stop_pid_file(home / "fake-hub.pid")
            stop_pid_file(home / "data/node/service.pid")
            for lp, tgt in ((home / "current", staged), (home / "web/current", staged / "web/dist")):
                tmp = lp.parent / f".{lp.name}.tmp"
                tmp.unlink(missing_ok=True)
                os.symlink(tgt, tmp)
                os.replace(tmp, lp)
            plist_p = plist_path_for(home, label)
            prev_plist_p = plist_p.with_suffix(".plist.previous")
            if not prev_plist_p.exists():
                prev_plist_p.write_bytes(plist_p.read_bytes())
            plist_obj = plistlib.loads(plist_p.read_bytes())
            plist_obj["ProgramArguments"] = [str(home / "current/bin/steve-hub")]
            plist_p.write_bytes(plistlib.dumps(plist_obj))
            journal = {
                "version": cand_ver,
                "candidate": str(staged),
                "previous_hub": str(prev),
                "previous_web": str(prev / "web/dist"),
                "previous_manifest": prev_m,
                "label": label,
                "stage": "succeeded",
                "backup_snapshot": "snap-macos-1",
            }
            (home / "releases/.macos-rollback.json").write_text(json.dumps(journal), encoding="utf-8")
            (home / "logs/release").mkdir(parents=True, exist_ok=True)
            (home / "logs/release/macos-upgrade-fake.json").write_text(
                json.dumps({"ok": True, "version": cand_ver}), encoding="utf-8"
            )
            start_detached_hub(home, staged, label=label)
            print(json.dumps({
                "ok": True,
                "version": cand_ver,
                "backup_snapshot": "snap-macos-1",
                "plist_rewritten": True,
            }))
            return 0
        if sub == "rollback-macos":
            home = Path(args[args.index("--home") + 1]).resolve()
            label = args[args.index("--label") + 1]
            journal_p = home / "releases/.macos-rollback.json"
            journal = json.loads(journal_p.read_text(encoding="utf-8"))
            prev = Path(journal["previous_hub"])
            stop_pid_file(home / "fake-hub.pid")
            stop_pid_file(home / "data/node/service.pid")
            snap_file = home / "backups/snap-macos-1.json"
            if snap_file.is_file():
                save_db(home, json.loads(snap_file.read_text(encoding="utf-8")))
            for lp, tgt in ((home / "current", prev), (home / "web/current", Path(journal["previous_web"]))):
                tmp = lp.parent / f".{lp.name}.tmp"
                tmp.unlink(missing_ok=True)
                os.symlink(tgt, tmp)
                os.replace(tmp, lp)
            plist_p = plist_path_for(home, label)
            prev_plist_p = plist_p.with_suffix(".plist.previous")
            restored_from_backup = False
            if prev_plist_p.is_file():
                plist_p.write_bytes(prev_plist_p.read_bytes())
                prev_plist_p.unlink()
                restored_from_backup = True
            journal_p.unlink(missing_ok=True)
            start_detached_hub(home, prev, label=label)
            print(json.dumps({
                "ok": True,
                "version": journal["previous_manifest"]["version"],
                "database_restored": True,
                "plist_restored_from_backup": restored_from_backup,
            }))
            return 0
        if sub == "confirm-macos":
            home = Path(args[args.index("--home") + 1]).resolve()
            journal_p = home / "releases/.macos-rollback.json"
            journal = json.loads(journal_p.read_text(encoding="utf-8"))
            archive = home / "logs/release" / f"macos-upgrade-confirmed-{journal['version']}.json"
            archive.parent.mkdir(parents=True, exist_ok=True)
            archive.write_text(json.dumps({"journal": journal}), encoding="utf-8")
            journal_p.unlink(missing_ok=True)
            plist_path_for(home, journal["label"]).with_suffix(".plist.previous").unlink(missing_ok=True)
            print(json.dumps({
                "ok": True,
                "confirmed_version": journal["version"],
                "archived_journal": str(archive),
            }))
            return 0
    supervise = "--supervise" in args
    home = Path(os.environ["STEVE_HOME"])
    return serve_hub(home, supervise=supervise)


if __name__ == "__main__":
    raise SystemExit(main())
'''

FAKE_CLI_CODE = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import time

BUNDLE = Path(__file__).resolve().parents[1]
MANIFEST = json.loads((BUNDLE / "release.json").read_text(encoding="utf-8"))
VERSION = MANIFEST["version"]
COMMIT = MANIFEST["commit"]
TARGET = os.environ.get("FAKE_TARGET", "aarch64-apple-darwin")
BROKEN_SETUP = bool(MANIFEST.get("fake_broken_setup", False))
HAS_LEGACY_OPS = (BUNDLE / "ops/run.sh").is_file()
IS_LINUX = TARGET.endswith("unknown-linux-gnu")
PID_LOG_ENV_VARS = ("FAKE_REHEARSAL_PID_LOG", "STEVE_TEST_PID_LOG")
PROC_TERM_WAIT_SECS = 3.0
PROC_KILL_WAIT_SECS = 2.0
PID_POLL_INTERVAL_SECS = 0.1


def record_pid(role: str, pid: int) -> None:
    log_path = next(
        (os.environ.get(name, "").strip() for name in PID_LOG_ENV_VARS if os.environ.get(name, "").strip()),
        "",
    )
    if not log_path:
        return
    try:
        fd = os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    except OSError:
        return
    try:
        os.write(fd, f"{pid}\t{role}\n".encode("utf-8"))
    except OSError:
        pass
    finally:
        os.close(fd)


def parse_env(path: Path) -> dict[str, str]:
    out = {}
    if not path.is_file():
        return out
    for raw in path.read_text(encoding="utf-8").splitlines():
        s = raw.strip()
        if not s or s.startswith("#") or "=" not in s:
            continue
        k, v = s.split("=", 1)
        v = v.strip()
        if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
            v = v[1:-1]
        out[k.strip()] = v
    return out


def service_def_path(label: str) -> Path:
    if IS_LINUX:
        return Path.home() / ".config/systemd/user" / f"{label}.service"
    return Path.home() / "Library/LaunchAgents" / f"{label}.plist"


def stop_pid(pid_file: Path) -> None:
    if not pid_file.is_file():
        return
    lines = pid_file.read_text(encoding="utf-8", errors="replace").splitlines()
    if not lines:
        pid_file.unlink(missing_ok=True)
        return
    first = lines[0].strip()
    try:
        pid = int(first)
    except ValueError:
        return
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pid_file.unlink(missing_ok=True)
        return
    for _ in range(int(PROC_TERM_WAIT_SECS / PID_POLL_INTERVAL_SECS)):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(PID_POLL_INTERVAL_SECS)
    else:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        for _ in range(int(PROC_KILL_WAIT_SECS / PID_POLL_INTERVAL_SECS)):
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                break
            time.sleep(PID_POLL_INTERVAL_SECS)
    pid_file.unlink(missing_ok=True)


def running_pid(pid_file: Path) -> int | None:
    if not pid_file.is_file():
        return None
    try:
        pid = int(pid_file.read_text(encoding="utf-8").splitlines()[0].strip())
        os.kill(pid, 0)
        return pid
    except (ValueError, ProcessLookupError, OSError):
        return None


def main() -> int:
    args = sys.argv[1:]
    if args == ["--build-info"]:
        print(json.dumps({"version": VERSION, "commit": COMMIT, "target": TARGET}))
        return 0
    cmd = args[0]
    if cmd == "setup" and args[1] == "hub":
        home = Path(args[args.index("--home") + 1]).resolve()
        listen = args[args.index("--listen") + 1]
        label = args[args.index("--label") + 1]
        vault_dir = Path(args[args.index("--vault-dir") + 1]).resolve()
        host, port_s = listen.split(":")
        port = int(port_s)
        home.mkdir(parents=True, exist_ok=True)
        vault_dir.mkdir(parents=True, exist_ok=True)
        rel_dir = home / "releases" / VERSION
        if not rel_dir.exists():
            shutil.copytree(BUNDLE, rel_dir)
        env_path = home / "steve.env"
        if not env_path.is_file():
            env_path.write_text(
                f"STEVE_HOST={host}\nSTEVE_PORT={port}\n"
                f"STEVE_PUBLIC_URL=http://{host}:{port + 1}\n"
                f"STEVE_WEB_PORT={port + 1}\n"
                f"STEVE_WEB_DIR={home / 'web/current'}\n"
                f"STEVE_VAULT_KEYSTORE=dir\nSTEVE_VAULT_KEY_DIR={vault_dir}\n",
                encoding="utf-8",
            )
        (home / "setup.json").write_text(json.dumps({"version": VERSION, "label": label}), encoding="utf-8")
        if BROKEN_SETUP and not (home / "data/steve.sqlite3").is_file():
            print("error: missing sqlite database (simulated pre-#634)", file=sys.stderr)
            return 2
        (home / "secrets").mkdir(parents=True, exist_ok=True)
        (home / "secrets/owner.token").write_text("user=maintainer\ntoken=stt_rehearsal_token\n", encoding="utf-8")
        (home / "web").mkdir(parents=True, exist_ok=True)
        for lp, tgt in ((home / "current", rel_dir), (home / "web/current", rel_dir / "web/dist")):
            if not lp.exists() and not lp.is_symlink():
                os.symlink(tgt, lp)
        svc = service_def_path(label)
        svc.parent.mkdir(parents=True, exist_ok=True)
        if IS_LINUX:
            exec_cmd = f"/bin/sh {home}/current/ops/run-managed.sh" if HAS_LEGACY_OPS else f"{home}/current/bin/steve-hub --supervise"
            svc.write_text(f"[Unit]\nDescription=Steve hub\n[Service]\nExecStart={exec_cmd}\n", encoding="utf-8")
        else:
            prog = ["/bin/zsh", str(home / "current/ops/run.sh")] if HAS_LEGACY_OPS else [str(home / "current/bin/steve-hub")]
            svc.write_bytes(plistlib.dumps({"Label": label, "ProgramArguments": prog, "RunAtLoad": True}))
        (home / "services").mkdir(parents=True, exist_ok=True)
        (home / "services/hub.json").write_text(json.dumps({"component": "hub", "label": label, "release": str(rel_dir)}), encoding="utf-8")
        return 0

    if cmd == "hub":
        action = args[1]
        home = Path(args[args.index("--home") + 1]).resolve()
        label = args[args.index("--label") + 1]
        svc = service_def_path(label)
        pid_file = home / ("fake-supervisor.pid" if IS_LINUX else "fake-hub.pid")
        if action == "start":
            if running_pid(pid_file) is None:
                env = os.environ.copy()
                env.update(parse_env(home / "steve.env"))
                env["STEVE_HOME"] = str(home)
                env["STEVE_WEB_DIR"] = str(home / "web/current")
                env["FAKE_LABEL"] = label
                (home / "logs").mkdir(parents=True, exist_ok=True)
                hub_args = [str((home / "current/bin/steve-hub").resolve())]
                if IS_LINUX:
                    hub_args.append("--supervise")
                with (home / "logs/hub.log").open("a", encoding="utf-8") as lf:
                    proc = subprocess.Popen(
                        hub_args,
                        cwd=str(home),
                        env=env,
                        stdin=subprocess.DEVNULL,
                        stdout=lf,
                        stderr=lf,
                        start_new_session=True,
                    )
                record_pid("fake-supervisor" if IS_LINUX else "fake-hub", proc.pid)
                pid_file.write_text(f"{proc.pid}\n", encoding="utf-8")
            print(json.dumps({"ok": True, "started": True, "label": label}))
            return 0
        if action == "stop":
            stop_pid(home / "fake-supervisor.pid")
            stop_pid(home / "fake-hub.pid")
            stop_pid(home / "data/node/service.pid")
            print(json.dumps({"ok": True, "running": False, "label": label}))
            return 0
        if action == "status":
            cur_m = json.loads((home / "current/release.json").read_text(encoding="utf-8"))
            is_up = running_pid(home / "fake-hub.pid") is not None
            print(json.dumps({
                "ok": True,
                "label": label,
                "service": {"installed": svc.is_file(), "running": is_up},
                "configured_build": {"version": cur_m["version"], "commit": cur_m["commit"]},
            }))
            return 0
        if action == "uninstall":
            svc.unlink(missing_ok=True)
            print(json.dumps({"installed": False, "changed": True, "label": label}))
            return 0
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
'''

FAKE_NODE_CODE = r'''#!/usr/bin/env python3
import errno
import json
import os
from pathlib import Path
import signal
import sys
import time

VERSION = "__FAKE_VERSION__"
COMMIT = "__FAKE_COMMIT__"
TARGET = "__FAKE_TARGET__"
POLL_INTERVAL_SECS = 0.1
MAX_LIFETIME_SECS = 600.0
PID_LOG_ENV_VARS = ("FAKE_REHEARSAL_PID_LOG", "STEVE_TEST_PID_LOG")


def record_pid(role: str, pid: int) -> None:
    log_path = next(
        (os.environ.get(name, "").strip() for name in PID_LOG_ENV_VARS if os.environ.get(name, "").strip()),
        "",
    )
    if not log_path:
        return
    try:
        fd = os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    except OSError:
        return
    try:
        os.write(fd, f"{pid}\t{role}\n".encode("utf-8"))
    except OSError:
        pass
    finally:
        os.close(fd)


def _capture_watched(path: Path) -> tuple[Path, tuple[int, int]] | None:
    try:
        st = path.stat()
    except OSError:
        return None
    return path, (st.st_dev, st.st_ino)


def _watched_intact(path: Path, expected: tuple[int, int]) -> bool:
    try:
        st = path.stat()
    except (FileNotFoundError, NotADirectoryError):
        return False
    except OSError as exc:
        if exc.errno in (errno.ENOENT, errno.ENOTDIR):
            return False
        return True
    return (st.st_dev, st.st_ino) == expected


def main() -> int:
    args = sys.argv[1:]
    if args == ["--local-node"]:
        def _handle_exit(_signum: int, _frame) -> None:
            raise SystemExit(0)

        signal.signal(signal.SIGTERM, _handle_exit)
        signal.signal(signal.SIGINT, _handle_exit)

        watched: list[tuple[Path, tuple[int, int]]] = []
        exe_watched = _capture_watched(Path(__file__).resolve())
        if exe_watched is None:
            return 0
        watched.append(exe_watched)

        steve_home = os.environ.get("STEVE_HOME", "").strip()
        if steve_home:
            home_watched = _capture_watched(Path(steve_home))
            if home_watched is None:
                return 0
            watched.append(home_watched)

        record_pid("fake-node", os.getpid())

        deadline = time.monotonic() + MAX_LIFETIME_SECS
        while time.monotonic() < deadline:
            if any(not _watched_intact(path, ident) for path, ident in watched):
                return 0
            time.sleep(POLL_INTERVAL_SECS)
        return 0

    print(json.dumps({"version": VERSION, "commit": COMMIT, "target": TARGET}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
'''


def make_fake_bundle(
    root: Path,
    *,
    version: str,
    commit: str,
    target: str,
    include_cli: bool = True,
    broken_setup: bool = False,
    legacy_ops: bool = False,
    legacy_policy: bool = False,
) -> Path:
    root.mkdir(parents=True, exist_ok=True)
    (root / "bin").mkdir(parents=True, exist_ok=True)
    (root / "web/dist").mkdir(parents=True, exist_ok=True)
    manifest = {
        "format_version": 1,
        "version": version,
        "commit": commit,
        "hub_version": version,
        "web_version": version,
        "api_protocol": 1,
        "schema": {"min_read": 1, "max_read": 1, "target": 1},
        "assets": [],
        "fake_broken_setup": broken_setup,
        "fake_legacy_policy": legacy_policy,
    }
    (root / "release.json").write_text(json.dumps(manifest), encoding="utf-8")
    (root / "web/dist/index.html").write_text(
        f'<html><head><meta name="steve-web-version" content="{version}" /></head><body>ok</body></html>',
        encoding="utf-8",
    )
    (root / "web/dist/release.json").write_text(json.dumps({"version": version, "commit": commit}), encoding="utf-8")

    hub = root / "bin/steve-hub"
    hub.write_text(FAKE_HUB_CODE.replace('TARGET = os.environ.get("FAKE_TARGET", "aarch64-apple-darwin")', f'TARGET = {target!r}'), encoding="utf-8")
    hub.chmod(0o755)

    node = root / "bin/steve-node"
    node_code = (
        FAKE_NODE_CODE.replace("#!/usr/bin/env python3", f"#!{sys.executable}", 1)
        .replace('"__FAKE_VERSION__"', repr(version))
        .replace('"__FAKE_COMMIT__"', repr(commit))
        .replace('"__FAKE_TARGET__"', repr(target))
    )
    node.write_text(node_code, encoding="utf-8")
    node.chmod(0o755)

    restic = root / "bin/restic"
    restic.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    restic.chmod(0o755)

    if target.endswith("apple-darwin"):
        kh = root / "bin/steve-keyhelper"
        kh.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        kh.chmod(0o755)

    if include_cli:
        cli = root / "bin/steve"
        cli.write_text(FAKE_CLI_CODE.replace('TARGET = os.environ.get("FAKE_TARGET", "aarch64-apple-darwin")', f'TARGET = {target!r}'), encoding="utf-8")
        cli.chmod(0o755)

    if legacy_ops:
        (root / "ops").mkdir(parents=True, exist_ok=True)
        (root / "ops/run.sh").write_text("#!/bin/sh\n", encoding="utf-8")
        (root / "ops/run-managed.sh").write_text("#!/bin/sh\n", encoding="utf-8")
    return root


def pack_tar_gz(bundle_dir: Path, archive_path: Path) -> Path:
    with tarfile.open(archive_path, "w:gz") as tf:
        for path in sorted(bundle_dir.rglob("*")):
            tf.add(path, arcname=str(path.relative_to(bundle_dir)), recursive=False)
    return archive_path


class RehearseUpgradeTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="steve-test-reh-")
        self.addCleanup(self._tmp.cleanup)
        self.addCleanup(self._cleanup_recorded_pids)
        self.root = Path(self._tmp.name).resolve()
        self.pid_log = self.root / "spawned-pids.log"
        self.fake_home = self.root / "operator-home"
        self.fake_home.mkdir(parents=True)
        (self.fake_home / ".steve").mkdir()
        (self.fake_home / ".steve/steve.env").write_text("STEVE_PORT=8440\n", encoding="utf-8")
        self.system_calls: list[list[str]] = []

    def _read_recorded_pids(self) -> dict[int, set[str]]:
        records: dict[int, set[str]] = {}
        if not self.pid_log.is_file():
            return records
        for raw_line in self.pid_log.read_text(encoding="utf-8", errors="replace").splitlines():
            line = raw_line.strip()
            if not line:
                continue
            pid_s, _, role = line.partition("\t")
            try:
                pid = int(pid_s)
            except ValueError:
                continue
            records.setdefault(pid, set()).add(role or "unknown")
        return records

    def _wait_for_recorded_pid(
        self,
        pid: int,
        *,
        proc: subprocess.Popen | None = None,
        timeout: float = 5.0,
    ) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if pid in self._read_recorded_pids():
                return
            if proc is not None and proc.poll() is not None:
                self.fail(f"fake process {pid} exited early with code {proc.returncode} before recording PID")
            time.sleep(0.02)
        self.fail(f"fake process {pid} did not record PID within {timeout}s")

    def _is_our_recorded_process(self, pid: int) -> tuple[bool, str]:
        if pid <= 1 or pid == os.getpid():
            return False, "invalid or self pid"
        if not REH.process_is_alive(pid):
            return False, "exited"
        ident = REH.process_identity(pid)
        if ident is None:
            if REH.process_is_alive(pid):
                return True, "alive (identity unavailable)"
            return False, "exited"
        exec_path, argv = ident
        candidates = [exec_path or "", *argv]
        root_str = str(self.root)
        if any(root_str in part for part in candidates):
            return True, repr(ident)
        return False, f"unrelated pid {ident!r}"

    def _cleanup_recorded_pids(self) -> None:
        for pid in sorted(self._read_recorded_pids()):
            ours, _ = self._is_our_recorded_process(pid)
            if not ours:
                continue
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                continue
            deadline = time.monotonic() + 2.0
            while time.monotonic() < deadline:
                if not REH.process_is_alive(pid):
                    break
                time.sleep(0.05)
            else:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass

    def _assert_no_recorded_processes_alive(
        self,
        *,
        min_nodes: int = 0,
        min_hubs: int = 0,
        exact_nodes: int | None = None,
        exact_supervisors: int | None = None,
    ) -> dict[int, set[str]]:
        records = self._read_recorded_pids()
        node_pids = [pid for pid, roles in records.items() if "fake-node" in roles]
        hub_pids = [pid for pid, roles in records.items() if "fake-hub" in roles]
        supervisor_pids = [pid for pid, roles in records.items() if "fake-supervisor" in roles]
        self.assertGreaterEqual(len(node_pids), min_nodes, msg=f"recorded PIDs={records!r}")
        self.assertGreaterEqual(len(hub_pids), min_hubs, msg=f"recorded PIDs={records!r}")
        if exact_nodes is not None:
            self.assertEqual(len(node_pids), exact_nodes, msg=f"recorded node PIDs={node_pids!r} all={records!r}")
        if exact_supervisors is not None:
            self.assertEqual(
                len(supervisor_pids),
                exact_supervisors,
                msg=f"recorded supervisor PIDs={supervisor_pids!r} all={records!r}",
            )

        alive: list[tuple[int, list[str], str]] = []
        for pid, roles in sorted(records.items()):
            ours, detail = self._is_our_recorded_process(pid)
            if ours:
                alive.append((pid, sorted(roles), detail))
        self.assertEqual(alive, [], msg=f"leftover fake rehearsal processes still alive: {alive!r}")
        return records

    def _run_with_platform(
        self,
        platform_key: str,
        rust_target: str,
        argv: list[str],
    ) -> tuple[int, str, dict]:
        orig_run = REH.run_command

        def fake_run_command(cmd, **kwargs):
            prog = Path(cmd[0]).name
            if prog in ("launchctl", "systemctl", "security"):
                self.system_calls.append(list(cmd))
                code = 113 if prog == "launchctl" and "print" in cmd else 0
                return REH.subprocess.CompletedProcess(cmd, code, stdout="", stderr="")
            return orig_run(cmd, **kwargs)

        stdout_buf = io.StringIO()
        stderr_buf = io.StringIO()
        report_path = self.root / f"report-{len(self.system_calls)}.json"
        full_argv = [*argv, "--report-out", str(report_path)]

        with (
            patch.dict(
                os.environ,
                {
                    "HOME": str(self.fake_home),
                    "STEVE_PORT": "9999",
                    "FAKE_REHEARSAL_PID_LOG": str(self.pid_log),
                },
            ),
            patch.object(REH, "detect_target", return_value=(platform_key, rust_target)),
            patch.object(REH, "run_command", side_effect=fake_run_command),
            patch("sys.stdout", stdout_buf),
            patch("sys.stderr", stderr_buf),
        ):
            code = REH.main(full_argv)
        report = json.loads(report_path.read_text(encoding="utf-8"))
        return code, stdout_buf.getvalue(), report

    def test_macos_full_rehearsal_with_broken_setup_workaround_and_rollback(self) -> None:
        prev_dir = make_fake_bundle(
            self.root / "prev-0.13.0",
            version="0.13.0",
            commit="1" * 40,
            target="aarch64-apple-darwin",
            include_cli=True,
            broken_setup=True,
            legacy_ops=True,
            legacy_policy=True,
        )
        cand_dir = make_fake_bundle(
            self.root / "cand-0.14.1",
            version="0.14.1",
            commit="2" * 40,
            target="aarch64-apple-darwin",
            include_cli=False,
            broken_setup=False,
            legacy_ops=False,
            legacy_policy=False,
        )
        work_dir = self.root / "work"
        code, stdout, report = self._run_with_platform(
            "macos",
            "aarch64-apple-darwin",
            ["--from", str(prev_dir), "--candidate", str(cand_dir), "--work-dir", str(work_dir), "--timeout", "30"],
        )
        self.assertEqual(code, 0, msg=stdout + "\n" + json.dumps(report, indent=2))
        self.assertTrue(report["passed"])
        self.assertTrue(report["cleanup_ok"])
        self.assertTrue(report["operator_untouched"])
        self.assertIn("Result: PASS", stdout)
        by_name = {s["name"]: s for s in report["steps"]}
        self.assertTrue(by_name["fresh_setup"]["details"]["workaround_applied"])
        self.assertEqual(by_name["seed_data"]["details"]["policy_api"], "approval-policy")
        self.assertEqual(by_name["verify_pass1"]["details"]["permission_mode"], "auto")
        self.assertEqual(by_name["verify_rollback"]["details"]["version"], "0.13.0")
        self.assertTrue(by_name["verify_pass2"]["details"]["permission_writes_checked"])
        self.assertEqual(by_name["confirm"]["status"], "pass")
        self.assertEqual(list(work_dir.iterdir()), [])
        self.assertEqual(list((self.fake_home / "Library/LaunchAgents").glob("*rehearsal*")), [])
        self._assert_no_recorded_processes_alive(min_nodes=4, min_hubs=5)

    def test_macos_skip_rollback_with_tarball_candidate_and_json_output(self) -> None:
        prev_dir = make_fake_bundle(
            self.root / "prev-0.14.0",
            version="0.14.0",
            commit="3" * 40,
            target="aarch64-apple-darwin",
            include_cli=True,
        )
        cand_raw = make_fake_bundle(
            self.root / "cand-0.14.1-raw",
            version="0.14.1",
            commit="4" * 40,
            target="aarch64-apple-darwin",
            include_cli=False,
        )
        cand_tgz = pack_tar_gz(cand_raw, self.root / "steve-0.14.1-aarch64-apple-darwin.tar.gz")
        work_dir = self.root / "work-tar"
        code, stdout, report = self._run_with_platform(
            "macos",
            "aarch64-apple-darwin",
            [
                "--from", str(prev_dir),
                "--candidate", str(cand_tgz),
                "--work-dir", str(work_dir),
                "--skip-rollback",
                "--json",
                "--timeout", "30",
            ],
        )
        self.assertEqual(code, 0, msg=stdout)
        parsed_stdout = json.loads(stdout)
        self.assertTrue(parsed_stdout["passed"])
        by_name = {s["name"]: s for s in parsed_stdout["steps"]}
        self.assertEqual(by_name["rollback"]["status"], "skip")
        self.assertEqual(by_name["verify_rollback"]["status"], "skip")
        self.assertTrue(by_name["verify_pass1"]["details"]["permission_writes_checked"])
        self.assertEqual(by_name["confirm"]["status"], "pass")
        self.assertEqual(list(work_dir.iterdir()), [])
        self._assert_no_recorded_processes_alive(min_nodes=2, min_hubs=2)

    def test_linux_full_rehearsal_and_confirm_skipped(self) -> None:
        prev_dir = make_fake_bundle(
            self.root / "prev-linux",
            version="0.13.0",
            commit="5" * 40,
            target="x86_64-unknown-linux-gnu",
            include_cli=True,
            broken_setup=False,
            legacy_ops=True,
            legacy_policy=True,
        )
        cand_dir = make_fake_bundle(
            self.root / "cand-linux",
            version="0.14.1",
            commit="6" * 40,
            target="x86_64-unknown-linux-gnu",
            include_cli=True,
        )
        work_dir = self.root / "work-linux"
        code, stdout, report = self._run_with_platform(
            "linux",
            "x86_64-unknown-linux-gnu",
            ["--from", str(prev_dir), "--candidate", str(cand_dir), "--work-dir", str(work_dir), "--timeout", "30"],
        )
        self.assertEqual(code, 0, msg=stdout + "\n" + json.dumps(report, indent=2))
        by_name = {s["name"]: s for s in report["steps"]}
        self.assertEqual(by_name["confirm"]["status"], "skip")
        self.assertEqual(by_name["verify_pass2"]["details"]["local_node_version"], "0.14.1")
        self.assertTrue(any("systemctl" in c[0] and "daemon-reload" in c for c in self.system_calls))
        self.assertEqual(list(work_dir.iterdir()), [])
        self._assert_no_recorded_processes_alive(exact_nodes=4, min_hubs=4, exact_supervisors=2)

    def test_failure_during_upgrade_and_verify_still_cleans_up(self) -> None:
        prev_dir = make_fake_bundle(
            self.root / "prev-fail",
            version="0.14.0",
            commit="7" * 40,
            target="aarch64-apple-darwin",
            include_cli=True,
        )
        cand_dir = make_fake_bundle(
            self.root / "cand-fail",
            version="0.14.1",
            commit="8" * 40,
            target="aarch64-apple-darwin",
            include_cli=False,
        )
        # Inject failure inside upgrade_pass1 via a wrapper hook on _step_upgrade
        work_dir = self.root / "work-fail"
        with patch.object(REH.RehearsalRunner, "_step_upgrade", side_effect=REH.RehearsalError("injected upgrade boom")):
            code, _stdout, report = self._run_with_platform(
                "macos",
                "aarch64-apple-darwin",
                ["--from", str(prev_dir), "--candidate", str(cand_dir), "--work-dir", str(work_dir), "--timeout", "30"],
            )
        self.assertEqual(code, 1)
        self.assertFalse(report["passed"])
        self.assertTrue(report["cleanup_ok"])
        self.assertTrue(report["operator_untouched"])
        self.assertIn("hub_log_tail", report.get("diagnostics", {}))
        self.assertEqual(list(work_dir.iterdir()), [])
        self.assertEqual(list((self.fake_home / "Library/LaunchAgents").glob("*rehearsal*")), [])
        self._assert_no_recorded_processes_alive(min_nodes=1, min_hubs=1)

    def test_isolation_and_tar_validation_guards(self) -> None:
        with patch.dict(os.environ, {"HOME": str(self.fake_home)}):
            with self.assertRaises(REH.RehearsalError):
                REH.validate_sandbox_paths(self.fake_home / ".steve")
            with self.assertRaises(REH.RehearsalError):
                REH.validate_sandbox_paths(self.fake_home / ".steve/sub")

        env = REH.clean_subprocess_env({"STEVE_TEST_ONLY": "1"})
        for xdg_var in ("XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_CACHE_HOME"):
            self.assertNotIn(xdg_var, REH.HARNESS_STATE_VARS)
        with patch.dict(
            os.environ,
            {
                "STEVE_HOME": "/tmp/bad",
                "CODEX_HOME": "/tmp/bad2",
                "XDG_CONFIG_HOME": "/tmp/xdg-cfg",
            },
        ):
            stripped = REH.clean_subprocess_env()
        self.assertNotIn("STEVE_HOME", stripped)
        self.assertNotIn("CODEX_HOME", stripped)
        self.assertEqual(stripped["XDG_CONFIG_HOME"], "/tmp/xdg-cfg")
        self.assertEqual(env["STEVE_TEST_ONLY"], "1")

        # Tar traversal & symlink checks
        bad_trav = self.root / "bad-trav.tar.gz"
        with tarfile.open(bad_trav, "w:gz") as tf:
            info = tarfile.TarInfo("../escape.txt")
            data = b"nope"
            info.size = len(data)
            tf.addfile(info, io.BytesIO(data))
        with self.assertRaises(REH.RehearsalError):
            REH.safe_extract_tar_gz(bad_trav, self.root / "out1")

        bad_link = self.root / "bad-link.tar.gz"
        with tarfile.open(bad_link, "w:gz") as tf:
            info = tarfile.TarInfo("bin/link")
            info.type = tarfile.SYMTYPE
            info.linkname = "/etc/passwd"
            tf.addfile(info)
        with self.assertRaises(REH.RehearsalError):
            REH.safe_extract_tar_gz(bad_link, self.root / "out2")

        # steve.env formatting and deduplication
        env_file = self.root / "steve.env"
        env_file.write_text("STEVE_PORT=19100\nSTEVE_BACKUP='on'\n", encoding="utf-8")
        added = REH.append_isolated_env(
            env_file,
            {"STEVE_BACKUP": "off", "STEVE_DISK_FLOOR_GB": "1", "CODEX_HOME": "/tmp/c"},
        )
        self.assertEqual(added, ["STEVE_DISK_FLOOR_GB", "CODEX_HOME"])
        parsed = REH.parse_env_file(env_file)
        self.assertEqual(parsed["STEVE_BACKUP"], "on")
        self.assertEqual(parsed["STEVE_DISK_FLOOR_GB"], "1")
        self.assertEqual(parsed["CODEX_HOME"], "/tmp/c")

        # Same version+commit non-legacy candidate rejected
        b1 = make_fake_bundle(
            self.root / "same1", version="0.14.1", commit="9" * 40, target="aarch64-apple-darwin"
        )
        b2 = make_fake_bundle(
            self.root / "same2", version="0.14.1", commit="9" * 40, target="aarch64-apple-darwin"
        )
        code, _stdout, report = self._run_with_platform(
            "macos",
            "aarch64-apple-darwin",
            ["--from", str(b1), "--candidate", str(b2), "--work-dir", str(self.root / "w-same")],
        )
        self.assertEqual(code, 1)
        self.assertTrue(report["cleanup_ok"])
        self.assertEqual(report["steps"][0]["status"], "fail")
        self._assert_no_recorded_processes_alive()

    def test_snapshot_operator_state_handles_permission_error(self) -> None:
        resolved_steve = (self.fake_home / ".steve").resolve()
        env_target = resolved_steve / "steve.env"
        current_target = resolved_steve / "current"
        orig_lstat = Path.lstat
        orig_readlink = os.readlink

        def fake_lstat(path_self: Path):
            if path_self in (env_target, current_target):
                raise PermissionError(1, "Operation not permitted")
            return orig_lstat(path_self)

        def fake_readlink(path):
            if Path(path) == current_target:
                raise PermissionError(1, "Operation not permitted")
            return orig_readlink(path)

        with (
            patch.dict(os.environ, {"HOME": str(self.fake_home)}),
            patch.object(
                REH,
                "run_command",
                return_value=REH.subprocess.CompletedProcess(["security"], 0, stdout='"login.keychain-db"', stderr=""),
            ),
            patch.object(Path, "lstat", fake_lstat),
            patch.object(os, "readlink", fake_readlink),
        ):
            snap = REH.snapshot_operator_state("macos")

        self.assertEqual(snap["steve_env_stat"], "<protected>")
        self.assertEqual(snap["steve_home_current"], "<protected>")

    def test_snapshot_operator_state_linux_legacy_unit(self) -> None:
        unit_dir = self.fake_home / ".config/systemd/user"
        legacy_unit = unit_dir / "steve-hub.service"
        with patch.dict(os.environ, {"HOME": str(self.fake_home)}):
            snap_absent = REH.snapshot_operator_state("linux")
            self.assertIsNone(snap_absent["legacy_linux_service_stat"])

            unit_dir.mkdir(parents=True, exist_ok=True)
            legacy_unit.write_text("[Unit]\nDescription=Legacy Steve\n", encoding="utf-8")
            snap_present = REH.snapshot_operator_state("linux")

        self.assertIsInstance(snap_present["legacy_linux_service_stat"], list)
        self.assertEqual(len(snap_present["legacy_linux_service_stat"]), 3)

    def test_legacy_non_json_404_and_invalid_200_json(self) -> None:
        class LegacyHandler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                if self.path in ("/api/permission-policy", "/api/broken-json"):
                    status = 404 if self.path == "/api/permission-policy" else 200
                    raw = b"404 page not found\n"
                    self.send_response(status)
                    self.send_header("Content-Type", "text/plain; charset=utf-8")
                    self.send_header("Content-Length", str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                    return
                if self.path == "/api/approval-policy":
                    raw = json.dumps({"policy": {"revision": 1, "mode": "ask_owner"}, "editable": True}).encode("utf-8")
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                    return
                self.send_response(404)
                self.end_headers()

            def do_PUT(self):
                length = int(self.headers.get("Content-Length", "0"))
                _ = self.rfile.read(length) if length > 0 else b"{}"
                if self.path == "/api/approval-policy":
                    raw = json.dumps({"policy": {"revision": 2, "mode": "harness_auto"}, "editable": True}).encode("utf-8")
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(raw)))
                    self.end_headers()
                    self.wfile.write(raw)
                    return
                self.send_response(404)
                self.end_headers()

            def log_message(self, *_args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), LegacyHandler)
        server.daemon_threads = True
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        runner = None
        try:
            api_base = f"http://127.0.0.1:{server.server_address[1]}"
            client = REH.HubClient(api_base, api_base)
            status, payload = client.api_json("GET", "/api/permission-policy", expected_statuses=(200, 404))
            self.assertEqual(status, 404)
            self.assertEqual(payload, {"raw": "404 page not found\n"})

            args = REH.parse_args(
                ["--from", "0.13.0", "--candidate", "0.14.1", "--work-dir", str(self.root / "probe-work")]
            )
            with patch.object(REH, "detect_target", return_value=("macos", "aarch64-apple-darwin")):
                runner = REH.RehearsalRunner(args)
            runner.client = client
            self.assertEqual(runner._probe_and_set_preupgrade_policy(), ("approval-policy", "harness_auto", "auto"))

            with self.assertRaises(REH.RehearsalError) as ctx:
                client.api_json("GET", "/api/broken-json", expected_statuses=(200,))
            self.assertIn("invalid JSON", str(ctx.exception))
        finally:
            if runner is not None and runner.sandbox.exists():
                shutil.rmtree(runner.sandbox, ignore_errors=True)
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

    def test_default_sandbox_path_fits_unix_socket_limit(self) -> None:
        self.assertEqual(REH.validate_sandbox_paths(None), Path("/tmp").resolve())
        args = REH.parse_args(["--from", "0.13.0", "--candidate", "0.14.1"])
        with patch.object(REH, "detect_target", return_value=("macos", "aarch64-apple-darwin")):
            runner = REH.RehearsalRunner(args)
        try:
            self.assertEqual(runner.sandbox.parent, Path("/tmp").resolve())
            sock_path = runner.home / REH.NODE_FILE_ACCESS_SOCKET_REL
            self.assertLessEqual(len(str(sock_path).encode("utf-8")), REH.UNIX_SOCKET_PATH_LIMIT)
        finally:
            if runner.sandbox.exists():
                shutil.rmtree(runner.sandbox, ignore_errors=True)

    def test_fake_node_self_exits_when_home_removed_or_replaced(self) -> None:
        bundle = make_fake_bundle(
            self.root / "retire-bundle",
            version="0.15.0",
            commit="a" * 40,
            target="aarch64-apple-darwin",
            include_cli=False,
        )
        home = self.root / "retire-home"
        home.mkdir(parents=True)
        env = os.environ.copy()
        env["STEVE_HOME"] = str(home)
        env["FAKE_REHEARSAL_PID_LOG"] = str(self.pid_log)
        node_exe = bundle / "bin/steve-node"

        # Case 1: deleting STEVE_HOME causes fake node to self-exit promptly.
        proc1 = subprocess.Popen(
            [str(node_exe), "--local-node"],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        try:
            self._wait_for_recorded_pid(proc1.pid, proc=proc1)
            self.assertIsNone(proc1.poll())
            shutil.rmtree(home)
            self.assertEqual(proc1.wait(timeout=5), 0)
        finally:
            if proc1.poll() is None:
                proc1.kill()
                proc1.wait(timeout=5)

        # Case 2: replacing STEVE_HOME at the same path (inode change) causes self-exit.
        home.mkdir(parents=True)
        proc2 = subprocess.Popen(
            [str(node_exe), "--local-node"],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        try:
            self._wait_for_recorded_pid(proc2.pid, proc=proc2)
            self.assertIsNone(proc2.poll())
            home.rename(self.root / "retire-home-old")
            home.mkdir(parents=True)
            self.assertEqual(proc2.wait(timeout=5), 0)
        finally:
            if proc2.poll() is None:
                proc2.kill()
                proc2.wait(timeout=5)

        # Case 3: deleting the steve-node executable causes self-exit.
        proc3 = subprocess.Popen(
            [str(node_exe), "--local-node"],
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        try:
            self._wait_for_recorded_pid(proc3.pid, proc=proc3)
            self.assertIsNone(proc3.poll())
            node_exe.unlink()
            self.assertEqual(proc3.wait(timeout=5), 0)
        finally:
            if proc3.poll() is None:
                proc3.kill()
                proc3.wait(timeout=5)

        self._assert_no_recorded_processes_alive(min_nodes=3)

    def test_fake_node_handles_sigterm_and_sigint(self) -> None:
        bundle = make_fake_bundle(
            self.root / "signal-bundle",
            version="0.15.0",
            commit="b" * 40,
            target="aarch64-apple-darwin",
            include_cli=False,
        )
        home = self.root / "signal-home"
        home.mkdir(parents=True)
        env = os.environ.copy()
        env["STEVE_HOME"] = str(home)
        env["FAKE_REHEARSAL_PID_LOG"] = str(self.pid_log)
        node_exe = bundle / "bin/steve-node"

        for sig in (signal.SIGTERM, signal.SIGINT):
            proc = subprocess.Popen(
                [str(node_exe), "--local-node"],
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                start_new_session=True,
            )
            try:
                self._wait_for_recorded_pid(proc.pid, proc=proc)
                self.assertIsNone(proc.poll())
                proc.send_signal(sig)
                self.assertEqual(proc.wait(timeout=3), 0)
            finally:
                if proc.poll() is None:
                    proc.kill()
                    proc.wait(timeout=5)

        self._assert_no_recorded_processes_alive(min_nodes=2)

    def test_process_belongs_to_home_and_stop_verified_local_node_on_fake_node(self) -> None:
        home = self.root / "verify-stop-home"
        rel = make_fake_bundle(
            home / "releases/0.14.1",
            version="0.14.1",
            commit="c" * 40,
            target="aarch64-apple-darwin",
            include_cli=False,
        )
        os.symlink(rel, home / "current")
        (home / "data/node").mkdir(parents=True)

        env = os.environ.copy()
        env["STEVE_HOME"] = str(home)
        env["FAKE_REHEARSAL_PID_LOG"] = str(self.pid_log)
        snippet = (
            "import subprocess, sys; "
            "p = subprocess.Popen([sys.argv[1], '--local-node'], "
            "stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, "
            "start_new_session=True); "
            "print(p.pid)"
        )
        launcher = subprocess.run(
            [sys.executable, "-c", snippet, str(home / "current/bin/steve-node")],
            env=env,
            capture_output=True,
            text=True,
            check=True,
        )
        pid = int(launcher.stdout.strip())
        try:
            self._wait_for_recorded_pid(pid)
            (home / "data/node/service.pid").write_text(f"{pid}\n0.14.1\n", encoding="utf-8")
            belongs = False
            detail = ""
            deadline = time.monotonic() + 2.0
            while time.monotonic() < deadline:
                belongs, detail = REH.process_belongs_to_home(pid, home, require_local_node=True)
                if belongs:
                    break
                time.sleep(0.05)
            self.assertTrue(belongs, msg=detail)
            other_belongs, _ = REH.process_belongs_to_home(
                pid, self.root / "other-home", require_local_node=True
            )
            self.assertFalse(other_belongs)
            self.assertTrue(REH.stop_verified_local_node(home))
            self.assertFalse(REH.process_is_alive(pid))
        finally:
            if REH.process_is_alive(pid):
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass

        self._assert_no_recorded_processes_alive(min_nodes=1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
