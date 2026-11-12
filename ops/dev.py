#!/usr/bin/env python3
"""Run this checkout as a development Hub next to the installed one: build, restart, try it in the browser.

    dev.py up [--fresh | --snapshot [--from HOME]]   build the Hub, start it and the web dev server
    dev.py restart                                   rebuild the Hub and restart it (the web reloads by itself)
    dev.py stop                                      stop the Hub, its Node and the web dev server
    dev.py status                                    what runs, where, and on which data
    dev.py logs                                      follow the Hub's log

Data, in `STEVE_DEV_HOME` (default ~/.steve-dev), stays across `up` and `restart` until a mode is given:
  --fresh     an empty database: the whole Hub works, sessions included, as on a new installation.
  --snapshot  a consistent copy of an installed Hub's database (`--from`, default ~/.steve) and its files, opened
              as a database copy (STEVE_NO_RESUME): nothing resumes, no remote Node is accepted, no backup runs, and
              its vault stays locked (the keys are in the installed Hub's key store). For looking at real data;
              new sessions do not run there. The copy's owner signs in with the one-click development login.

The Hub listens on 127.0.0.1:18440 (its own Node on 18443) and the web dev server on 127.0.0.1:18441, which proxies
/api to it; `STEVE_DEV_PORT` and `STEVE_DEV_WEB_PORT` change them. Web edits show at once (Vite reloads); Rust edits
need `restart`, an incremental debug build. The Hub's environment is built here, not inherited: `STEVE_*` variables
of the calling shell are dropped, and `<dev home>/steve.env` (KEY=value lines) adds or overrides settings.
Harness state (CLAUDE_CONFIG_DIR, CODEX_HOME) is in `<dev home>/harness`, seeded once with your own settings files
(never credentials) and kept by --fresh; sign in there with the harness's own command if it asks.
"""

import argparse
import os
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
HOME = Path.home()
DEV = Path(os.environ.get("STEVE_DEV_HOME") or HOME / ".steve-dev").expanduser()
PORT = int(os.environ.get("STEVE_DEV_PORT") or 18440)
WEB_PORT = int(os.environ.get("STEVE_DEV_WEB_PORT") or 18441)
TARGET = Path(os.environ.get("CARGO_TARGET_DIR") or REPO / "hub-rs/target")
HUB_BIN = TARGET / "debug/steve-hub"
RUN = DEV / "run"
MODE = DEV / "mode"
HARNESS = DEV / "harness"


def say(msg):
    print(f"dev: {msg}", flush=True)


def die(msg):
    print(f"dev: {msg}", file=sys.stderr)
    sys.exit(1)


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def read_pid(path, expect):
    """The pid in `path` when that process still runs and its command line contains `expect`."""
    try:
        pid = int(path.read_text().strip())
    except (OSError, ValueError):
        return None
    if not alive(pid):
        return None
    cmd = subprocess.run(["ps", "-o", "command=", "-p", str(pid)], capture_output=True, text=True).stdout
    return pid if expect in cmd else None


def port_busy(port):
    with socket.socket() as s:
        return s.connect_ex(("127.0.0.1", port)) == 0


def stop_pid(path, expect, name):
    pid = read_pid(path, expect)
    if pid is None:
        path.unlink(missing_ok=True)
        return
    # each was started in a session of its own: the group takes its children (npm's vite, the Hub's adapters)
    try:
        os.killpg(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    for _ in range(100):
        if not alive(pid):
            break
        time.sleep(0.1)
    else:
        os.killpg(pid, signal.SIGKILL)
    path.unlink(missing_ok=True)
    say(f"stopped {name} ({pid})")


def stop_hub():
    stop_pid(RUN / "hub.pid", "steve-hub", "Hub")
    # the Hub's own Node runs in a process of its own and outlives a Hub restart; a new build replaces it
    stop_pid(DEV / "data/node/service.pid", "steve-hub", "the Hub's Node")


def stop_web():
    stop_pid(RUN / "web.pid", "npm", "web dev server")


def build():
    jobs = os.environ.get("CARGO_BUILD_JOBS") or str(min(6, max(1, (os.cpu_count() or 2) // 2)))
    say(f"building steve-hub (debug, -j {jobs})")
    t = time.monotonic()
    cmd = ["nice", "cargo", "build", "-p", "steve-hub", "-j", jobs]
    if subprocess.run(cmd, cwd=REPO / "hub-rs").returncode != 0:
        die("build failed; the running Hub was left as it was")
    say(f"built in {time.monotonic() - t:.0f}s")


def clear_data():
    for d in ("data", "worktrees", "scratch", "backups", "vault-keys"):
        shutil.rmtree(DEV / d, ignore_errors=True)


def fresh():
    clear_data()
    MODE.write_text("fresh\n")
    say(f"empty database in {DEV}")


def snapshot(src_home):
    src = src_home / "data/steve.sqlite3"
    if not src.is_file():
        die(f"no database at {src}")
    clear_data()
    data = DEV / "data"
    data.mkdir(parents=True)
    dst = data / "steve.sqlite3"
    # SQLite's online backup: a consistent copy while the installed Hub keeps writing
    with sqlite3.connect(f"file:{src}?mode=ro", uri=True) as s, sqlite3.connect(dst) as d:
        s.backup(d)
        # the copy's owner signs in with the one-click development login
        d.execute(
            "UPDATE users SET subject='dev:local' WHERE id=(SELECT id FROM users WHERE role='owner' ORDER BY created LIMIT 1)"
        )
    if (src_home / "data/files").is_dir():
        shutil.copytree(src_home / "data/files", data / "files")
    MODE.write_text(f"snapshot {src_home}\n")
    say(f"copied {src} ({dst.stat().st_size // 2**20} MB) into {DEV}")


def seed_harness():
    for name, own, files in (
        ("claude", HOME / ".claude", ["settings.json"]),
        ("codex", Path(os.environ.get("CODEX_HOME") or HOME / ".codex"), ["config.toml"]),
    ):
        d = HARNESS / name
        if d.exists():
            continue
        d.mkdir(parents=True)
        for f in files:
            if (own / f).is_file():
                shutil.copy2(own / f, d / f)


def dev_env_file():
    out = {}
    p = DEV / "steve.env"
    if p.is_file():
        for line in p.read_text().splitlines():
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                out[k.strip()] = v.strip().strip("'\"")
    return out


def hub_env(mode):
    env = {k: v for k, v in os.environ.items() if not k.startswith("STEVE_")}
    for k in ("CLAUDE_CONFIG_DIR", "CODEX_HOME"):
        env.pop(k, None)
    sha = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=REPO, capture_output=True, text=True).stdout
    web = f"http://127.0.0.1:{WEB_PORT}"
    env.update(
        STEVE_HOME=str(DEV),
        STEVE_HOST="127.0.0.1",
        STEVE_PORT=str(PORT),
        STEVE_PUBLIC_URL=web,
        STEVE_PUBLIC_URLS=f"{web},http://localhost:{WEB_PORT},http://127.0.0.1:{PORT}",
        STEVE_DEV_LOGIN="1",
        STEVE_VAULT_KEYSTORE="file",
        STEVE_BACKUP="off",
        STEVE_REPO=str(REPO),
        STEVE_WEB_DIR=str(REPO / "web/dist"),
        STEVE_VERSION=f"dev-{sha.strip() or 'unknown'}",
        CLAUDE_CONFIG_DIR=str(HARNESS / "claude"),
        CODEX_HOME=str(HARNESS / "codex"),
        NO_COLOR="1",
    )
    if shutil.which("node"):
        env["STEVE_NODE"] = shutil.which("node")
    if mode.startswith("snapshot"):
        env["STEVE_NO_RESUME"] = "1"
    env.update(dev_env_file())
    return env


def link_adapters():
    # the installed Hub's ACP adapters, when there are any: a development Hub runs the same harnesses
    own = DEV / "adapters"
    installed = HOME / ".steve/adapters"
    if not own.exists() and not own.is_symlink() and installed.is_dir():
        own.symlink_to(installed)


def start_hub():
    if not HUB_BIN.is_file():
        die(f"{HUB_BIN} is missing; run `dev.py up`")
    if port_busy(PORT):
        die(f"127.0.0.1:{PORT} is taken by something else; set STEVE_DEV_PORT")
    mode = MODE.read_text().strip() if MODE.is_file() else "fresh"
    RUN.mkdir(parents=True, exist_ok=True)
    seed_harness()
    link_adapters()
    log = open(DEV / "hub.log", "ab")
    p = subprocess.Popen(
        [str(HUB_BIN)], cwd=DEV, env=hub_env(mode), stdout=log, stderr=log, stdin=subprocess.DEVNULL,
        start_new_session=True,
    )
    (RUN / "hub.pid").write_text(f"{p.pid}\n")
    for _ in range(120):
        if p.poll() is not None:
            tail = (DEV / "hub.log").read_text(errors="replace").splitlines()[-20:]
            die("the Hub exited at start:\n" + "\n".join(tail))
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{PORT}/healthz", timeout=1)
            say(f"Hub up on 127.0.0.1:{PORT} ({mode})")
            return
        except OSError:
            time.sleep(0.5)
    die(f"the Hub did not answer /healthz in 60s; see {DEV / 'hub.log'}")


def start_web():
    if read_pid(RUN / "web.pid", "npm"):
        # the dev server of another checkout serves that checkout's web: replace it
        if (RUN / "web.repo").is_file() and (RUN / "web.repo").read_text().strip() == str(REPO):
            return
        stop_web()
    if port_busy(WEB_PORT):
        die(f"127.0.0.1:{WEB_PORT} is taken by something else; set STEVE_DEV_WEB_PORT")
    if not (REPO / "node_modules").is_dir():
        say("installing web dependencies (npm ci)")
        if subprocess.run(["npm", "ci"], cwd=REPO).returncode != 0:
            die("npm ci failed")
    env = dict(os.environ, STEVE_HUB=f"http://127.0.0.1:{PORT}")
    log = open(DEV / "web.log", "ab")
    p = subprocess.Popen(
        ["npm", "-w", "web", "run", "dev", "--", "--host", "127.0.0.1", "--port", str(WEB_PORT), "--strictPort"],
        cwd=REPO, env=env, stdout=log, stderr=log, stdin=subprocess.DEVNULL, start_new_session=True,
    )
    (RUN / "web.pid").write_text(f"{p.pid}\n")
    (RUN / "web.repo").write_text(f"{REPO}\n")
    for _ in range(60):
        if p.poll() is not None:
            die(f"the web dev server exited; see {DEV / 'web.log'}")
        if port_busy(WEB_PORT):
            return
        time.sleep(0.5)
    die(f"the web dev server did not listen in 30s; see {DEV / 'web.log'}")


def show_url():
    say(f"open http://127.0.0.1:{WEB_PORT}/auth/dev  (one-click sign-in, then the app)")


def cmd_up(a):
    DEV.mkdir(parents=True, exist_ok=True)
    if not a.no_build:
        build()
    stop_hub()
    if a.fresh:
        fresh()
    elif a.snapshot:
        snapshot(Path(a.src).expanduser())
    elif not MODE.is_file():
        fresh()
    start_hub()
    start_web()
    show_url()


def cmd_restart(a):
    if not a.no_build:
        build()
    stop_hub()
    start_hub()
    start_web()
    show_url()


def cmd_stop(_):
    stop_web()
    stop_hub()


def cmd_status(_):
    mode = MODE.read_text().strip() if MODE.is_file() else "none yet"
    hub = read_pid(RUN / "hub.pid", "steve-hub")
    web = read_pid(RUN / "web.pid", "npm")
    print(f"home    {DEV}\ndata    {mode}")
    print(f"hub     {'running (' + str(hub) + ') on 127.0.0.1:' + str(PORT) if hub else 'stopped'}")
    print(f"web     {'running (' + str(web) + ') on http://127.0.0.1:' + str(WEB_PORT) if web else 'stopped'}")
    print(f"build   {HUB_BIN}")


def cmd_logs(_):
    os.execvp("tail", ["tail", "-n", "100", "-F", str(DEV / "hub.log")])


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    up = sub.add_parser("up", help="build, then start the Hub and the web dev server")
    g = up.add_mutually_exclusive_group()
    g.add_argument("--fresh", action="store_true", help="start from an empty database")
    g.add_argument("--snapshot", action="store_true", help="start from a copy of an installed Hub's database")
    up.add_argument("--from", dest="src", default=str(HOME / ".steve"), help="the installed Hub's home (--snapshot)")
    up.add_argument("--no-build", action="store_true", help="run the last build")
    rs = sub.add_parser("restart", help="rebuild the Hub and restart it")
    rs.add_argument("--no-build", action="store_true", help="restart the last build")
    sub.add_parser("stop")
    sub.add_parser("status")
    sub.add_parser("logs")
    a = ap.parse_args()
    if a.cmd == "up" and not a.snapshot and a.src != str(HOME / ".steve"):
        ap.error("--from goes with --snapshot")
    {"up": cmd_up, "restart": cmd_restart, "stop": cmd_stop, "status": cmd_status, "logs": cmd_logs}[a.cmd](a)


if __name__ == "__main__":
    main()
