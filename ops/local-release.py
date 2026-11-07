#!/usr/bin/env python3
"""Prepare an official Steve bundle and switch this machine's signed macOS installation to it.

    local-release.py build <commit> --version X.Y.Z    download and prepare the official release (legacy runner)
    local-release.py build <commit> --from-source     explicit developer build from a tagged commit
    local-release.py prepare-bundle <release dir> --version X.Y.Z --commit SHA
    local-release.py deploy <release dir> [--operation <id>]  switch the running Steve to it (newer or older)
    local-release.py prune [--dry-run]                   remove old release dirs and pre-release backups now

build defaults to the official bundle, verifying the published manifest, size and SHA256 before safe extraction.
--from-source requires Cargo, npm and Python 3.12+; only that explicit developer path compiles Hub/Node and Web.
prepare-bundle takes a verified archive already extracted by the Hub's runner and needs Python 3.9+. Both preserve
the installed keyhelper and sign only the candidate Hub with the original identity before deploy.

deploy, in order; any failure before the switch leaves the running version as it was:
  1. the release can open the database (ops/schema_gate.py): an older release on a database a newer one migrated
     is refused here, so this is also the only way back to an earlier release; and the release's Hub accepts this
     machine's steve.env (`steve-hub --check-config`: every startup check a real start makes on its environment,
     listen addresses included, without the database or the port). A setting it would refuse stops the deploy here
     with what to set, while the old version keeps serving;
  2. drain: the Hub's own maintenance drain (/internal/upgrade/drain) until it reports ready: no new work, running
     turns finished, adapters stopped. Not ready in --drain-s seconds: the drain is lifted and nothing changes;
  3. stop the service, back up the database (quick_check must pass) into $STEVE_HOME/backups;
  4. probe: the release's Hub opens the live database in maintenance (migrations, vault read), business off. A
     failure puts the backup back (nothing external happened: everything was drained and the probe ran in
     maintenance) and restarts the old version;
  5. switch `current` and `web/current`, start the service, lift maintenance, wait for /healthz to show the version.
     A failure after the switch is not undone: the database may be migrated, the new version stays and the
     result says so (exit 3), with the backup named.

After a healthy deploy (and with `prune`), old copies go: of the pre-release backups this script takes
(backups/pre-<version>-local-<sha7>-<stamp>.sqlite3, with their -shm/-wal) the newest 8 stay; of the release dirs
(<version>-local-<sha7>, and ops/release.sh's <stamp>-<sha>) `current`, the previous one and the newest 3 stay. The
previous one is what the last healthy deploy replaced: from its result in logs/release/, else from ops/release.sh's
releases/history.jsonl.
Nothing else in backups/ is touched (pre-rust, pre-purge, pre-takeover, daily copies, failed-probe-*). A deploy that
did not end healthy removes nothing. The one-click upgrade (hub-rs upgrades/runner.rs) runs this deploy, so it
prunes the same way.

--operation <id>: also write each step into the Hub's upgrade journal ($STEVE_HOME/updates/operations/<id>.json,
the phases the managed supervisor uses), so Settings · 关于 can follow a one-click upgrade across the restart. The
Hub's runner job (hub-rs upgrades/runner.rs) passes it; the result there is one of succeeded, rolled_back, failed
("升级已提交…" once the switch happened: not undone).

build and deploy hold the fcntl lock on $STEVE_HOME/releases/.release.lock, the one ops/release.sh takes: one release
at a time, whichever way it was started.

Identities come from $STEVE_HOME/signing/identity.json, written once per machine:
  {"label": "<launchd label>", "hub_identifier": "...", "keyhelper_identifier": "...", "vault_service": "..."}
and are checked against the installed Hub and steve-keyhelper signatures before anything is built or switched.
Results: $STEVE_HOME/logs/release/local-<version>-<time>.json.
"""
import argparse
import base64
import contextlib
import fcntl
import re
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
import urllib.parse
import uuid

OPS = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(OPS))
import schema_gate  # noqa: E402

HOME = pathlib.Path(os.environ.get("STEVE_HOME", pathlib.Path.home() / ".steve"))
DB = HOME / "data/steve.sqlite3"
PLIST_DIR = pathlib.Path.home() / "Library/LaunchAgents"


class Refused(Exception):
    """Stopped before anything changed for the running Steve."""


class Journal:
    """The Hub's record of a one-click upgrade (--operation): each step, then the result. Without an id it does
    nothing. Written the way the Hub writes it: the whole operation, to a temporary file, renamed, mode 0600."""

    def __init__(self, op_id):
        self.path = None
        if op_id:
            uuid.UUID(op_id)  # a path component: only an operation id
            self.path = HOME / "updates/operations" / f"{op_id}.json"
            if not self.path.exists():
                raise Refused(f"no upgrade operation {op_id}")

    def __call__(self, phase, error=None, step=None, **fields):
        if not self.path:
            return
        op = json.loads(self.path.read_text())
        step = step or {"draining": "waiting", "restarting": "starting", "verifying": "checking",
                        "committed": "checking", "succeeded": "complete", "rolling_back": "rollback",
                        "rolled_back_pending_health": "rollback", "rolled_back": "rollback",
                        "staging": "resolving", "ready": "preflight"}.get(phase, phase)
        progress = dict(op.get("progress") or {})
        # Drain polls often repeat the same snapshot. Keep the previous observation instead of fsyncing it
        # every second; elapsed time is derived from started_at by the UI.
        if (op.get("phase") == phase and op.get("error") == error and progress.get("step") == step
                and all(op.get(k) == v for k, v in fields.items())):
            return
        now = int(time.time() * 1000)
        if progress.get("step") != step:
            progress = dict(step=step, started_at=now, updated_at=now)
        else:
            progress.update(updated_at=now)
        op.update(fields, phase=phase, error=error, updated_at=now, progress=progress)
        tmp = self.path.with_name(f".{uuid.uuid4()}.tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as f:
            f.write(json.dumps(op, indent=2, ensure_ascii=False))
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, self.path)

    def terminal(self):
        return bool(self.path) and json.loads(self.path.read_text()).get("phase") in ("succeeded", "failed", "rolled_back")


def release_lock():
    """ops/release.sh's lock (zsystem flock, an fcntl lock): held until this process exits."""
    p = HOME / "releases/.release.lock"
    p.parent.mkdir(parents=True, exist_ok=True)
    f = open(p, "a")
    try:
        fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        f.close()
        raise Refused(f"另一个发布正在进行（{p} 被占用）")
    return f


def log(msg):
    print(f"{time.strftime('%H:%M:%S')} {msg}", flush=True)


def env_file():
    """What steve.env sets, read the way run.sh reads it (sourced by the shell: $HOME and quotes expand)."""
    p = HOME / "steve.env"
    if not p.exists():
        return {}
    clean = {k: os.environ[k] for k in ("HOME", "PATH", "USER", "TMPDIR") if k in os.environ}
    out = subprocess.run(["/bin/zsh", "-fc", 'set -a; source "$1"; env -0', "zsh", str(p)],
                         env=clean, capture_output=True, check=True).stdout
    sourced = dict(kv.split("=", 1) for kv in out.decode().split("\0") if "=" in kv)
    return {k: v for k, v in sourced.items() if clean.get(k) != v and k not in ("PWD", "OLDPWD", "SHLVL", "_")}


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

SIGNING_ACCOUNT = "steve/signing/keychain-pass"  # hub-rs steve-modules vault/keystore/signing.rs


def signing_password(ident, from_stdin=False):
    """The signing keychain's password: the first line of stdin (the Hub's runner), the 0600 file sign-setup.sh
    writes, or the login keychain entry the Hub moved it into (`security` asks the user for that one)."""
    if from_stdin:
        password = sys.stdin.readline().strip()
        if not password:
            raise Refused("--keychain-pass-stdin: no password on stdin")
        return password
    file = HOME / "signing/keychain-pass"
    if file.exists():
        return file.read_text().strip()
    # only the signed Hub reads the entry unasked; anyone else meets a macOS prompt, which only a person at a
    # terminal should see (a Hub-run or agent-run build would block on it, or teach the user to click it away)
    if not sys.stdin.isatty():
        raise Refused(f"the signing keychain's password is in the login keychain ({SIGNING_ACCOUNT}), and only a run "
                      "from a terminal asks macOS for it: upgrade from Steve's settings, or run this in Terminal")
    log(f"macOS asks for the login keychain item {SIGNING_ACCOUNT}: click 允许 (Allow), never 始终允许 (Always Allow), "
        "which would let every process read it unasked")
    got = subprocess.run(["security", "find-generic-password", "-s", ident["vault_service"], "-a", SIGNING_ACCOUNT, "-w"],
                         capture_output=True, text=True)
    if got.returncode != 0:
        raise Refused("no signing keychain password: not in signing/keychain-pass, and the keychain entry "
                      f"{SIGNING_ACCOUNT} did not open (run the upgrade from Steve's settings, or allow the prompt)")
    try:
        return base64.b64decode(got.stdout.strip(), validate=True).hex()
    except ValueError:
        raise Refused(f"the keychain entry {SIGNING_ACCOUNT} is not a signing password")

def quotable(password, keychain):
    """What a `security -i` line can carry: no quoting inside it."""
    if not password or any(c.isspace() or c in "\"'\\" for c in password) or any(c in "\"\\\n" for c in str(keychain)):
        raise Refused("the signing keychain's password or path has a space, quote or backslash: not passed to security")


@contextlib.contextmanager
def keychain_unlocked(keychain, password):
    """The signing keychain, unlocked for the block and locked again after it, failed or not: left unlocked, it lets
    any process sign a binary as the Hub (and steve-keyhelper hands that binary the vault's keys). The password goes
    to `security -i` on stdin, never on a command line (`ps` shows those to every process of the user). The keychain
    also locks itself after 5 idle minutes and on sleep, should this process die inside the block."""
    quotable(password, keychain)
    unlocked = subprocess.run(["security", "-i"], input=f'unlock-keychain -p {password} "{keychain}"\n',
                              capture_output=True, text=True)
    if unlocked.returncode != 0:
        raise Refused("the signing keychain did not unlock")
    try:
        subprocess.run(["security", "set-keychain-settings", "-l", "-u", "-t", "300", str(keychain)], capture_output=True)
        yield
    finally:
        if subprocess.run(["security", "lock-keychain", str(keychain)], capture_output=True).returncode != 0:
            log(f"could not lock {keychain} again")


def sign(binary, keychain, password, identifier):
    """codesign `binary` as `identifier` with the signing keychain unlocked only meanwhile."""
    with keychain_unlocked(keychain, password):
        subprocess.run(["codesign", "--force", "--sign", "Steve Hub", "--identifier", identifier,
                        "--keychain", str(keychain), str(binary)], check=True)
    subprocess.run(["codesign", "--verify", "--strict", str(binary)], check=True)


BUNDLE_OPS = {"run.sh", "run-managed.sh", "backup.sh", "schema_gate.py", "local-release.py", "steve-service.py", "sign-setup.sh", "release-lib.sh"}
MAX_BUNDLE_BYTES = 2 * 1024 * 1024 * 1024


class ReleaseRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        url = urllib.parse.urlparse(newurl)
        if (url.scheme != "https" or url.username or url.password
                or not (url.hostname == "github.com" or (url.hostname or "").endswith(".githubusercontent.com"))):
            raise Refused("release download redirected outside GitHub HTTPS")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def release_download(url, destination=None, expected_size=None, expected_sha=None):
    """Bounded HTTPS download; only GitHub's release storage redirects are allowed."""
    opener = urllib.request.build_opener(ReleaseRedirect())
    req = urllib.request.Request(url, headers={"User-Agent": "Steve-release-bootstrap", "Accept": "application/octet-stream"})
    limit = expected_size if destination else 1024 * 1024
    digest, total, chunks = hashlib.sha256(), 0, []
    with opener.open(req, timeout=60) as response:
        with (destination.open("xb") if destination else contextlib.nullcontext()) as output:
            while True:
                chunk = response.read(1024 * 1024)
                if not chunk:
                    break
                total += len(chunk)
                if total > limit:
                    raise Refused("release download exceeds expected size")
                digest.update(chunk)
                if output:
                    output.write(chunk)
                else:
                    chunks.append(chunk)
    if destination:
        if total != expected_size or digest.hexdigest() != expected_sha:
            raise Refused("release archive size or SHA256 does not match manifest")
    else:
        return json.loads(b"".join(chunks))


def unpack_bundle(archive, candidate):
    """Python 3.9-compatible extraction: only regular build outputs, no links, devices or paths outside root."""
    total, names = 0, set()
    with tarfile.open(archive, "r:gz") as bundle:
        for member in bundle:
            name = member.name.rstrip("/")
            path = pathlib.PurePosixPath(name)
            if (not name or path.is_absolute() or ".." in path.parts or "\\" in name
                    or str(path) != name or name in names or len(names) >= 100000):
                raise Refused("unsafe or duplicate release archive path")
            names.add(name)
            directory = name in {"bin", "web", "web/dist", "ops"} or name.startswith("web/dist/")
            regular = (name in {"release.json", "bin/steve-hub", "bin/steve-node", "bin/steve-keyhelper"}
                       or name in {"ops/" + n for n in BUNDLE_OPS} or name.startswith("web/dist/"))
            target = candidate.joinpath(*path.parts)
            if member.isdir() and directory:
                target.mkdir(parents=True, exist_ok=True)
            elif member.isfile() and regular:
                total += member.size
                if member.size < 0 or total > MAX_BUNDLE_BYTES:
                    raise Refused("release archive exceeds unpacked size limit")
                target.parent.mkdir(parents=True, exist_ok=True)
                with bundle.extractfile(member) as source, target.open("xb") as output:
                    shutil.copyfileobj(source, output, length=1024 * 1024)
                target.chmod(0o755 if name.startswith(("bin/", "ops/")) else 0o644)
            else:
                raise Refused(f"unexpected release archive member: {name}")


def build(args):
    """Compatibility bridge for older runners: `build` now prepares the official precompiled bundle.

    Old runners derive this exact candidate path and use the `build ... into ...` marker to own cleanup.
    Keep that wire contract. Source compilation is only the explicitly requested --from-source path.
    """
    if args.from_source:
        return source_build(args)
    version, commit = args.version, args.commit
    if (not version or not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", version)
            or not re.fullmatch(r"[0-9a-f]{40}", commit)):
        raise Refused("bundle preparation needs --version X.Y.Z and a full commit SHA; source builds need --from-source")
    repository = os.environ.get("STEVE_UPDATE_REPOSITORY", "Infranite/steve")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository) or any(p in (".", "..") for p in repository.split("/")):
        raise Refused("invalid STEVE_UPDATE_REPOSITORY")
    target = "aarch64-apple-darwin"
    base = f"https://github.com/{repository}/releases/download/v{version}/"
    manifest = release_download(base + "steve-release.json")
    if (not isinstance(manifest, dict) or manifest.get("format_version") != 1
            or manifest.get("version") != version or manifest.get("commit") != commit):
        raise Refused("published release identity differs from requested version/commit")
    assets = [a for a in manifest.get("assets", []) if isinstance(a, dict) and a.get("target") == target]
    expected_name = f"steve-{version}-{target}.tar.gz"
    if len(assets) != 1:
        raise Refused("release has no unique matching platform asset")
    asset = assets[0]
    if (asset.get("name") != expected_name or type(asset.get("size")) is not int
            or not 0 < asset["size"] <= MAX_BUNDLE_BYTES
            or not re.fullmatch(r"[0-9a-f]{64}", str(asset.get("sha256", "")))):
        raise Refused("release asset metadata is invalid")
    candidate = (HOME / "releases").resolve() / f"{version}-local-{commit[:7]}"
    if candidate.exists() or candidate.is_symlink():
        raise Refused(f"{candidate} exists: inspect it rather than overwrite")
    with tempfile.TemporaryDirectory(prefix="steve-release-bundle-") as tmp:
        archive = pathlib.Path(tmp) / expected_name
        log(f"download official release {version} ({asset['size']} bytes)")
        release_download(base + expected_name, archive, asset["size"], asset["sha256"])
        log(f"build {version} from {commit} into {candidate}")
        candidate.mkdir(parents=True)
        try:
            unpack_bundle(archive, candidate)
            inner = json.loads((candidate / "release.json").read_text())
            if dict(inner, assets=[]) != dict(manifest, assets=[]):
                raise Refused("archive manifest differs from published release")
            args.release = str(candidate)
            return prepare_bundle(args)
        except BaseException:
            # This invocation created the directory while holding release_lock; never remove pre-existing data.
            shutil.rmtree(candidate)
            raise


def source_build(args):
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
    if candidate.exists() or candidate.is_symlink():
        raise Refused(f"{candidate} exists: inspect it rather than overwrite")
    keychain = HOME / "signing/steve-signing.keychain-db"
    if not keychain.exists():
        raise Refused(f"no signing keychain at {keychain} (ops/sign-setup.sh): an ad-hoc signed Hub can't read the vault")
    # the keychain's password: from the Hub over stdin (the one-click upgrade), else signing/keychain-pass, else the
    # key store entry the Hub moved it into (a macOS prompt: only the signed Hub reads it unasked)
    password = signing_password(ident, from_stdin=args.keychain_pass_stdin)
    quotable(password, keychain)
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
                        "-p", "steve-hub", "-p", "steve-node-cli", "-p", "steve-keyhelper"], env=env, check=True)
        binary = candidate / "bin/steve-hub"
        binary.parent.mkdir(exist_ok=True)
        shutil.copy2(target_dir / "release/steve-hub", binary)
        for name in ("steve-node", "steve-keyhelper"):
            shutil.copy2(target_dir / "release" / name, candidate / "bin" / name)
        sign(binary, keychain, password, ident["hub_identifier"])
        del password
        info = build_info(binary)
        if info.get("version") != version or info.get("commit") != commit:
            raise Refused(f"--build-info says {info}, expected {version} {commit}")
        log(f"hub: {info}")
        subprocess.run(["npm", "ci", "-w", "web", "--include-workspace-root", "--no-audit", "--no-fund"], cwd=candidate, check=True)
        subprocess.run(["npm", "-w", "web", "run", "build"], cwd=candidate, check=True)
        with tempfile.TemporaryDirectory() as out:
            subprocess.run([sys.executable, str(candidate / "scripts/package-release.py"), "package", "--version", version,
                            "--commit", commit, "--target", info["target"], "--binary", str(binary),
                            "--node", str(target_dir / "release/steve-node"), "--keyhelper", str(target_dir / "release/steve-keyhelper"),
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


def validate_bundle(args):
    """Validate an already verified/extracted official bundle, in place. No network, compiler or helper install.

    The caller verifies the downloaded archive against the official release manifest before extraction. This
    second gate checks the on-disk identities before signing only the Hub as this machine's original identity.
    The installed helper remains untouched: its certificate and compiled service define the existing vault.
    """
    candidate = pathlib.Path(args.release).absolute()
    releases = (HOME / "releases").resolve()
    if (candidate.is_symlink() or not candidate.is_dir() or candidate.resolve() != candidate
            or candidate.parent != releases):
        raise Refused("bundle must be a real direct child of STEVE_HOME/releases")
    for current in (HOME / "current", HOME / "web/current"):
        if current.exists() and (current.resolve() == candidate or candidate in current.resolve().parents):
            raise Refused("cannot prepare the running release")
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", str(args.version or "")):
        raise Refused("expected version must be stable SemVer")
    if not re.fullmatch(r"[0-9a-f]{40}", str(args.commit or "")):
        raise Refused("expected commit must be a full Git SHA")
    if any(p.is_symlink() for p in candidate.rglob("*")):
        raise Refused("bundle contains a symlink")
    try:
        manifest = json.loads((candidate / "release.json").read_text())
        web = json.loads((candidate / "web/dist/release.json").read_text())
        html = (candidate / "web/dist/index.html").read_text()
        info = build_info(candidate / "bin/steve-hub")
    except (OSError, ValueError, subprocess.SubprocessError) as e:
        raise Refused(f"cannot read bundle identity: {e}") from e
    if not all(isinstance(v, dict) for v in (manifest, web, info)):
        raise Refused("bundle identities must be objects")
    for name, value in (("release.json", manifest), ("Hub", info), ("Web", web)):
        if value.get("version") != args.version or value.get("commit") != args.commit:
            raise Refused(f"{name} identity differs from expected version/commit")
    if (manifest.get("format_version") != 1 or manifest.get("hub_version") != args.version
            or manifest.get("web_version") != args.version
            or info.get("target") != "aarch64-apple-darwin"
            or (manifest.get("schema") or {}).get("target") != info.get("schema")):
        raise Refused("release metadata does not match Hub build")
    if not re.search(r'<meta\s+name=["\']steve-web-version["\']\s+content=["\']'
                     + re.escape(args.version) + r'["\']', html):
        raise Refused("Web index identity differs from expected version")
    for name in ("run.sh", "backup.sh", "schema_gate.py", "local-release.py"):
        if not (candidate / "ops" / name).is_file():
            raise Refused(f"bundle has no runtime ops/{name}")
    return candidate, info


def prepare_bundle(args):
    candidate, info = validate_bundle(args)
    ident = identity()
    helper = HOME / "bin/steve-keyhelper"
    if not helper.is_file():
        raise Refused("installed steve-keyhelper is missing; refusing to create or change vault identity")
    helper_hash = sha256(helper)
    keychain = HOME / "signing/steve-signing.keychain-db"
    if not keychain.is_file():
        raise Refused("the existing signing keychain is missing")
    password = signing_password(ident, from_stdin=args.keychain_pass_stdin)
    sign(candidate / "bin/steve-hub", keychain, password, ident["hub_identifier"])
    del password
    if signed_identifier(candidate / "bin/steve-hub") != ident["hub_identifier"]:
        raise Refused("prepared Hub signature has the wrong identifier")
    if sha256(helper) != helper_hash:
        raise Refused("installed steve-keyhelper changed during preparation")
    if build_info(candidate / "bin/steve-hub") != info:
        raise Refused("Hub build identity changed during preparation")
    print(json.dumps(dict(candidate=str(candidate), build=info, source="release-bundle")))
    return 0


PUBLIC_IDENTITY = dict(hub_identifier="io.github.infranite.steve.hub",
                       keyhelper_identifier="io.github.infranite.steve.keyhelper",
                       vault_service="io.github.infranite.steve.vault")


def setup_keychain(args):
    """Explicit first install from a verified bundle. Never replace an installed identity/helper or migrate keys.

    sign-setup.sh creates the local certificate, then the already compiled generic helper and Hub are signed.
    A completed setup is repeatable; an interrupted helper install needs inspection instead of blind replacement.
    """
    if sys.platform != "darwin":
        raise Refused("setup-keychain is only for a new macOS Hub")
    candidate = pathlib.Path(args.release).absolute()
    ident = dict(PUBLIC_IDENTITY, label=args.label)
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,200}", args.label):
        raise Refused("invalid service label")
    setup_record = HOME / "signing/bundle-setup.json"
    identity_file = HOME / "signing/identity.json"
    helper = HOME / "bin/steve-keyhelper"
    if setup_record.is_file() and identity_file.is_file() and helper.is_file():
        record = json.loads(setup_record.read_text())
        if (record.get("release") == str(candidate) and json.loads(identity_file.read_text()) == ident
                and signed_identifier(helper) == ident["keyhelper_identifier"]
                and signed_identifier(candidate / "bin/steve-hub") == ident["hub_identifier"]):
            print(json.dumps(dict(initialized=True, changed=False, release=str(candidate))))
            return 0
    if (identity_file.exists() or identity_file.is_symlink() or setup_record.exists() or setup_record.is_symlink()
            or helper.exists() or helper.is_symlink()
            or (HOME / "current").exists() or (HOME / "current").is_symlink()
            or ((HOME / "data").exists() and any((HOME / "data").iterdir()))):
        raise Refused("home already has a Hub, identity, helper or data; setup will not replace or migrate an existing vault")
    manifest = json.loads((candidate / "release.json").read_text())
    args.version, args.commit = manifest.get("version"), manifest.get("commit")
    candidate, info = validate_bundle(args)
    generic_helper = candidate / "bin/steve-keyhelper"
    if not generic_helper.is_file():
        raise Refused("bundle has no precompiled keyhelper")
    for name in ("sign-setup.sh", "release-lib.sh"):
        if not (candidate / "ops" / name).is_file():
            raise Refused(f"bundle has no ops/{name}")
    env = {k: os.environ[k] for k in ("HOME", "PATH", "USER", "TMPDIR") if k in os.environ}
    env["STEVE_HOME"] = str(HOME)
    subprocess.run(["/bin/zsh", str(candidate / "ops/sign-setup.sh")], env=env, check=True)
    keychain = HOME / "signing/steve-signing.keychain-db"
    password = signing_password(ident)
    helper.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".steve-keyhelper-", dir=helper.parent)
    os.close(fd)
    temporary = pathlib.Path(temporary)
    try:
        shutil.copyfile(generic_helper, temporary)
        temporary.chmod(0o755)
        sign(temporary, keychain, password, ident["keyhelper_identifier"])
        sign(candidate / "bin/steve-hub", keychain, password, ident["hub_identifier"])
        if (signed_identifier(temporary) != ident["keyhelper_identifier"]
                or signed_identifier(candidate / "bin/steve-hub") != ident["hub_identifier"]):
            raise Refused("first-install signatures did not verify")
        # link is atomic and fails if a helper appeared meanwhile; never overwrite any existing helper.
        os.link(temporary, helper)
        fd = os.open(identity_file, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as output:
            json.dump(ident, output, indent=2)
        fd = os.open(setup_record, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "w") as output:
            json.dump(dict(release=str(candidate), build=info), output)
    finally:
        del password
        temporary.unlink(missing_ok=True)
    print(json.dumps(dict(initialized=True, changed=True, release=str(candidate), build=info)))
    return 0


# ── deploy ──

def private_backups():
    """backups/ holds copies of the database: 0700, everything in it the owner's alone, also what older releases
    left readable; the same for steve.env and its stray copies (steve.env.bak-*)."""
    d = HOME / "backups"
    d.mkdir(parents=True, exist_ok=True)
    os.chmod(d, 0o700)
    for root, dirs, files in os.walk(d):
        for n in dirs:
            os.chmod(os.path.join(root, n), 0o700)
        for n in files:
            p = os.path.join(root, n)
            if not os.path.islink(p):
                os.chmod(p, 0o600)
    for p in HOME.glob("steve.env*"):
        if p.is_file() and not p.is_symlink():
            os.chmod(p, 0o600)


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


def hub_env(candidate):
    """The production environment (what run.sh gives the Hub) for `candidate`'s Hub."""
    env = {k: os.environ[k] for k in ("HOME", "PATH", "TMPDIR") if k in os.environ}
    env.update(env_file())
    env.update(STEVE_HOME=str(HOME), STEVE_WEB_DIR=str(candidate / "web/dist"))
    env.setdefault("STEVE_KEYHELPER", str(HOME / "bin/steve-keyhelper"))
    return env


ANY_HOST_HINT = ("二选一：在 steve.env 里加上 STEVE_ALLOW_ANY_HOST=1（继续监听所有网络），或者把 STEVE_HOST 改成 127.0.0.1 "
                 "加上需要访问 Hub 的地址（例如 127.0.0.1,100.64.0.2）")


def check_config(candidate, info):
    """The candidate's own startup checks on the real steve.env (`steve-hub --check-config`, hub-rs check.rs), before
    anything is drained: the probe below runs on a private port and host, so it cannot see a listen address the new
    Hub would refuse. A release from before the check (no check_config in its --build-info) is not asked: that
    binary would start a whole Hub instead."""
    if not info.get("check_config"):
        log(f"{candidate.name} has no --check-config; its startup checks are left to the probe")
        return
    try:
        r = subprocess.run([str(candidate / "bin/steve-hub"), "--check-config"], env=hub_env(candidate),
                           capture_output=True, text=True, timeout=120)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise Refused(f"不能切到 {candidate.name}：它的 --check-config 没能跑完（{e}），没有排空也没有停止，当前版本照常运行")
    try:
        report = json.loads(r.stdout)
    except ValueError:
        report = None
    if not isinstance(report, dict):
        report = dict(ok=False, errors=[(r.stdout + r.stderr).strip() or f"exit {r.returncode}"])
    if r.returncode == 0 and report.get("ok") is True:
        return
    errors = [str(e) for e in report.get("errors") or []] or [f"exit {r.returncode}"]
    hint = f"。{ANY_HOST_HINT}" if any(e.startswith("STEVE_HOST: 0.0.0.0") for e in errors) else ""
    raise Refused(f"不能切到 {candidate.name}：它按 {HOME / 'steve.env'} 启动不了，没有排空也没有停止，当前版本照常运行。"
                  f"改好 steve.env 再部署：{'；'.join(errors)}{hint}")


def probe(candidate, version):
    """The candidate's Hub on the live database in maintenance: migrations, vault read; business must be off. Its
    listen address and port are its own: the environment's own startup checks ran in check_config()."""
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
    # port, the upgrade gate, no resumed work, no agent that could start, and no local Node (an explicit
    # STEVE_NODE_MODE=local in steve.env would refuse a STEVE_NO_RESUME copy, and the Node's home is the live one's)
    env = hub_env(candidate)
    env.update(STEVE_PORT=str(p_port), STEVE_HOST="127.0.0.1",
               STEVE_UPGRADE_ID=ident, STEVE_NO_RESUME="1", STEVE_NODE_MODE="off", STEVE_REPO=str(HOME / "probe-unused-repo"),
               STEVE_ADAPTER_CLAUDE="/usr/bin/false", STEVE_ADAPTER_CODEX="/usr/bin/false",
               CLAUDE_CODE_EXECUTABLE="/usr/bin/false")
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


def deploy(args, journal=Journal(None)):
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
    check_config(candidate, info)
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
        journal("draining", target=str(candidate))
        done["drain"] = True
        control("POST")
        until, st = time.monotonic() + args.drain_s, {}
        while time.monotonic() < until:
            st = control("GET")
            journal("draining", drain=st)
            if st.get("ready"):
                break
            time.sleep(1)
        else:
            raise Refused(f"the Hub did not drain in {args.drain_s}s ({st})")
        log(f"drained: {st}")

        phase = "backup"
        journal("stopping")
        done["stopped"] = True  # even a bootout that fails halfway may have stopped it
        launchd("stop", ident)
        journal("backing_up")
        backup = HOME / f"backups/pre-{candidate.name}-{stamp}.sqlite3"
        private_backups()
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
        journal("backing_up", backup=str(backup))
        log(f"backup {backup}")

        phase = "probe"
        done["probed"] = True  # from here the database may be migrated
        journal("backing_up", step="probing")
        log(f"probe passed: {probe(candidate, version)}")

        phase = "switch"
        journal("restarting", previous=str(old_hub))
        # a link is replaced atomically: it changed only once link() returned
        link(HOME / "current", candidate)
        done["hub_link"] = True
        link(HOME / "web/current", candidate / "web/dist")
        done["web_link"] = True
        done["started"] = True  # the new version may run business from here: no going back
        launchd("start", ident)
        journal("verifying")
        if not wait_health(lambda h: h.get("version") == version and h.get("build_commit") == manifest["commit"], 60):
            raise RuntimeError(f"{version} did not answer /healthz")
        journal("committed")
        journal("committed", step="checking")
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
            journal("failed", error=f"升级已提交：{err}；结果见 {out}")
            log(err)
            return 3
        recovered, steps = restore(done, old_hub, old_web, old_manifest, backup, candidate, stamp, ident)
        save(ok=False, phase=phase, error=f"{type(e).__name__}: {e}", recovered=recovered, recovery=steps)
        if recovered:
            log(f"✗ {phase}: {e}; {old_hub.name} runs as before ({'; '.join(steps)})")
            if done["stopped"]:
                journal("rolled_back", error=f"{phase} 失败（{type(e).__name__}: {e}），已恢复到 {old_hub.name}：{'；'.join(steps)}。结果见 {out}")
            else:
                journal("failed", error=f"{phase} 失败（{type(e).__name__}: {e}），旧版本没有停止，没有改动。结果见 {out}")
            return 1
        log(f"✗ {phase}: {e}; and the old version did not come back: {'; '.join(steps)}")
        journal("failed", error=f"{phase} 失败（{type(e).__name__}: {e}），旧版本没有恢复，需要手动处理：{'；'.join(steps)}。"
                         f"发布前的备份：{backup or '尚未备份'}；结果见 {out}")
        return 2
    if helper_sha and sha256(helper) != helper_sha:
        log("warning: steve-keyhelper changed during the deploy")
    save(ok=True, health=h, db_after=schema_gate.state(DB), finished=time.strftime("%Y%m%d-%H%M%S"))
    journal("succeeded")
    log(f"live: {h}")
    prune_after(old_hub, save)
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
        return False, steps + ["service not started: restore the pre-release database backup by hand, then start it"]
    if done["stopped"] and not health():
        step("service started", lambda: launchd("start", ident))
    back = wait_health(lambda h: h.get("version") == old_manifest["version"]
                       and h.get("build_commit") in (None, old_manifest.get("commit")), 60)
    if not back:
        return False, steps + [f"{old_hub.name} does not answer /healthz"]
    if done["drain"]:
        step("drain lifted", lift)
    return ok, steps


def lift(seconds=120):
    """End the drain and check it is off (DELETE, then GET says maintenance false). A Hub that just started
    may be busy (recovery, an index to copy): a call that times out is tried again until `seconds` pass."""
    until, last = time.monotonic() + seconds, None
    while time.monotonic() < until:
        try:
            control("DELETE")
            st = control("GET")
            if not st.get("maintenance"):
                return
            last = f"still on: {st}"
        except (OSError, ValueError) as e:
            last = f"{type(e).__name__}: {e}"
        time.sleep(2)
    raise RuntimeError(f"the drain could not be lifted in {seconds}s ({last})")


# ── prune ──

KEEP_BACKUPS = 8
KEEP_RELEASES = 3
# pre-<version>-local-<sha7>-<YYYYmmdd-HHMMSS>.sqlite3 (deploy below), with SQLite's side files
UUID_PATTERN = r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"
BACKUP_RE = re.compile(r"^(pre-(\d+\.\d+\.\d+)-(?:local-[0-9a-f]{7}|" + UUID_PATTERN
                       + r")-(\d{8}-\d{6})\.sqlite3)(-shm|-wal|-journal)?$")
BUNDLE_DIR_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)-" + UUID_PATTERN + "$")
LOCAL_DIR_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)-local-[0-9a-f]{7}$")
SCRIPT_DIR_RE = re.compile(r"^(\d{8}-\d{6})-[0-9a-f]{7,40}$")  # ops/release.sh's


def prune(keep=(), dry=False):
    """Old pre-release backups and release dirs, as the module docstring says. `keep`: release dirs that stay
    whatever their rank (the previous one of a deploy). Returns what was removed (or would be, `dry`)."""
    removed = []

    def rm(p):
        removed.append(str(p))
        if dry:
            return
        try:
            if p.is_dir() and not p.is_symlink():
                shutil.rmtree(p)
            else:
                p.unlink()
        except FileNotFoundError:
            pass

    backups = HOME / "backups"
    if backups.is_dir():
        groups = {}
        for f in backups.iterdir():
            m = BACKUP_RE.match(f.name)
            if m and not f.is_symlink() and f.is_file():
                groups.setdefault(m.group(1), []).append(f)
        whole = sorted((b for b in groups if (backups / b) in groups[b]),
                       key=lambda b: (BACKUP_RE.match(b).group(3), b), reverse=True)
        for base in sorted(groups):
            # beyond the newest, and a -shm/-wal whose database is gone (of no use to anyone)
            if base not in whole[:KEEP_BACKUPS]:
                for f in sorted(groups[base]):
                    rm(f)

    releases = HOME / "releases"
    if releases.is_dir():
        protect = {pathlib.Path(k).resolve() for k in keep}
        for link in (HOME / "current", HOME / "previous", HOME / "web/current"):
            if link.exists():
                target = link.resolve()
                protect.update(p for p in (target, *target.parents) if p.parent == releases.resolve())
        ranked = []
        for d in releases.iterdir():
            if d.is_symlink() or not d.is_dir():
                continue
            if m := LOCAL_DIR_RE.match(d.name):
                ranked.append(((1, tuple(int(x) for x in m.groups()), d.name), d))
            elif m := BUNDLE_DIR_RE.match(d.name):
                # UUID-shaped user directories are not ours unless their manifest owns this exact version.
                try:
                    manifest = json.loads((d / "release.json").read_text())
                    owned = (not (d / "release.json").is_symlink() and isinstance(manifest, dict)
                             and manifest.get("format_version") == 1
                             and manifest.get("version") == ".".join(m.groups())
                             and re.fullmatch(r"[0-9a-f]{40}", str(manifest.get("commit", ""))))
                except (OSError, ValueError):
                    owned = False
                if owned:
                    ranked.append(((1, tuple(int(x) for x in m.groups()), d.name), d))
            elif m := SCRIPT_DIR_RE.match(d.name):
                ranked.append(((0, (), m.group(1)), d))  # older than any local build
        ranked.sort(key=lambda r: r[0], reverse=True)
        for _, d in ranked[KEEP_RELEASES:]:
            if d.resolve() not in protect:
                rm(d)
    return removed


def last_previous():
    """The release dir the last healthy deploy replaced (its rollback target): from the newest successful
    logs/release/local-*.json, else (a machine that only ever used ops/release.sh) the last healthy hub line of
    releases/history.jsonl. None when neither says."""
    results = []
    for f in (HOME / "logs/release").glob("local-*.json"):
        try:
            r = json.loads(f.read_text())
        except (OSError, ValueError):
            continue
        if isinstance(r, dict) and r.get("ok") is True and r.get("previous"):
            results.append((str(r.get("finished") or r.get("started") or ""), f.name, r["previous"]))
    if results:
        return pathlib.Path(max(results)[2])
    prev = None
    try:
        lines = (HOME / "releases/history.jsonl").read_text().splitlines()
    except OSError:
        lines = []
    for line in lines:
        try:
            h = json.loads(line)
        except ValueError:
            continue
        if isinstance(h, dict) and h.get("kind") == "hub" and h.get("ok") is True and h.get("prev"):
            prev = HOME / "releases" / pathlib.Path(h["prev"]).name
    return prev


def prune_after(previous, save):
    """prune() once a deploy is healthy: a failure here is logged, never the deploy's result."""
    try:
        removed = prune(keep=[previous])
    except Exception as e:  # noqa: BLE001 - the release is live; tidying up must not say otherwise
        log(f"warning: pruning old releases failed: {type(e).__name__}: {e}")
        save(pruned_error=f"{type(e).__name__}: {e}")
        return
    if removed:
        log(f"pruned {len(removed)}: {', '.join(pathlib.Path(p).name for p in removed)}")
    save(pruned=removed)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    b = sub.add_parser("build")
    b.add_argument("commit")
    b.add_argument("--version")
    b.add_argument("--repo", default=str(OPS.parent))
    b.add_argument("--from-source", action="store_true", help="explicit developer build; requires cargo, npm and Python 3.12+")
    b.add_argument("--keychain-pass-stdin", action="store_true",
                   help="read the signing keychain's password from the first line of stdin (the Hub's upgrade runner)")
    p = sub.add_parser("prepare-bundle")
    p.add_argument("release")
    p.add_argument("--version", required=True)
    p.add_argument("--commit", required=True)
    p.add_argument("--keychain-pass-stdin", action="store_true")
    first = sub.add_parser("setup-keychain", help="explicit first macOS install; never replaces existing identity or helper")
    first.add_argument("release")
    first.add_argument("--label", default="io.github.infranite.steve.hub")
    d = sub.add_parser("deploy")
    d.add_argument("release")
    d.add_argument("--drain-s", type=int, default=600)
    d.add_argument("--operation", help="the Hub's upgrade operation id: write each step into its journal")
    pr = sub.add_parser("prune")
    pr.add_argument("--dry-run", action="store_true", help="only list what would be removed")
    args = ap.parse_args()
    journal = Journal(None)
    try:
        if args.cmd == "deploy":
            journal = Journal(args.operation)
        lock = release_lock()
        try:
            if args.cmd == "prune":
                prev = last_previous()
                for p in prune(keep=[prev] if prev else [], dry=args.dry_run):
                    print(("would remove " if args.dry_run else "removed ") + p)
                return 0
            if args.cmd == "setup-keychain":
                return setup_keychain(args)
            if args.cmd == "prepare-bundle":
                return prepare_bundle(args)
            return build(args) if args.cmd == "build" else deploy(args, journal)
        finally:
            lock.close()
    except (Refused, OSError, ValueError, tarfile.TarError, subprocess.SubprocessError) as e:
        log(f"✗ {e}")
        if journal.path and not journal.terminal():
            journal("failed", error=f"没有改动：{e}")
        return 1


if __name__ == "__main__":
    sys.exit(main())
