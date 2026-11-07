#!/usr/bin/env python3
"""Local signed-Hub upgrades (Python 3.9+), using the Hub's durable Operation/Spec journal.

    python3 ops/upgrade.py check [--version X.Y.Z]
    python3 ops/upgrade.py request --version X.Y.Z --key UNIQUE_REQUEST_KEY
    python3 ops/upgrade.py status [--operation UUID]
    python3 ops/upgrade.py watch [--operation UUID] [--timeout 600]

The OS account owns this action: no browser cookie or Web user is impersonated. Only the installed
signed Hub reads signing credentials. This script validates the installation, records the same request
contract as Updates::request, and asks launchd to run that Hub independently of this terminal.
"""
import argparse
import contextlib
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import platform
import plistlib
import pwd
import re
import shlex
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

TERMINAL = {"succeeded", "failed", "rolled_back"}
PYTHON_DIRS = ("/opt/homebrew/bin", "/usr/local/bin",
               "/Library/Frameworks/Python.framework/Versions/Current/bin", "/usr/bin")
SAFE_PATH = ":".join((*PYTHON_DIRS, "/bin", "/usr/sbin", "/sbin"))
DEFAULT_REPOSITORY = "Infranite/steve"
MAX_JSON = 1024 * 1024


class Refused(Exception):
    pass


def stable(value):
    if not isinstance(value, str) or not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", value):
        raise Refused("version must be a stable X.Y.Z without a prefix or suffix")
    return tuple(map(int, value.split(".")))


def read_json(path):
    if path.stat().st_size > MAX_JSON:
        raise Refused("metadata exceeds size limit: " + str(path))
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise Refused("metadata must be an object: " + str(path))
    return value


def atomic(path, value):
    data = value if isinstance(value, bytes) else (json.dumps(value, indent=2) + "\n").encode()
    tmp = path.with_name("." + uuid.uuid4().hex + ".tmp")
    try:
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(fd, "wb") as out:
            out.write(data)
            out.flush()
            os.fsync(out.fileno())
        os.replace(tmp, path)
        fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)
    finally:
        tmp.unlink(missing_ok=True)


class GitHubRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        check_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def check_url(url):
    parsed = urllib.parse.urlparse(url)
    if (parsed.scheme != "https" or parsed.username or parsed.password
            or not (parsed.hostname in ("github.com", "api.github.com")
                    or (parsed.hostname or "").endswith(".githubusercontent.com"))):
        raise Refused("release requests require GitHub HTTPS")


def fetch_json(url):
    check_url(url)
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), GitHubRedirect())
    request = urllib.request.Request(url, headers={"User-Agent": "Steve-local-upgrade/1"})
    with opener.open(request, timeout=30) as response:
        data = response.read(MAX_JSON + 1)
    if len(data) > MAX_JSON:
        raise Refused("release metadata exceeds size limit")
    return json.loads(data)


class Upgrade:
    def __init__(self, *, account=None, system=None, run=None, fetch=None):
        # Injection is for isolated fixtures, never an environment variable or CLI override.
        self.uid = os.getuid()
        if self.uid == 0 or os.geteuid() != self.uid:
            raise Refused("run as the installation's OS user, without sudo or setuid")
        self.account = account or pwd.getpwuid(self.uid)
        if self.account.pw_uid != self.uid:
            raise Refused("OS account identity mismatch")
        if (system or platform.system()) != "Darwin":
            raise Refused("this command supports signed macOS Hubs; use the managed Hub update API on Linux")
        self.user_home = Path(self.account.pw_dir).resolve(strict=True)
        self.home = self.user_home / ".steve"
        self.actor = "local-os:" + str(self.uid)
        self.run = run or subprocess.run
        self.fetch = fetch or fetch_json
        self.env = dict(HOME=str(self.user_home), USER=self.account.pw_name, LOGNAME=self.account.pw_name,
                        PATH=SAFE_PATH, LANG="en_US.UTF-8", STEVE_HOME=str(self.home), NO_COLOR="1",
                        GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_NOSYSTEM="1", GIT_TERMINAL_PROMPT="0")
        self.refresh()

    def owned(self, path, directory=False):
        info = path.lstat()
        if (info.st_uid != self.uid or info.st_mode & 0o022 or stat.S_ISLNK(info.st_mode)
                or not (stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode))):
            raise Refused("installation path must be owned by this user, without links or group/other writes: " + str(path))
        return path

    def directory(self, path):
        if path != self.home:
            self.owned(path.parent, directory=True)
        if not path.exists() and not path.is_symlink():
            path.mkdir(mode=0o700, exist_ok=True)
        return self.owned(path, directory=True)

    def command(self, args, check=True):
        result = self.run([str(a) for a in args], env=self.env.copy(), stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, timeout=30)
        if check and result.returncode:
            raise Refused("command failed: " + str(args[0]) + ": " + result.stderr.strip())
        return result

    def settings(self, service_env):
        path = self.home / "steve.env"
        keys = ("STEVE_UPDATE_REPOSITORY", "STEVE_PORT", "STEVE_INSTALL_KIND")
        values = {k: service_env[k] for k in keys if k in service_env}
        seen = set()
        if path.exists() or path.is_symlink():
            self.owned(path)
            for line in path.read_text().splitlines():
                if not line.strip() or line.lstrip().startswith("#"):
                    continue
                match = re.match(r"^\s*(?:export\s+)?(STEVE_UPDATE_REPOSITORY|STEVE_PORT|STEVE_INSTALL_KIND)\s*=(.*)$", line)
                if not match:
                    if any(re.search(r"\b" + key + r"\b", line) for key in keys):
                        raise Refused("upgrade settings must use literal assignments, not shell expressions")
                    continue
                key, raw = match.groups()
                if key in seen or any(c in raw for c in "$`\\;\n\r"):
                    raise Refused("installation setting needs one literal assignment: " + key)
                seen.add(key)
                parts = shlex.split(raw, comments=True)
                if len(parts) != 1:
                    raise Refused("installation setting needs one literal value: " + key)
                values[key] = parts[0]
        if values.get("STEVE_INSTALL_KIND") == "external":
            raise Refused("installation is externally managed; use its deployment system")
        repository = values.get("STEVE_UPDATE_REPOSITORY", DEFAULT_REPOSITORY)
        if (not re.fullmatch(r"[A-Za-z0-9_-][A-Za-z0-9_.-]*/[A-Za-z0-9_-][A-Za-z0-9_.-]*", repository)
                or any(p.endswith(".") for p in repository.split("/"))):
            raise Refused("invalid installed STEVE_UPDATE_REPOSITORY")
        port = values.get("STEVE_PORT", "8440")
        if not port.isascii() or not port.isdigit() or not 1 <= int(port) <= 65535:
            raise Refused("invalid installed STEVE_PORT")
        return repository, port

    def signature(self, path, expected):
        self.owned(path)
        self.command(["/usr/bin/codesign", "--verify", "--strict", path])
        report = self.command(["/usr/bin/codesign", "-dv", path]).stderr
        found = next((line.partition("=")[2] for line in report.splitlines() if line.startswith("Identifier=")), None)
        if found != expected:
            raise Refused("installed signature identifier differs from signing/identity.json")

    def refresh(self):
        self.owned(self.user_home, directory=True)
        self.owned(self.home, directory=True)
        for name in ("signing", "releases", "bin"):
            self.owned(self.home / name, directory=True)
        identity = read_json(self.owned(self.home / "signing/identity.json"))
        for key in ("label", "hub_identifier", "keyhelper_identifier", "vault_service"):
            if not isinstance(identity.get(key), str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]{0,200}", identity[key]):
                raise Refused("invalid signing identity field: " + key)
        # Existence/ownership only: this process never reads a keychain or signing password.
        self.owned(self.home / "signing/steve-signing.keychain-db")
        current = self.home / "current"
        if current.lstat().st_uid != self.uid:
            raise Refused("current belongs to another OS user")
        release = current.resolve(strict=True)
        if release.parent != self.home / "releases":
            raise Refused("current must name a direct child of this installation's releases")
        self.owned(release, directory=True)
        self.owned(release / "bin", directory=True)
        binary = self.owned(release / "bin/steve-hub")
        self.signature(binary, identity["hub_identifier"])
        self.signature(self.home / "bin/steve-keyhelper", identity["keyhelper_identifier"])
        info = json.loads(self.command([binary, "--build-info"]).stdout)
        manifest = read_json(self.owned(release / "release.json"))
        stable(info.get("version"))
        if (not re.fullmatch(r"[0-9a-f]{40}", str(info.get("commit", "")))
                or any(manifest.get(k) != info.get(k) for k in ("version", "commit"))
                or info.get("target") != "aarch64-apple-darwin"):
            raise Refused("installed release and Hub build identities do not agree")
        capability = info.get("upgrade_runner")
        if capability is None:
            kind = "local_release"
        elif (isinstance(capability, dict) and type(capability.get("journal")) is int and capability["journal"] == 1
              and isinstance(capability.get("kinds"), list) and "release_bundle" in capability["kinds"]):
            kind = "release_bundle"
        else:
            raise Refused("unsupported upgrade_runner capability; refusing a silent legacy fallback")
        # Reuse service ownership rules, including the exact legacy signed run.sh definition.
        spec = importlib.util.spec_from_file_location("steve_service", Path(__file__).with_name("steve-service.py"))
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        service = module.Service("hub", self.home, system="Darwin", user_home=self.user_home, runner=self.run)
        self.owned(service.config.parent.parent, directory=True)
        self.owned(service.config.parent, directory=True)
        self.owned(service.config)
        try:
            service.check_owner()
        except module.Refused as error:
            raise Refused(str(error)) from error
        definition = plistlib.loads(service.config.read_bytes())
        if (definition.get("ProgramArguments") != ["/bin/zsh", str(self.home / "current/ops/run.sh")]
                or definition.get("Label") != identity["label"]
                or any(k in definition for k in ("Program", "RootDirectory", "UserName", "GroupName"))):
            raise Refused("signed Hub service does not use its installed run.sh")
        service_env = definition.get("EnvironmentVariables", {})
        if not isinstance(service_env, dict) or any(not isinstance(v, str) for v in service_env.values()):
            raise Refused("invalid installed service environment")
        if "HOME" in service_env and Path(service_env["HOME"]).resolve() != self.user_home:
            raise Refused("installed service names another account home")
        repository, port = self.settings(service_env)
        self.env.update(STEVE_UPDATE_REPOSITORY=repository, STEVE_PORT=port)
        record = self.home / "services/hub.json"
        if record.exists() or record.is_symlink():
            self.owned(self.home / "services", directory=True)
            self.owned(record)
            if read_json(record).get("label") != identity["label"]:
                raise Refused("Hub service metadata differs from signing identity")
        self.identity, self.release, self.binary, self.info = identity, release, binary, info
        self.kind, self.repository = kind, repository

    def published(self, version=None):
        endpoint = "latest" if version is None else "tags/v" + version
        fallback = False
        try:
            release = self.fetch(f"https://api.github.com/repos/{self.repository}/releases/{endpoint}")
        except urllib.error.HTTPError as error:
            if version is None or error.code not in (403, 429):
                raise Refused("release API unavailable; an explicit --version can use published assets when rate limited") from error
            # Same published-tag fallback as Updates::resolve; never acquire a GitHub token.
            fallback = True
            release = dict(tag_name="v" + version, draft=False, prerelease=False, assets=[])
        if (not isinstance(release, dict) or release.get("draft") is not False
                or release.get("prerelease") is not False or not isinstance(release.get("tag_name"), str)):
            raise Refused("release is not a published stable target")
        tag = release["tag_name"]
        selected = tag[1:] if tag.startswith("v") else ""
        stable(selected)
        if version is not None and selected != version:
            raise Refused("release tag differs from requested version")
        manifest = self.fetch(f"https://github.com/{self.repository}/releases/download/{tag}/steve-release.json")
        if (not isinstance(manifest, dict) or manifest.get("format_version") != 1
                or any(manifest.get(k) != selected for k in ("version", "hub_version", "web_version"))
                or manifest.get("api_protocol") != 1
                or not re.fullmatch(r"[0-9a-f]{40}", str(manifest.get("commit", "")))):
            raise Refused("invalid published release identity")
        schema = manifest.get("schema", {})
        if (not isinstance(schema, dict) or any(type(schema.get(k)) is not int or schema[k] < 0 for k in ("min_read", "max_read", "target"))
                or not schema["min_read"] <= schema["target"] <= schema["max_read"]):
            raise Refused("invalid release schema compatibility")
        target = self.info["target"]
        assets = [a for a in manifest.get("assets", []) if isinstance(a, dict) and a.get("target") == target]
        expected = f"steve-{selected}-{target}.tar.gz"
        published_names = {a.get("name") for a in release.get("assets", []) if isinstance(a, dict)}
        if (len(assets) != 1 or assets[0].get("name") != expected
                or type(assets[0].get("size")) is not int or not 0 < assets[0]["size"] <= 512 * 1024 * 1024
                or not re.fullmatch(r"[0-9a-f]{64}", str(assets[0].get("sha256", "")))
                or (not fallback and not {expected, "steve-release.json"}.issubset(published_names))):
            raise Refused("published release has no valid matching bundle")
        return manifest

    def check(self, version=None):
        if version is not None:
            stable(version)
        manifest = self.published(version)
        return dict(current=self.info, version=manifest["version"], commit=manifest["commit"],
                    available=stable(manifest["version"]) > stable(self.info["version"]),
                    runner=self.kind, repository=self.repository, actor=self.actor)

    @contextlib.contextmanager
    def lock(self, path, release=False):
        self.owned(path.parent, directory=True)
        if path.exists() or path.is_symlink():
            self.owned(path)
        fd = os.open(path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, "r+") as handle:
            try:
                (fcntl.lockf if release else fcntl.flock)(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except OSError as e:
                raise Refused("another upgrade owns " + path.name) from e
            yield

    def operations(self):
        folder = self.home / "updates/operations"
        if not folder.exists():
            return []
        self.owned(folder, directory=True)
        return [self.operation(p.stem) for p in sorted(folder.glob("*.json"))]

    def operation(self, ident):
        if str(uuid.UUID(ident)) != ident:
            raise Refused("operation must be a canonical UUID")
        self.owned(self.home / "updates/operations", directory=True)
        value = read_json(self.owned(self.home / "updates/operations" / (ident + ".json")))
        if value.get("id") != ident or not isinstance(value.get("phase"), str):
            raise Refused("invalid operation journal")
        return value

    def active(self):
        path = self.home / "updates/active.json"
        return self.operation(read_json(self.owned(path))["id"]) if path.exists() or path.is_symlink() else None

    def existing(self, version, key):
        operations = self.operations()
        matches = [op for op in operations if op.get("idempotency_key") == key]
        if len(matches) > 1:
            raise Refused("duplicate idempotency records require inspection")
        existing = matches[0] if matches else None
        if existing and (existing.get("version") != version or existing.get("requested_by") != self.actor):
            raise Refused("idempotency key conflict")
        active = self.active()
        pending = [op for op in operations if op["phase"] not in TERMINAL and (not existing or op["id"] != existing["id"])]
        if existing:
            if existing["phase"] not in TERMINAL:
                if pending or (active and active["phase"] not in TERMINAL and active["id"] != existing["id"]):
                    raise Refused("another active operation conflicts with this retry")
                atomic(self.home / "updates/active.json", {"id": existing["id"]})
            return self.reap(existing)
        if pending or (active and active["phase"] not in TERMINAL):
            raise Refused("an upgrade or approval is already pending; no operation was changed")
        return None

    def job(self):
        result = self.command(["/bin/launchctl", "print", f"gui/{self.uid}/{self.identity['label']}.upgrade"], check=False)
        if result.returncode:
            if any(s in (result.stderr + result.stdout).lower() for s in ("could not find service", "no such process")):
                return "absent"
            return "unknown"
        if re.search(r"^\s*pid\s*=\s*[1-9][0-9]*\s*$", result.stdout, re.M):
            return "live"
        state = re.search(r"^\s*state\s*=\s*(.+)$", result.stdout, re.M)
        if state and state[1].strip() in ("exited", "not running"):
            path = re.search(r"^\s*path\s*=\s*(.+)$", result.stdout, re.M)
            if not path:
                return "unknown"
            file = Path(path[1].strip())
            try:
                if file.parent != self.home / "updates/runner":
                    return "foreign"
                self.owned(file.parent, directory=True)
                definition = plistlib.loads(self.owned(file).read_bytes())
                args = definition.get("ProgramArguments", [])
                op = self.operation(file.stem)
                if (definition.get("Label") != self.identity["label"] + ".upgrade" or len(args) != 4
                        or args[1:] != ["--upgrade-runner", str(self.home), op["id"]]
                        or Path(args[0]).parent.parent.parent != self.home / "releases"
                        or "Program" in definition):
                    return "foreign"
            except (OSError, ValueError, KeyError, Refused):
                return "unknown"
            return "exited"
        return "unknown"

    def reap(self, op):
        """Match the Hub's runner grace, without retrying possibly executed work. Caller holds journal.lock."""
        if op["phase"] in TERMINAL or op.get("requested_by") != self.actor or op["phase"] == "awaiting_approval":
            return op
        spec = op.get("runner") or {}
        since = max(spec.get("launched_at") or 0, op.get("updated_at") or 0)
        if int(time.time() * 1000) - since < 60_000:
            return op
        try:
            with self.lock(self.home / "updates/runner.lock"), self.lock(self.home / "releases/.release.lock", release=True):
                if self.job() not in ("absent", "exited"):
                    return op
                fresh = self.operation(op["id"])
                if fresh != op:
                    return fresh
                fresh.update(phase="failed", updated_at=int(time.time() * 1000),
                             error="Upgrade runner disappeared without a result; it was not restarted. "
                             "Inspect current, the recorded backup and runner log before using a new request key.")
                atomic(self.home / "updates/operations" / (op["id"] + ".json"), fresh)
                return fresh
        except Refused:
            return op

    def interpreter(self):
        code = "import sys,ssl,sqlite3; assert sys.version_info >= (3,9)"
        if self.kind == "local_release":
            code += "; import tarfile; tarfile.data_filter"
        for directory in PYTHON_DIRS:
            for name in ("python3.13", "python3.12", "python3"):
                path = Path(directory) / name
                if path.is_file() and self.command([path, "-I", "-c", code], check=False).returncode == 0:
                    return str(path)
        raise Refused("no trusted installer Python; legacy runners also require tarfile.data_filter")

    def source_cache(self):
        cache = self.home / "updates/source-cache.git"
        if cache.exists() or cache.is_symlink():
            self.owned(cache, directory=True)
            config = self.owned(cache / "config").read_text()
            expected = "[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = true\n"
            if config != expected:
                raise Refused("legacy object cache configuration changed; inspect it before retry")
        else:
            self.command(["/usr/bin/git", "-c", "init.templateDir=", "init", "--bare", cache])
            # Normalize platform-specific git-init defaults; no repository-local hooks, includes,
            # remotes, rewrites or credential configuration are part of this object-only cache.
            atomic(cache / "config", b"[core]\n\trepositoryformatversion = 0\n\tfilemode = true\n\tbare = true\n")
        return str(cache)

    def request(self, version, key):
        stable(version)
        if not key or len(key.encode()) > 200:
            raise Refused("idempotency key required (1..200 bytes)")
        updates = self.directory(self.home / "updates")
        self.directory(updates / "operations")
        with self.lock(updates / "journal.lock"):
            prior = self.existing(version, key)
            if prior:
                return prior
        # No journal or runner lock is held during remote I/O. Revalidate the installation below.
        manifest = self.published(version)
        snapshot = self.binary, self.info, self.repository, self.identity
        with self.lock(updates / "journal.lock"):
            self.refresh()
            if snapshot != (self.binary, self.info, self.repository, self.identity):
                raise Refused("installation changed during release check; retry from its new state")
            prior = self.existing(version, key)
            if prior:
                return prior
            if stable(version) <= stable(self.info["version"]):
                raise Refused("upgrade target must be newer than the installed Hub")
            with self.lock(updates / "runner.lock"), self.lock(self.home / "releases/.release.lock", release=True):
                job = self.job()
                if job not in ("absent", "exited"):
                    raise Refused("upgrade job is live or unknown; it was not stopped")
                python = self.interpreter()
                commit = manifest["commit"]
                candidate = self.home / "releases" / f"{version}-local-{commit[:7]}"
                if self.kind == "local_release" and (candidate.exists() or candidate.is_symlink()):
                    raise Refused("legacy candidate already exists; inspect it before creating another request")
                repo = self.source_cache() if self.kind == "local_release" else ""
                ident, now = str(uuid.uuid4()), int(time.time() * 1000)
                self.directory(self.home / "logs")
                self.directory(self.home / "logs/release")
                self.directory(updates / "runner")
                log = self.home / "logs/release" / f"upgrade-{version}-{ident[:8]}.log"
                fd = os.open(log, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                os.close(fd)
                runner = dict(kind=self.kind, repo=repo, source=f"https://github.com/{self.repository}.git",
                              commit=commit, python=python, cargo="", log=str(log), launched_at=now)
                op = dict(id=ident, idempotency_key=key, version=version, phase="staging", created_at=now,
                          updated_at=now, requested_by=self.actor, approved_by=self.actor, approved_at=now,
                          error=None, previous=None, target=None, runner=runner)
                label = self.identity["label"] + ".upgrade"
                plist = updates / "runner" / (ident + ".plist")
                definition = dict(Label=label, ProgramArguments=[str(self.binary), "--upgrade-runner", str(self.home), ident],
                                  EnvironmentVariables=self.env, RunAtLoad=True, AbandonProcessGroup=True,
                                  ProcessType="Standard", StandardOutPath=str(log), StandardErrorPath=str(log))
                atomic(plist, plistlib.dumps(definition))
                # Stop only an observed exited job while holding the runner lock, never a live one.
                if job == "exited":
                    self.command(["/bin/launchctl", "bootout", f"gui/{self.uid}/{label}"])
                atomic(updates / "operations" / (ident + ".json"), op)
                atomic(updates / "active.json", {"id": ident})
            # The runner must be able to take runner.lock immediately. journal.lock still excludes
            # another request, and active.json owns the operation before launchd can start it.
            try:
                self.command(["/bin/launchctl", "bootstrap", f"gui/{self.uid}", plist])
            except (Refused, OSError, subprocess.TimeoutExpired) as error:
                fresh = self.operation(ident)
                if fresh["phase"] not in TERMINAL:
                    try:
                        with self.lock(updates / "runner.lock"), self.lock(self.home / "releases/.release.lock", release=True):
                            if self.operation(ident) != fresh:
                                return self.operation(ident)
                            fresh["error"] = "launch result: " + str(error)
                            if self.job() == "absent":
                                fresh["phase"] = "failed"
                            fresh["updated_at"] = int(time.time() * 1000)
                            atomic(updates / "operations" / (ident + ".json"), fresh)
                    except Refused:
                        pass  # A runner started despite the launch error; it owns the journal now.
                return self.operation(ident)
            return self.operation(ident)

    def status(self, ident=None):
        if (self.home / "current").resolve(strict=True) != self.release:
            self.refresh()
        updates = self.home / "updates"
        if updates.exists():
            self.owned(updates, directory=True)
        operation = self.operation(ident) if ident else self.active()
        if operation and operation["phase"] not in TERMINAL:
            with self.lock(updates / "journal.lock"):
                operation = self.reap(self.operation(operation["id"]))
        return dict(actor=self.actor, current=self.info, operation=operation, job=self.job())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check").add_argument("--version")
    request = commands.add_parser("request")
    request.add_argument("--version", required=True)
    request.add_argument("--key", required=True)
    for name in ("status", "watch"):
        command = commands.add_parser(name)
        command.add_argument("--operation")
        if name == "watch":
            command.add_argument("--timeout", type=float, default=600)
    args = parser.parse_args()
    upgrade = Upgrade()
    if args.command == "check":
        print(json.dumps(upgrade.check(args.version), ensure_ascii=False))
    elif args.command == "request":
        result = upgrade.request(args.version, args.key)
        print(json.dumps(result, ensure_ascii=False))
        return 1 if result["phase"] in {"failed", "rolled_back"} else 0
    elif args.command == "status":
        print(json.dumps(upgrade.status(args.operation), ensure_ascii=False))
    else:
        if not 0 < args.timeout <= 86400:
            raise Refused("watch timeout must be within 0..86400 seconds")
        deadline, last, ident = time.monotonic() + args.timeout, None, args.operation
        while True:
            result = upgrade.status(ident)
            op = result["operation"]
            if op:
                ident = op["id"]
            serialized = json.dumps(result, ensure_ascii=False)
            if serialized != last:
                print(serialized, flush=True)
                last = serialized
            if op and op["phase"] in TERMINAL:
                return 0 if op["phase"] == "succeeded" else 1
            if time.monotonic() >= deadline:
                return 2
            time.sleep(1)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (Refused, OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(json.dumps({"error": str(error)}, ensure_ascii=False), file=sys.stderr)
        sys.exit(1)
