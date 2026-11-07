#!/usr/bin/env python3
"""ops/local-release.py deploy against the real Hub, in a throwaway STEVE_HOME (macOS: codesign).

    python3 ops/test-local-release.py <steve-hub built with STEVE_RELEASE_VERSION=0.9.0 STEVE_RELEASE_COMMIT=c…c (40)>

The service is this script (`__service start|stop`, through STEVE_SERVICE_CMD) instead of launchd. Cases:
  - a deploy drains the running Hub, switches, and leaves no drain behind (business answers, /healthz drain false);
  - a release that can't open the database is refused before anything stops;
  - a release whose probe fails gets the backup put back, the old version restarted, the drain lifted;
  - a switch that fails halfway (the web link) puts the links and the database back, restarts and lifts the drain;
  - only a healthy deploy prunes old release dirs and pre-release backups.
Never touches ~/.steve, launchd or port 8440.
"""
import json
import os
import pathlib
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

OPS = pathlib.Path(__file__).resolve().parent
VERSION, COMMIT, IDENT = "0.9.0", "c" * 40, "test.steve.local-release.hub"


def service(action):
    home = pathlib.Path(os.environ["STEVE_HOME"])
    pid = home / "hub.pid"
    if action == "stop":
        if pid.exists():
            p = int(pid.read_text())
            try:
                os.killpg(p, signal.SIGTERM)
                for _ in range(200):
                    os.killpg(p, 0)
                    time.sleep(0.1)
                os.killpg(p, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):  # gone (macOS: EPERM for an exited group)
                pass
            pid.unlink()
        return
    env = dict(os.environ)
    for line in (home / "steve.env").read_text().splitlines():
        k, v = line.split("=", 1)
        env[k] = v
    env["STEVE_WEB_DIR"] = str(home / "web/current")
    with (home / "hub.log").open("a") as log:
        p = subprocess.Popen([str(home / "current/bin/steve-hub")], env=env, stdout=log, stderr=log,
                             cwd=home / "current", start_new_session=True)
    pid.write_text(str(p.pid))


if len(sys.argv) == 3 and sys.argv[1] == "__service":
    service(sys.argv[2])
    sys.exit(0)

binary = pathlib.Path(sys.argv[1]).resolve(strict=True)
info = json.loads(subprocess.check_output([str(binary), "--build-info"]))
assert (info["version"], info["commit"]) == (VERSION, COMMIT), f"build the Hub with {VERSION} {COMMIT}: {info}"
passed = 0


def ok(what):
    global passed
    passed += 1
    print(f"  ✓ {what}")


with tempfile.TemporaryDirectory(prefix="steve-local-release-") as tmp:
    root = pathlib.Path(tmp)
    home = root / "home"
    (home / "data").mkdir(parents=True)
    (home / "signing").mkdir()
    keys = root / "keys"
    keys.mkdir(mode=0o700)
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        port = s.getsockname()[1]
    assert port != 8440
    (home / "steve.env").write_text("\n".join([
        f"STEVE_PORT={port}", "STEVE_HOST=127.0.0.1", f"STEVE_PUBLIC_URL=http://127.0.0.1:{port}",
        "STEVE_VAULT_KEYSTORE=dir", f"STEVE_VAULT_KEY_DIR={keys}", "STEVE_NO_RESUME=1", "STEVE_NODE_MODE=off", "STEVE_DEV_LOGIN=1",
        "STEVE_ADAPTER_CLAUDE=/usr/bin/false", "STEVE_ADAPTER_CODEX=/usr/bin/false", f"CODEX_HOME={root / 'codex'}",
        f"STEVE_REPO={root / 'repo'}", "STEVE_FORBID_REAL_HOSTS=1"]) + "\n")
    (home / "signing/identity.json").write_text(json.dumps(dict(
        label="test.unused", hub_identifier=IDENT, keyhelper_identifier="test.unused.keyhelper", vault_service="test.unused")))
    schema = info["schema"]

    def bundle(name, target=schema, web_version=VERSION):
        d = home / "releases" / name
        (d / "bin").mkdir(parents=True)
        (d / "web/dist/assets").mkdir(parents=True)
        shutil.copy2(binary, d / "bin/steve-hub")
        subprocess.run(["codesign", "-f", "-s", "-", "-i", IDENT, str(d / "bin/steve-hub")], check=True,
                       capture_output=True)
        (d / "release.json").write_text(json.dumps(dict(
            format_version=1, version=VERSION, commit=COMMIT, hub_version=VERSION, web_version=VERSION, api_protocol=1,
            schema=dict(min_read=target, max_read=target, target=target), assets=[])))
        (d / "web/dist/release.json").write_text(json.dumps({"version": web_version}))
        (d / "web/dist/index.html").write_text(
            f'<!doctype html><html><head><meta name="steve-web-version" content="{web_version}"></head></html>')
        return d

    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("STEVE_", "LARK_", "FEISHU_", "GITHUB_", "CLAUDE_"))}
    env.update(STEVE_HOME=str(home), STEVE_PORT=str(port),
               STEVE_SERVICE_CMD=f"{sys.executable} {pathlib.Path(__file__).resolve()} __service")
    os.environ.update(env)

    def health():
        try:
            with urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1) as r:
                return json.load(r)
        except (OSError, ValueError):
            return None

    def wait(fn, what, seconds=60):
        until = time.monotonic() + seconds
        while time.monotonic() < until:
            v = fn()
            if v:
                return v
            time.sleep(0.2)
        raise AssertionError(f"timeout: {what}\n{(home / 'hub.log').read_text()[-4000:]}")

    def business():
        """/api/state answers (401 without a login) instead of the drain's 503."""
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/api/state", timeout=2)
            return 200
        except urllib.error.HTTPError as e:
            return e.code

    def deploy(release):
        return subprocess.run([sys.executable, str(OPS / "local-release.py"), "deploy", str(release), "--drain-s", "60"],
                              env=os.environ, capture_output=True, text=True)

    def pid():
        return (home / "hub.pid").read_text()

    a = bundle("0.9.0-a")
    (home / "current").symlink_to(a)
    (home / "web").mkdir()
    (home / "web/current").symlink_to(a / "web/dist")
    try:
        service("start")
        wait(lambda: (health() or {}).get("drain") is False, "first Hub up")

        print("── deploy b over a")
        b = bundle("0.9.0-b")
        for n in range(5):
            (home / "releases" / f"0.1.{n}-local-000000{n}").mkdir()
        (home / "backups").mkdir(exist_ok=True)
        for n in range(10):
            (home / "backups" / f"pre-0.1.{n}-local-000000{n}-202611{n + 1:02d}-120000.sqlite3").write_text("x")
        r = deploy(b)
        assert r.returncode == 0, r.stdout + r.stderr
        assert (home / "current").resolve() == b.resolve()
        h = health()
        assert h["drain"] is False and h["build_commit"] == COMMIT, h
        ok("switched to b, /healthz says the drain is off")
        assert business() != 503, business()
        ok("business answers (not the drain's 503)")
        res = json.loads(sorted((home / "logs/release").glob("local-*.json"))[-1].read_text())
        assert res["ok"] and pathlib.Path(res["backup"]).exists(), res
        with sqlite3.connect(res["backup"]) as c:
            assert c.execute("PRAGMA quick_check").fetchone()[0] == "ok"
        ok("result file: ok, with a checked backup")
        assert sorted(p.name for p in (home / "releases").glob("0.1.*")) == [
            "0.1.2-local-0000002", "0.1.3-local-0000003", "0.1.4-local-0000004"], list((home / "releases").iterdir())
        assert a.is_dir() and b.is_dir()
        assert len(list((home / "backups").glob("pre-0.1.*"))) == 8 and pathlib.Path(res["backup"]).exists()
        assert len(res["pruned"]) == 2 + 2, res["pruned"]
        ok("a healthy deploy pruned the old release dirs and backups, kept a, b and its own backup")
        stale = home / "releases/0.0.1-local-0000001"
        stale.mkdir()

        print("── a release that can't open the database: refused, nothing stops")
        before = pid()
        r = deploy(bundle("0.8.0-old", target=schema - 1))
        assert r.returncode == 1 and "需要至少" in r.stdout, r.stdout + r.stderr
        assert (home / "current").resolve() == b.resolve() and pid() == before
        assert health()["drain"] is False
        ok("refused by the gate, same Hub process, no drain")

        print("── a probe that fails: the backup is back, the old version runs, no drain")
        with sqlite3.connect(home / "data/steve.sqlite3") as c:
            c.execute("CREATE TABLE IF NOT EXISTS local_release_evidence (v TEXT)")
            c.execute("INSERT INTO local_release_evidence VALUES ('kept')")
        r = deploy(bundle("0.9.0-badweb", web_version="0.0.1"))
        assert r.returncode == 1, r.stdout + r.stderr
        assert (home / "current").resolve() == b.resolve()
        wait(lambda: (health() or {}).get("drain") is False, "old Hub back without the drain")
        assert business() != 503
        with sqlite3.connect(home / "data/steve.sqlite3") as c:
            assert c.execute("SELECT v FROM local_release_evidence").fetchone()[0] == "kept"
        assert list((home / "backups").glob("failed-probe-0.9.0-badweb-*"))
        ok("probe failed: b runs again on the restored database, drain lifted, the probed database kept apart")

        print("── the web link fails after the probe and the Hub link: everything goes back")
        before = pid()
        (home / "web").chmod(0o555)  # web/current can't be replaced
        try:
            r = deploy(bundle("0.9.0-c"))
        finally:
            (home / "web").chmod(0o755)
        assert r.returncode == 1, r.stdout + r.stderr
        assert (home / "current").resolve() == b.resolve(), (home / "current").resolve()
        assert (home / "web/current").resolve() == (b / "web/dist").resolve()
        h = wait(lambda: (health() or {}).get("drain") is False and health(), "b back without the drain")
        assert business() != 503 and pid() != before
        res = json.loads(sorted((home / "logs/release").glob("local-*.json"))[-1].read_text())
        assert res["recovered"] is True and res["phase"] == "switch", res
        assert any("current back" in x for x in res["recovery"]) and any("backup is back" in x for x in res["recovery"]), res
        ok("current and the database put back, b restarted, drain lifted, recorded as recovered")
        assert stale.is_dir() and len(list((home / "backups").glob("pre-0.1.*"))) == 8
        ok("deploys that did not end healthy pruned nothing")
    except BaseException:
        print((home / "hub.log").read_text()[-6000:], file=sys.stderr)
        raise
    finally:
        service("stop")
print(f"all {passed} checks passed")
