#!/usr/bin/env python3
"""Build a signed Steve release from a tagged commit and switch this machine's Steve to it.

    local-release.py build <commit> [--version X.Y.Z]   build $STEVE_HOME/releases/<version>-local-<sha7>
    local-release.py deploy <release dir>               switch the running Steve to it (newer or older)

build: the version is the vX.Y.Z tag on the commit unless given. The Hub is built with cargo (STEVE_CARGO, else
cargo on PATH) into $STEVE_HOME/build/cargo-target, signed with the "Steve Hub" identity in
$STEVE_HOME/signing (no keychain: the build fails, an ad-hoc Hub can't read the vault), its --build-info must
name the version and commit, and the Web is built and packaged the way CI does (scripts/package-release.py).

deploy, in order; any failure before the switch leaves the running version as it was:
  1. the release can open the database (ops/schema_gate.py): an older release on a database a newer one migrated
     is refused here, so this is also the only way back to an earlier release;
  2. drain: the Hub's own maintenance drain (/internal/upgrade/drain) until it reports ready: no new work, running
     turns finished, adapters stopped. Not ready in --drain-s seconds: the drain is lifted and nothing changes;
  3. stop the service, back up the database (quick_check must pass) into $STEVE_HOME/backups;
  4. probe: the release's Hub opens the live database in maintenance (migrations, vault read), business off. A
     failure puts the backup back (nothing external happened: everything was drained and the probe ran in
     maintenance) and restarts the old version;
  5. switch `current` and `web/current`, start the service, lift maintenance, wait for /healthz to show the version.
     A failure after the switch is not undone: the database may be migrated, the new version stays and the
     result says so (exit 3), with the backup named.

Identities come from $STEVE_HOME/signing/identity.json, written once per machine:
  {"label": "<launchd label>", "hub_identifier": "...", "keyhelper_identifier": "...", "vault_service": "..."}
and are checked against the installed Hub and steve-keyhelper signatures before anything is built or switched.
Results: $STEVE_HOME/logs/release/local-<version>-<time>.json.
"""
import argparse
import hashlib
import json
import os
import pathlib
import shutil
import socket
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import uuid

OPS = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(OPS))
import schema_gate  # noqa: E402

HOME = pathlib.Path(os.environ.get("STEVE_HOME", pathlib.Path.home() / ".steve"))
DB = HOME / "data/steve.sqlite3"
PLIST_DIR = pathlib.Path.home() / "Library/LaunchAgents"


class Refused(Exception):
    """Stopped before anything changed for the running Steve."""


def log(msg):
    print(f"{time.strftime('%H:%M:%S')} {msg}", flush=True)


def env_file():
    """steve.env as a dict (KEY=VALUE lines)."""
    out = {}
    p = HOME / "steve.env"
    if p.exists():
        for line in p.read_text().splitlines():
            if "=" in line and not line.lstrip().startswith("#"):
                k, v = line.split("=", 1)
                out[k.strip()] = v.strip().strip("'\"")
    return out


def port():
    return int(os.environ.get("STEVE_PORT") or env_file().get("STEVE_PORT") or 8440)


def signed_identifier(path):
    out = subprocess.run(["codesign", "-dv", str(path)], capture_output=True, text=True).stderr
    return next((l.split("=", 1)[1] for l in out.splitlines() if l.startswith("Identifier=")), None)


def identity():
    p = HOME / "signing/identity.json"
    if not p.exists():
        raise Refused(f"{p} is missing: write the launchd label and the signing identities once (see --help)")
    ident = json.loads(p.read_text())
    for k in ("label", "hub_identifier", "keyhelper_identifier", "vault_service"):
        if not ident.get(k):
            raise Refused(f"{p} has no {k}")
    helper = HOME / "bin/steve-keyhelper"
    if helper.exists() and signed_identifier(helper) != ident["keyhelper_identifier"]:
        raise Refused(f"{helper} is signed as {signed_identifier(helper)}, identity.json says {ident['keyhelper_identifier']}")
    hub = HOME / "current/bin/steve-hub"
    if hub.exists() and signed_identifier(hub) != ident["hub_identifier"]:
        raise Refused(f"{hub} is signed as {signed_identifier(hub)}, identity.json says {ident['hub_identifier']}")
    return ident


def build_info(binary):
    return json.loads(subprocess.check_output([str(binary), "--build-info"], timeout=15))


def sha256(path):
    return hashlib.sha256(pathlib.Path(path).read_bytes()).hexdigest()


# ── build ──

def build(args):
    ident = identity()
    repo = pathlib.Path(args.repo).resolve()
    git = lambda *a: subprocess.check_output(["git", "-C", str(repo), *a], text=True).strip()
    commit = git("rev-parse", f"{args.commit}^{{commit}}")
    version = args.version
    if not version:
        tags = [t for t in git("tag", "--points-at", commit).split() if t.startswith("v")]
        if len(tags) != 1:
            raise Refused(f"{commit[:7]} has tags {tags or 'none'}: tag it vX.Y.Z or pass --version")
        version = tags[0][1:]
    candidate = HOME / "releases" / f"{version}-local-{commit[:7]}"
    if candidate.exists():
        raise Refused(f"{candidate} exists: inspect it rather than overwrite")
    keychain = HOME / "signing/steve-signing.keychain-db"
    if not keychain.exists() or not (keychain.parent / "keychain-pass").exists():
        raise Refused(f"no signing keychain at {keychain} (ops/sign-setup.sh): an ad-hoc signed Hub can't read the vault")
    log(f"build {version} from {commit} into {candidate}")
    candidate.mkdir(parents=True)
    try:
        with tempfile.TemporaryDirectory() as tmp:
            archive = pathlib.Path(tmp) / "source.tar"
            with archive.open("wb") as f:
                subprocess.run(["git", "-C", str(repo), "archive", commit], stdout=f, check=True)
            with tarfile.open(archive) as t:
                t.extractall(candidate, filter="data")
        target_dir = HOME / "build/cargo-target"
        env = dict(os.environ, STEVE_RELEASE_VERSION=version, STEVE_RELEASE_COMMIT=commit,
                   STEVE_KEYHELPER_SERVICE=ident["vault_service"], STEVE_KEYHELPER_HUB_IDENTIFIER=ident["hub_identifier"],
                   STEVE_KEYHELPER_IDENTIFIER=ident["keyhelper_identifier"], CARGO_TARGET_DIR=str(target_dir))
        cargo = os.environ.get("STEVE_CARGO") or shutil.which("cargo")
        if not cargo:
            raise Refused("no cargo (set STEVE_CARGO)")
        log(f"cargo: {cargo}")
        subprocess.run([cargo, "build", "--locked", "--release", "--manifest-path", str(candidate / "hub-rs/Cargo.toml"),
                        "-p", "steve-hub"], env=env, check=True)
        binary = candidate / "bin/steve-hub"
        binary.parent.mkdir(exist_ok=True)
        shutil.copy2(target_dir / "release/steve-hub", binary)
        password = (keychain.parent / "keychain-pass").read_text().strip()
        unlocked = subprocess.run(["security", "unlock-keychain", "-p", password, str(keychain)], capture_output=True)
        del password
        if unlocked.returncode != 0:
            raise Refused("the signing keychain did not unlock")
        subprocess.run(["codesign", "--force", "--sign", "Steve Hub", "--identifier", ident["hub_identifier"],
                        "--keychain", str(keychain), str(binary)], check=True)
        subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)
        info = build_info(binary)
        if info.get("version") != version or info.get("commit") != commit:
            raise Refused(f"--build-info says {info}, expected {version} {commit}")
        log(f"hub: {info}")
        subprocess.run(["npm", "ci", "-w", "web", "--include-workspace-root", "--no-audit", "--no-fund"], cwd=candidate, check=True)
        subprocess.run(["npm", "-w", "web", "run", "build"], cwd=candidate, check=True)
        with tempfile.TemporaryDirectory() as out:
            subprocess.run([sys.executable, str(candidate / "scripts/package-release.py"), "package", "--version", version,
                            "--commit", commit, "--target", info["target"], "--binary", str(binary),
                            "--web", str(candidate / "web/dist"), "--out", out], check=True)
            with tarfile.open(pathlib.Path(out) / f"steve-{version}-{info['target']}.tar.gz") as t:
                for m in t.getmembers():
                    if m.name == "release.json" or m.name.startswith("web/"):
                        t.extract(m, candidate, filter="data")
        for nm in (candidate / "node_modules", candidate / "web/node_modules"):
            shutil.rmtree(nm, ignore_errors=True)
    except BaseException:
        log(f"build failed: {candidate} is left for inspection")
        raise
    print(json.dumps({"candidate": str(candidate), "build": info}))
    return 0


# ── deploy ──

def control(method):
    """The Hub's maintenance drain (loopback, control token)."""
    token_file = HOME / "updates/control-token.json"
    if not token_file.exists():
        token_file.parent.mkdir(parents=True, exist_ok=True)
        tmp = token_file.with_suffix(".tmp")
        tmp.write_text(json.dumps(uuid.uuid4().hex + uuid.uuid4().hex))
        os.chmod(tmp, 0o600)
        os.replace(tmp, token_file)
    req = urllib.request.Request(f"http://127.0.0.1:{port()}/internal/upgrade/drain", method=method,
                                 headers={"authorization": f"Bearer {json.loads(token_file.read_text())}"})
    with urllib.request.urlopen(req, timeout=10) as r:
        return json.load(r)


def health():
    try:
        with urllib.request.urlopen(f"http://127.0.0.1:{port()}/healthz", timeout=2) as r:
            return json.load(r)
    except (OSError, ValueError):
        return None


def wait_health(ok, seconds):
    until = time.monotonic() + seconds
    while time.monotonic() < until:
        h = health()
        if h and ok(h):
            return h
        time.sleep(0.5)
    return None


def launchd(action, ident):
    """Start or stop the service: launchd, or STEVE_SERVICE_CMD <start|stop> (ops/test-local-release.py)."""
    domain = f"gui/{os.getuid()}"
    plist = PLIST_DIR / f"{ident['label']}.plist"
    custom = os.environ.get("STEVE_SERVICE_CMD")
    if action == "stop":
        if custom:
            subprocess.run([*custom.split(), "stop"], check=True)
        else:
            subprocess.run(["launchctl", "bootout", f"{domain}/{ident['label']}"], check=True)
        for _ in range(150):
            with socket.socket() as s:
                if s.connect_ex(("127.0.0.1", port())) != 0:
                    return
            time.sleep(0.1)
        raise RuntimeError("the old Hub still listens after bootout")
    if custom:
        subprocess.run([*custom.split(), "start"], check=True)
    else:
        subprocess.run(["launchctl", "bootstrap", domain, str(plist)], check=True)


def link(path, target):
    tmp = path.with_name(path.name + ".deploy-new")
    if tmp.is_symlink() or tmp.exists():
        tmp.unlink()
    tmp.symlink_to(target)
    os.replace(tmp, path)


def probe(candidate, version):
    """The candidate's Hub on the live database in maintenance: migrations, vault read; business must be off."""
    ident = str(uuid.uuid4())
    ops = HOME / "updates/operations"
    ops.mkdir(parents=True, exist_ok=True)
    op = ops / f"{ident}.json"
    op.write_text(json.dumps(dict(id=ident, idempotency_key=f"local-release-probe-{version}", version=version,
                                  phase="verifying", created_at=0, updated_at=0, requested_by="local-release",
                                  error=None, previous=None, target=str(candidate))))
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        p_port = s.getsockname()[1]
    # the production environment (what run.sh gives the Hub), then only what keeps the probe private: its own
    # port, the upgrade gate, no resumed work, no agent that could start
    env = {k: os.environ[k] for k in ("HOME", "PATH", "TMPDIR") if k in os.environ}
    env.update(env_file())
    env.update(STEVE_HOME=str(HOME), STEVE_PORT=str(p_port), STEVE_HOST="127.0.0.1", STEVE_WEB_DIR=str(candidate / "web/dist"),
               STEVE_UPGRADE_ID=ident, STEVE_NO_RESUME="1", STEVE_REPO=str(HOME / "probe-unused-repo"),
               STEVE_ADAPTER_CLAUDE="/usr/bin/false", STEVE_ADAPTER_CODEX="/usr/bin/false",
               CLAUDE_CODE_EXECUTABLE="/usr/bin/false")
    env.setdefault("STEVE_KEYHELPER", str(HOME / "bin/steve-keyhelper"))
    logf = HOME / f"logs/release/probe-{version}-{ident[:8]}.log"
    logf.parent.mkdir(parents=True, exist_ok=True)
    with logf.open("w") as out:
        p = subprocess.Popen([str(candidate / "bin/steve-hub")], env=env, stdout=out, stderr=out)
        try:
            until = time.monotonic() + 60
            while time.monotonic() < until:
                if p.poll() is not None:
                    raise RuntimeError(f"the candidate exited during the probe (log {logf})")
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{p_port}/healthz", timeout=2) as r:
                        h = json.load(r)
                    if h.get("maintenance"):
                        if not h.get("version") == h.get("web_version") == version:
                            raise RuntimeError(f"probe health names {h.get('version')}/{h.get('web_version')}")
                        try:
                            urllib.request.urlopen(f"http://127.0.0.1:{p_port}/api/state", timeout=2)
                            raise RuntimeError("business reachable during the probe")
                        except urllib.error.HTTPError as e:
                            if e.code != 503:
                                raise RuntimeError(f"/api/state answered {e.code} during the probe")
                        return h
                except OSError:
                    pass
                time.sleep(0.2)
            raise RuntimeError(f"the candidate did not come up in maintenance (log {logf})")
        finally:
            p.terminate()
            try:
                p.wait(timeout=10)
            except subprocess.TimeoutExpired:
                p.kill()
                p.wait()
            op.unlink(missing_ok=True)


def deploy(args):
    ident = identity()
    candidate = pathlib.Path(args.release).resolve()
    manifest = json.loads((candidate / "release.json").read_text())
    version = manifest["version"]
    binary = candidate / "bin/steve-hub"
    subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)
    if signed_identifier(binary) != ident["hub_identifier"]:
        raise Refused(f"{binary} is signed as {signed_identifier(binary)}, not {ident['hub_identifier']}")
    info = build_info(binary)
    if (info["version"], info["commit"]) != (version, manifest["commit"]):
        raise Refused(f"--build-info {info} does not match release.json")
    if not (candidate / "web/dist/index.html").exists():
        raise Refused("the release has no web build")
    old_hub, old_web = (HOME / "current").resolve(), (HOME / "web/current").resolve()
    if old_hub == candidate:
        raise Refused(f"{candidate.name} is already current")
    why = schema_gate.check(DB, candidate)
    if why:
        raise Refused(f"不能切到 {candidate.name}：{why}")
    stamp = time.strftime("%Y%m%d-%H%M%S")
    result = dict(version=version, release=str(candidate), previous=str(old_hub), previous_web=str(old_web),
                  db_before=schema_gate.state(DB), started=stamp)
    out = HOME / f"logs/release/local-{version}-{stamp}.json"
    out.parent.mkdir(parents=True, exist_ok=True)

    def save(**kw):
        result.update(kw)
        out.write_text(json.dumps(result, indent=2, ensure_ascii=False))

    log(f"deploy {candidate.name} over {old_hub.name}; database {result['db_before']}")
    helper = HOME / "bin/steve-keyhelper"
    helper_sha = sha256(helper) if helper.exists() else None

    old_manifest = json.loads((old_hub / "release.json").read_text())
    # what has been changed so far, so a failure before the new version starts undoes exactly that
    done = dict(drain=False, stopped=False, probed=False, hub_link=False, web_link=False, started=False)
    phase, backup = "drain", None
    try:
        log("drain")
        done["drain"] = True
        control("POST")
        until, st = time.monotonic() + args.drain_s, {}
        while time.monotonic() < until:
            st = control("GET")
            if st.get("ready"):
                break
            time.sleep(1)
        else:
            raise Refused(f"the Hub did not drain in {args.drain_s}s ({st})")
        log(f"drained: {st}")

        phase = "backup"
        done["stopped"] = True  # even a bootout that fails halfway may have stopped it
        launchd("stop", ident)
        backup = HOME / f"backups/pre-{candidate.name}-{stamp}.sqlite3"
        backup.parent.mkdir(parents=True, exist_ok=True)
        src, dst = sqlite3.connect(DB), sqlite3.connect(backup)
        try:
            src.backup(dst)
            check = dst.execute("PRAGMA quick_check").fetchone()[0]
        finally:
            dst.close()
            src.close()
        os.chmod(backup, 0o600)
        if check != "ok":
            raise Refused(f"the backup does not check out ({check})")
        save(backup=str(backup))
        log(f"backup {backup}")

        phase = "probe"
        done["probed"] = True  # from here the database may be migrated
        log(f"probe passed: {probe(candidate, version)}")

        phase = "switch"
        # a link is replaced atomically: it changed only once link() returned
        link(HOME / "current", candidate)
        done["hub_link"] = True
        link(HOME / "web/current", candidate / "web/dist")
        done["web_link"] = True
        done["started"] = True  # the new version may run business from here: no going back
        launchd("start", ident)
        if not wait_health(lambda h: h.get("version") == version and h.get("build_commit") == manifest["commit"], 60):
            raise RuntimeError(f"{version} did not answer /healthz")
        lift()
        h = wait_health(lambda h: h.get("version") == h.get("web_version") == version and h.get("drain") is not True
                        and (h.get("recovery") or {}).get("state") != "failed", 60)
        if not h:
            raise RuntimeError(f"{version} answers but is not ready: {health()}")
    except BaseException as e:
        if done["started"]:
            err = (f"{version} 启动后没有正常起来（{e}）；数据库可能已迁移（{schema_gate.state(DB)}），没有回滚，新版本保留，"
                   f"排空可能仍开着。发布前的备份在 {backup}")
            save(ok=False, degraded=True, phase=phase, error=err, db_after=schema_gate.state(DB))
            log(err)
            return 3
        recovered, steps = restore(done, old_hub, old_web, old_manifest, backup, candidate, stamp, ident)
        save(ok=False, phase=phase, error=f"{type(e).__name__}: {e}", recovered=recovered, recovery=steps)
        if recovered:
            log(f"✗ {phase}: {e}; {old_hub.name} runs as before ({'; '.join(steps)})")
            return 1
        log(f"✗ {phase}: {e}; and the old version did not come back: {'; '.join(steps)}")
        return 2
    if helper_sha and sha256(helper) != helper_sha:
        log("warning: steve-keyhelper changed during the deploy")
    save(ok=True, health=h, db_after=schema_gate.state(DB), finished=time.strftime("%Y%m%d-%H%M%S"))
    log(f"live: {h}")
    print(json.dumps(result, ensure_ascii=False))
    return 0


def restore(done, old_hub, old_web, old_manifest, backup, candidate, stamp, ident):
    """Undo what a deploy changed before the new version started: links, the database (the backup: nothing external
    happened, everything was drained and the probe ran in maintenance), the service, the drain. Every step is tried
    and reported; the old version counts as back only when it answers as itself with the drain off."""
    steps, ok = [], True

    def step(what, fn):
        nonlocal ok
        try:
            fn()
            steps.append(what)
        except BaseException as e:  # keep going: the next steps may still bring it back
            ok = False
            steps.append(f"{what} failed: {e}")

    if done["web_link"]:
        step("web/current back", lambda: link(HOME / "web/current", old_web))
    if done["hub_link"]:
        step("current back", lambda: link(HOME / "current", old_hub))
    if done["probed"]:
        def database():
            failed = HOME / f"backups/failed-probe-{candidate.name}-{stamp}"
            failed.mkdir(parents=True, exist_ok=True)
            for f in DB.parent.glob("steve.sqlite3*"):
                shutil.move(str(f), str(failed / f.name))
            shutil.copy2(backup, DB)
        step(f"the backup is back (the probed database is kept apart)", database)
    if not ok and done["probed"]:
        # no database we can vouch for is in place: starting would give the Hub an empty or migrated one
        return False, steps + ["service not started: put a database back by hand (ops/CUTOVER.md), then start it"]
    if done["stopped"] and not health():
        step("service started", lambda: launchd("start", ident))
    back = wait_health(lambda h: h.get("version") == old_manifest["version"]
                       and h.get("build_commit") in (None, old_manifest.get("commit")), 60)
    if not back:
        return False, steps + [f"{old_hub.name} does not answer /healthz"]
    if done["drain"]:
        step("drain lifted", lift)
    return ok, steps


def lift():
    """End the drain and check it is off (DELETE, then GET says maintenance false)."""
    control("DELETE")
    st = control("GET")
    if st.get("maintenance"):
        raise RuntimeError(f"the drain is still on after lifting it: {st}")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("commit")
    b.add_argument("--version")
    b.add_argument("--repo", default=str(OPS.parent))
    d = sub.add_parser("deploy")
    d.add_argument("release")
    d.add_argument("--drain-s", type=int, default=600)
    args = ap.parse_args()
    try:
        return build(args) if args.cmd == "build" else deploy(args)
    except Refused as e:
        log(f"✗ {e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
