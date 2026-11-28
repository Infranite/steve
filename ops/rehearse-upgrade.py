#!/usr/bin/env python3
"""One-command end-to-end upgrade rehearsal for Steve maintainers.

Installs a previous release in an isolated temporary home and unique service label,
applies the broken-setup workaround only when needed for pre-#634 releases, seeds
representative data and permission policy state, upgrades with the candidate bundle,
verifies health, Web UI, data, permission tier, and local Node, exercises rollback
and re-upgrade plus confirmation (macOS), and always cleans up without touching the
operator's own Steve installation.
"""

from __future__ import annotations

import argparse
import ctypes
from dataclasses import dataclass, field
from datetime import datetime, timezone
import hashlib
import http.client
import json
import os
from pathlib import Path
import plistlib
import random
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.request
import uuid

DEFAULT_REPO = "Infranite/steve"
DEFAULT_TIMEOUT_SECS = 180
MIN_TIMEOUT_SECS = 10
MAX_TIMEOUT_SECS = 3600

# Hub uses P (API), P+1 (Web), P+2 (tailcat), and P+3 (local Node) concurrently.
PORT_RANGE_START = 19100
PORT_RANGE_END = 29000
CONTIGUOUS_PORT_SPAN = 4

HEALTH_POLL_INTERVAL_SECS = 0.5
HTTP_REQUEST_TIMEOUT_SECS = 10
DOWNLOAD_TIMEOUT_SECS = 120
SUBPROCESS_DEFAULT_TIMEOUT_SECS = 120
SERVICE_STOP_TIMEOUT_SECS = 60
SERVICE_UNINSTALL_TIMEOUT_SECS = 60
LAUNCHD_UNLOAD_WAIT_SECS = 15.0
DB_INIT_BOOT_TIMEOUT_SECS = 30
PROCESS_TERM_GRACE_SECS = 15
PROCESS_KILL_WAIT_SECS = 5
LOCAL_NODE_WAIT_SECS = 60
MAX_MANIFEST_BYTES = 1024 * 1024
MAX_BUNDLE_BYTES = 512 * 1024 * 1024
DIAGNOSTIC_LOG_TAIL_CHARS = 4000
MACOS_CTL_KERN = 1
MACOS_KERN_PROCARGS2 = 49
PROC_PIDPATHINFO_MAXSIZE = 4096

DEFAULT_SERVICE_LABEL = "io.github.infranite.steve.hub"
REHEARSAL_LABEL_PREFIX = "io.github.infranite.steve-rehearsal-"

HARNESS_STATE_VARS: dict[str, str] = {
    "CLAUDE_CONFIG_DIR": "claude",
    "CODEX_HOME": "codex",
    "GEMINI_CLI_HOME": "gemini",
    "PI_CODING_AGENT_DIR": "pi",
    "COPILOT_HOME": "copilot",
    "QWEN_HOME": "qwen",
    "GOOSE_PATH_ROOT": "goose",
    "KIMI_CODE_HOME": "kimi",
    "GROK_HOME": "grok",
    "FACTORY_HOME_OVERRIDE": "factory",
    "CODEBUDDY_CONFIG_DIR": "codebuddy",
    "CURSOR_CONFIG_DIR": "cursor",
    "KIRO_HOME": "kiro",
    "VIBE_HOME": "vibe",
    "QODER_CONFIG_DIR": "qoder",
}

SEMVER_RE = re.compile(r"^v?((?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*))$")
COMMIT_SHA_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
ENV_KEY_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
WEB_VERSION_META_RE_TEMPLATE = r"""<meta\s+name=["']steve-web-version["']\s+content=["']{version}["']"""

EXIT_OK = 0
EXIT_REHEARSAL_FAILED = 1
EXIT_CLEANUP_OR_USAGE_ERROR = 2


class RehearsalError(RuntimeError):
    """Typed failure inside a rehearsal step or precondition check."""


class RehearsalInterrupted(BaseException):
    """Raised on SIGINT/SIGTERM so cleanup still runs reliably."""

    def __init__(self, signum: int) -> None:
        self.signum = signum
        super().__init__(f"interrupted by signal {signum}")


@dataclass
class BundleInfo:
    """Validated unpacked Steve release or installer bundle."""

    root: Path
    version: str
    commit: str
    target: str
    hub_bin: Path
    node_bin: Path
    cli_bin: Path | None
    web_dir: Path
    has_legacy_run_sh: bool


@dataclass
class SeededData:
    """Deterministic pre-upgrade state created on the previous release."""

    user: str
    credential: str
    workspace_id: str
    bot_id: str
    bot_name: str
    memory_id: str
    memory_text: str
    tag_id: str
    tag_name: str
    item_id: str
    item_title: str
    item_goal: str
    prev_policy_api: str
    prev_policy_mode: str
    expected_candidate_mode: str


@dataclass
class StepResult:
    """Outcome of one numbered rehearsal phase."""

    name: str
    status: str  # "pass" | "fail" | "skip"
    duration_s: float
    summary: str
    details: dict[str, object] = field(default_factory=dict)

    def to_dict(self) -> dict[str, object]:
        return {
            "name": self.name,
            "status": self.status,
            "duration_s": round(self.duration_s, 3),
            "summary": self.summary,
            "details": self.details,
        }


def detect_target() -> tuple[str, str]:
    """Return (platform_key, rust_target) or fail on unsupported OS/arch."""
    uname = os.uname()
    sysname = uname.sysname.lower()
    machine = uname.machine.lower()
    if sysname == "darwin" and machine in ("arm64", "aarch64"):
        return "macos", "aarch64-apple-darwin"
    if sysname == "linux" and machine in ("x86_64", "amd64"):
        return "linux", "x86_64-unknown-linux-gnu"
    raise RehearsalError(
        f"unsupported platform {uname.sysname}/{uname.machine}; "
        "supported platforms are Apple Silicon macOS and x86_64 Linux"
    )


def clean_subprocess_env(extra: dict[str, str] | None = None) -> dict[str, str]:
    """Build an isolated environment stripping STEVE_* and ambient harness overrides."""
    blocked_exact = set(HARNESS_STATE_VARS.keys()) | {
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
        "CLAUDE_CODE_EXECUTABLE",
        "CODEX_EXECUTABLE",
        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    }
    env: dict[str, str] = {}
    for key, value in os.environ.items():
        if key.startswith("STEVE_") or key in blocked_exact:
            continue
        env[key] = value
    env["STEVE_PLAIN"] = "1"
    env["NO_COLOR"] = "1"
    if extra:
        env.update(extra)
    return env


def loopback_opener() -> urllib.request.OpenerDirector:
    """Build an opener that bypasses HTTP_PROXY/ALL_PROXY for loopback probes."""
    return urllib.request.build_opener(urllib.request.ProxyHandler({}))


def parse_env_file(path: Path) -> dict[str, str]:
    """Parse literal KEY=value lines from steve.env without shell evaluation."""
    if not path.is_file():
        return {}
    parsed: dict[str, str] = {}
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export ") or line.startswith("export\t"):
            line = line[len("export"):].lstrip(" \t")
        if "=" not in line:
            continue
        key, raw_val = line.split("=", 1)
        key = key.strip()
        if not ENV_KEY_RE.match(key):
            continue
        val = raw_val.strip()
        if len(val) >= 2 and val[0] == "'" and val[-1] == "'":
            val = val[1:-1]
        elif len(val) >= 2 and val[0] == '"' and val[-1] == '"':
            val = val[1:-1]
        elif " #" in val or "\t#" in val:
            val = re.split(r"[ \t]#", val, maxsplit=1)[0].rstrip()
        parsed[key] = val
    return parsed


def format_env_assignment(key: str, value: str) -> str:
    """Format a literal KEY=value assignment accepted by both zsh and steve env_file.rs."""
    if not ENV_KEY_RE.match(key):
        raise RehearsalError(f"invalid environment variable key: {key}")
    if "\n" in value or "\r" in value or "\0" in value:
        raise RehearsalError(f"environment value for {key} contains control characters")
    if "'" in value:
        escaped = value.replace("\\", "\\\\").replace('"', '\\"').replace("$", "\\$").replace("`", "\\`")
        return f'{key}="{escaped}"'
    return f"{key}='{value}'"


def append_isolated_env(env_path: Path, additions: dict[str, str]) -> list[str]:
    """Append non-duplicate isolated environment variables to steve.env."""
    existing_text = env_path.read_text(encoding="utf-8") if env_path.is_file() else ""
    existing = parse_env_file(env_path)
    lines: list[str] = []
    added_keys: list[str] = []
    for key, value in additions.items():
        if key in existing:
            continue
        lines.append(format_env_assignment(key, value))
        added_keys.append(key)
    if not lines:
        return added_keys
    prefix = "" if not existing_text or existing_text.endswith("\n") else "\n"
    with env_path.open("a", encoding="utf-8") as handle:
        handle.write(prefix + "\n".join(lines) + "\n")
    return added_keys


def allocate_port_block() -> int:
    """Find a free contiguous block [P, P+1, P+2, P+3] on 127.0.0.1."""
    span = CONTIGUOUS_PORT_SPAN
    max_start = PORT_RANGE_END - span
    total = max_start - PORT_RANGE_START + 1
    offset = random.randrange(total)
    for step in range(total):
        base = PORT_RANGE_START + ((offset + step) % total)
        sockets: list[socket.socket] = []
        try:
            for delta in range(span):
                sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
                sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
                sock.bind(("127.0.0.1", base + delta))
                sockets.append(sock)
            return base
        except OSError:
            continue
        finally:
            for sock in sockets:
                sock.close()
    raise RehearsalError(
        f"no contiguous {span}-port block available on 127.0.0.1 in {PORT_RANGE_START}..{PORT_RANGE_END}"
    )


def validate_sandbox_paths(work_dir: Path | None) -> Path:
    """Resolve work_dir and reject any overlap with operator ~/.steve or ~/.steve-node."""
    user_home = Path.home().resolve()
    forbidden_roots = [
        (user_home / ".steve").resolve(),
        (user_home / ".steve-node").resolve(),
    ]
    if work_dir is not None:
        resolved = work_dir.expanduser().resolve()
        for forbidden in forbidden_roots:
            if resolved == forbidden or resolved.is_relative_to(forbidden) or forbidden.is_relative_to(resolved):
                raise RehearsalError(
                    f"--work-dir ({resolved}) overlaps operator installation ({forbidden})"
                )
        resolved.mkdir(parents=True, exist_ok=True)
        return resolved
    return Path(tempfile.gettempdir()).resolve()


def _validate_tar_member(member: tarfile.TarInfo) -> Path:
    raw = member.name
    if not raw or raw.startswith("/") or raw.startswith("\\"):
        raise RehearsalError(f"unsafe absolute tar entry: {raw}")
    if ":" in raw or "\0" in raw:
        raise RehearsalError(f"unsafe tar entry path: {raw}")
    rel = Path(raw)
    if any(part in ("", "..") for part in rel.parts if part != "."):
        raise RehearsalError(f"unsafe tar traversal entry: {raw}")
    if not (member.isdir() or member.isreg()):
        raise RehearsalError(f"tar entry must be regular file or directory: {raw} (type={member.type!r})")
    return rel


def safe_extract_tar_gz(archive: Path, dest: Path) -> Path:
    """Validate every archive entry before unpacking into dest."""
    dest.mkdir(parents=True, exist_ok=True)
    dest_resolved = dest.resolve()
    with tarfile.open(archive, "r:gz") as tf:
        members = tf.getmembers()
        for member in members:
            rel = _validate_tar_member(member)
            target = (dest_resolved / rel).resolve()
            if target != dest_resolved and not target.is_relative_to(dest_resolved):
                raise RehearsalError(f"tar entry escapes destination: {member.name}")
        for member in members:
            rel = Path(member.name)
            if str(rel) in (".", ""):
                continue
            target = dest_resolved / rel
            if member.isdir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            src = tf.extractfile(member)
            if src is None:
                raise RehearsalError(f"cannot read tar member bytes: {member.name}")
            with src, target.open("wb") as out:
                shutil.copyfileobj(src, out)
            mode = 0o755 if (member.mode & 0o111) else 0o644
            target.chmod(mode)
    return dest_resolved


def _http_download_bytes(url: str, max_bytes: int, timeout: int) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "steve-upgrade-rehearsal/1"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            chunks: list[bytes] = []
            total = 0
            while True:
                chunk = resp.read(65536)
                if not chunk:
                    break
                total += len(chunk)
                if total > max_bytes:
                    raise RehearsalError(f"download exceeded {max_bytes} bytes: {url}")
                chunks.append(chunk)
            return b"".join(chunks)
    except urllib.error.HTTPError as exc:
        raise RehearsalError(f"HTTP {exc.code} downloading {url}: {exc.reason}") from exc
    except (urllib.error.URLError, TimeoutError, OSError) as exc:
        raise RehearsalError(f"failed to download {url}: {exc}") from exc


def _atomic_write_bytes(path: Path, data: bytes, mode: int = 0o644) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.parent / f".{path.name}.{uuid.uuid4().hex}.tmp"
    try:
        with os.fdopen(os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, mode), "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink(missing_ok=True)


def download_release_bundle(
    repo: str,
    version: str,
    target: str,
    kind: str,
    dest_dir: Path,
    cache_dir: Path | None,
) -> Path:
    """Download and SHA-256-verify a GitHub release tarball, then unpack into dest_dir."""
    if not re.fullmatch(r"[A-Za-z0-9._-]+/[A-Za-z0-9._-]+", repo):
        raise RehearsalError(f"invalid --repo value: {repo}")
    manifest_name = "steve-installer.json" if kind == "installer" else "steve-release.json"
    base_url = f"https://github.com/{repo}/releases/download/v{version}"
    version_cache = (cache_dir / f"v{version}") if cache_dir is not None else None

    manifest_bytes: bytes | None = None
    manifest_cache = (version_cache / manifest_name) if version_cache is not None else None
    if manifest_cache is not None and manifest_cache.is_file():
        manifest_bytes = manifest_cache.read_bytes()
    else:
        manifest_bytes = _http_download_bytes(
            f"{base_url}/{manifest_name}", MAX_MANIFEST_BYTES, DOWNLOAD_TIMEOUT_SECS
        )
        if manifest_cache is not None:
            _atomic_write_bytes(manifest_cache, manifest_bytes)

    try:
        manifest = json.loads(manifest_bytes.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RehearsalError(f"invalid {manifest_name} for v{version}: {exc}") from exc

    if manifest.get("format_version") != 1 or manifest.get("version") != version:
        raise RehearsalError(
            f"{manifest_name} version/format mismatch for v{version}: {manifest.get('version')!r}"
        )
    asset = next(
        (entry for entry in manifest.get("assets", []) if entry.get("target") == target),
        None,
    )
    if asset is None:
        raise RehearsalError(f"release v{version} ({manifest_name}) has no asset for {target}")
    asset_name = str(asset.get("name", ""))
    expected_sha = str(asset.get("sha256", "")).lower()
    expected_size = asset.get("size")
    if (
        not asset_name
        or "/" in asset_name
        or "\\" in asset_name
        or ".." in asset_name
        or not SHA256_RE.match(expected_sha)
        or not isinstance(expected_size, int)
        or expected_size <= 0
        or expected_size > MAX_BUNDLE_BYTES
    ):
        raise RehearsalError(f"invalid asset metadata in {manifest_name} for {target}: {asset!r}")

    archive_cache = (version_cache / asset_name) if version_cache is not None else None
    archive_path: Path | None = None
    if archive_cache is not None and archive_cache.is_file():
        cached_bytes = archive_cache.read_bytes()
        if (
            len(cached_bytes) == expected_size
            and hashlib.sha256(cached_bytes).hexdigest() == expected_sha
        ):
            archive_path = archive_cache
        else:
            archive_cache.unlink(missing_ok=True)

    if archive_path is None:
        archive_bytes = _http_download_bytes(
            f"{base_url}/{asset_name}", MAX_BUNDLE_BYTES, DOWNLOAD_TIMEOUT_SECS
        )
        actual_sha = hashlib.sha256(archive_bytes).hexdigest()
        if len(archive_bytes) != expected_size or actual_sha != expected_sha:
            raise RehearsalError(
                f"SHA-256/size mismatch for {asset_name}: "
                f"expected size={expected_size} sha256={expected_sha}, "
                f"got size={len(archive_bytes)} sha256={actual_sha}"
            )
        if archive_cache is not None:
            _atomic_write_bytes(archive_cache, archive_bytes)
            archive_path = archive_cache
        else:
            tmp_tar = dest_dir.parent / f"{asset_name}.download"
            _atomic_write_bytes(tmp_tar, archive_bytes)
            archive_path = tmp_tar
            try:
                return safe_extract_tar_gz(archive_path, dest_dir)
            finally:
                tmp_tar.unlink(missing_ok=True)

    return safe_extract_tar_gz(archive_path, dest_dir)


def run_command(
    cmd: list[str],
    *,
    timeout: float = SUBPROCESS_DEFAULT_TIMEOUT_SECS,
    env: dict[str, str] | None = None,
    cwd: Path | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run a command with isolated stdin and bounded timeout."""
    proc_env = clean_subprocess_env() if env is None else env
    try:
        proc = subprocess.run(
            cmd,
            cwd=str(cwd) if cwd is not None else None,
            env=proc_env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired as exc:
        raise RehearsalError(f"command timed out after {timeout}s: {' '.join(cmd)}") from exc
    except OSError as exc:
        raise RehearsalError(f"failed to run {' '.join(cmd)}: {exc}") from exc
    if check and proc.returncode != 0:
        raise RehearsalError(
            f"command failed (exit={proc.returncode}): {' '.join(cmd)}\n"
            f"stdout: {proc.stdout.strip()}\nstderr: {proc.stderr.strip()}"
        )
    return proc


def parse_json_output(stdout: str, context: str) -> dict[str, object]:
    """Parse JSON object from command stdout."""
    text = stdout.strip()
    if not text:
        raise RehearsalError(f"{context} produced empty stdout instead of JSON")
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        lines = [line.strip() for line in text.splitlines() if line.strip()]
        parsed = None
        for line in reversed(lines):
            try:
                parsed = json.loads(line)
                break
            except json.JSONDecodeError:
                continue
        if parsed is None:
            raise RehearsalError(f"{context} did not emit valid JSON: {text[:500]}")
    if not isinstance(parsed, dict):
        raise RehearsalError(f"{context} returned non-object JSON: {parsed!r}")
    return parsed


def validate_bundle(bundle_dir: Path, target: str, require_cli: bool) -> BundleInfo:
    """Validate structure and --build-info identity of an unpacked bundle."""
    root = bundle_dir.resolve()
    manifest_path = root / "release.json"
    if not manifest_path.is_file():
        raise RehearsalError(f"bundle missing release.json: {root}")
    try:
        manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise RehearsalError(f"invalid release.json in {root}: {exc}") from exc

    version = str(manifest.get("version", ""))
    commit = str(manifest.get("commit", ""))
    if manifest.get("format_version") != 1 or not SEMVER_RE.match(version) or not COMMIT_SHA_RE.match(commit):
        raise RehearsalError(f"invalid release metadata in {manifest_path}: {manifest!r}")

    hub_bin = root / "bin/steve-hub"
    node_bin = root / "bin/steve-node"
    cli_bin = root / "bin/steve"
    restic_bin = root / "bin/restic"
    web_dir = root / "web/dist"
    web_index = web_dir / "index.html"

    required_execs = [hub_bin, node_bin, restic_bin]
    if require_cli:
        required_execs.append(cli_bin)
    if target.endswith("apple-darwin"):
        required_execs.append(root / "bin/steve-keyhelper")

    for exe in required_execs:
        if not exe.is_file() or not os.access(exe, os.X_OK):
            raise RehearsalError(f"bundle missing executable {exe}")

    if not web_index.is_file():
        raise RehearsalError(f"bundle missing web UI index: {web_index}")
    html = web_index.read_text(encoding="utf-8", errors="replace")
    meta_re = re.compile(WEB_VERSION_META_RE_TEMPLATE.format(version=re.escape(version)))
    if not meta_re.search(html):
        raise RehearsalError(f"{web_index} does not declare steve-web-version={version}")

    for role, binary in (("steve-hub", hub_bin), ("steve-node", node_bin)):
        proc = run_command([str(binary), "--build-info"], timeout=30)
        info = parse_json_output(proc.stdout, f"{binary} --build-info")
        if info.get("version") != version or info.get("commit") != commit or info.get("target") != target:
            raise RehearsalError(
                f"{role} --build-info mismatch in {root}: expected ({version}, {commit}, {target}), "
                f"got ({info.get('version')}, {info.get('commit')}, {info.get('target')})"
            )

    if cli_bin.is_file() and os.access(cli_bin, os.X_OK):
        proc = run_command([str(cli_bin), "--build-info"], timeout=30)
        info = parse_json_output(proc.stdout, f"{cli_bin} --build-info")
        if info.get("version") != version or info.get("commit") != commit or info.get("target") != target:
            raise RehearsalError(f"steve CLI --build-info mismatch in {root}: {info!r}")
        resolved_cli: Path | None = cli_bin
    else:
        resolved_cli = None

    return BundleInfo(
        root=root,
        version=version,
        commit=commit,
        target=target,
        hub_bin=hub_bin,
        node_bin=node_bin,
        cli_bin=resolved_cli,
        web_dir=web_dir,
        has_legacy_run_sh=(root / "ops/run.sh").is_file(),
    )


def resolve_bundle_input(
    spec: str,
    *,
    role: str,
    repo: str,
    target: str,
    kind: str,
    extract_dir: Path,
    cache_dir: Path | None,
    require_cli: bool,
) -> BundleInfo:
    """Resolve a SemVer tag, local .tar.gz archive, or local extracted bundle directory."""
    candidate_path = Path(spec).expanduser()
    if candidate_path.exists():
        resolved = candidate_path.resolve()
        if resolved.is_dir():
            return validate_bundle(resolved, target, require_cli=require_cli)
        if resolved.is_file() and resolved.name.endswith(".tar.gz"):
            unpacked = safe_extract_tar_gz(resolved, extract_dir)
            return validate_bundle(unpacked, target, require_cli=require_cli)
        raise RehearsalError(f"--{role} path must be a directory or .tar.gz archive: {resolved}")

    match = SEMVER_RE.match(spec.strip())
    if not match:
        raise RehearsalError(
            f"--{role} must be an existing directory, .tar.gz file, or SemVer version (got {spec!r})"
        )
    version = match.group(1)
    unpacked = download_release_bundle(repo, version, target, kind, extract_dir, cache_dir)
    return validate_bundle(unpacked, target, require_cli=require_cli)


def preflight_linux_systemd() -> None:
    """Ensure systemd --user is reachable before starting a Linux rehearsal."""
    proc = run_command(["systemctl", "--user", "show-environment"], check=False, timeout=15)
    if proc.returncode != 0:
        raise RehearsalError(
            "Linux rehearsal requires an active systemd --user session "
            f"(systemctl --user show-environment failed: {proc.stderr.strip() or proc.stdout.strip()})"
        )


def snapshot_operator_state(platform_key: str) -> dict[str, object]:
    """Read-only snapshot of operator's default Steve home and service definition."""
    user_home = Path.home().resolve()
    steve_home = user_home / ".steve"
    current_link = steve_home / "current"
    env_file = steve_home / "steve.env"

    def stat_sig(path: Path) -> list[int] | str | None:
        try:
            st = path.lstat()
        except FileNotFoundError:
            return None
        except PermissionError:
            return "<protected>"
        return [st.st_mode, st.st_size, st.st_mtime_ns]

    try:
        current_target: str | None = os.readlink(current_link)
    except FileNotFoundError:
        current_target = None
    except PermissionError:
        current_target = "<protected>"
    except OSError:
        current_target = None

    try:
        steve_home.lstat()
        steve_home_exists: bool | str = True
    except FileNotFoundError:
        steve_home_exists = False
    except PermissionError:
        steve_home_exists = "<protected>"

    if platform_key == "macos":
        svc_file = user_home / "Library/LaunchAgents" / f"{DEFAULT_SERVICE_LABEL}.plist"
        kc_proc = run_command(["security", "list-keychains", "-d", "user"], check=False, timeout=15)
        keychains = kc_proc.stdout.strip() if kc_proc.returncode == 0 else None
    else:
        svc_file = user_home / ".config/systemd/user" / f"{DEFAULT_SERVICE_LABEL}.service"
        keychains = None

    return {
        "steve_home_exists": steve_home_exists,
        "steve_home_current": current_target,
        "steve_env_stat": stat_sig(env_file),
        "default_service_stat": stat_sig(svc_file),
        "legacy_linux_service_stat": (
            stat_sig(user_home / ".config/systemd/user/steve-hub.service")
            if platform_key == "linux"
            else None
        ),
        "macos_user_keychains": keychains,
    }


def process_identity(pid: int) -> tuple[str | None, list[str]] | None:
    """Return (exec_path, argv) for a live process using kernel APIs, or None if exited."""
    if pid <= 1:
        return None
    if sys.platform.startswith("linux"):
        proc_dir = Path(f"/proc/{pid}")
        if not proc_dir.exists():
            return None
        try:
            raw_cmd = (proc_dir / "cmdline").read_bytes()
        except OSError:
            return None
        argv = [part.decode("utf-8", errors="replace") for part in raw_cmd.split(b"\x00") if part]
        try:
            exec_path: str | None = os.readlink(proc_dir / "exe")
        except OSError:
            exec_path = None
        if not exec_path and not argv:
            return None
        return exec_path, argv

    if sys.platform == "darwin":
        libc = ctypes.CDLL("/usr/lib/libSystem.B.dylib")
        path_buf = ctypes.create_string_buffer(PROC_PIDPATHINFO_MAXSIZE)
        path_len = libc.proc_pidpath(pid, path_buf, PROC_PIDPATHINFO_MAXSIZE)
        exec_path = path_buf.value.decode("utf-8", errors="replace") if path_len > 0 else None

        mib = (ctypes.c_int * 3)(MACOS_CTL_KERN, MACOS_KERN_PROCARGS2, pid)
        size = ctypes.c_size_t(0)
        argv: list[str] = []
        if libc.sysctl(mib, 3, None, ctypes.byref(size), None, 0) == 0 and size.value > 4:
            arg_buf = ctypes.create_string_buffer(size.value)
            if libc.sysctl(mib, 3, arg_buf, ctypes.byref(size), None, 0) == 0 and size.value > 4:
                raw = arg_buf.raw[: size.value]
                argc = struct.unpack("i", raw[:4])[0]
                if 0 < argc < 4096:
                    after_exec = raw[4:].split(b"\x00", 1)
                    rest = after_exec[1].lstrip(b"\x00") if len(after_exec) == 2 else b""
                    if rest:
                        parts = rest.split(b"\x00", argc)[:argc]
                        argv = [p.decode("utf-8", errors="replace") for p in parts if p]
        if exec_path is None and not argv:
            return None
        return exec_path, argv

    return None


def process_is_alive(pid: int) -> bool:
    """Return True if pid currently exists in the process table."""
    if pid <= 1:
        return False
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def process_belongs_to_home(
    pid: int, home: Path, *, require_local_node: bool = True
) -> tuple[bool, str]:
    """Verify pid is an executable/script inside home (and optionally --local-node)."""
    ident = process_identity(pid)
    if ident is None:
        return False, "process not running"
    exec_path, argv = ident
    home_prefix_canon = str(home.resolve()) + os.sep
    home_prefix_raw = str(home) + os.sep
    candidates = [p for p in ([exec_path] + argv[:2]) if p]
    path_matches = any(
        c.startswith(home_prefix_canon) or c.startswith(home_prefix_raw) for c in candidates
    )
    if not path_matches:
        return False, f"executable/argv {candidates!r} outside {home_prefix_canon}"
    if require_local_node:
        if "--local-node" not in argv or not any(
            "steve-node" in c or "steve-hub" in c for c in candidates
        ):
            return False, f"process {candidates!r} argv={argv!r} is not steve-node --local-node"
    return True, " ".join(argv) if argv else (exec_path or "")


def terminate_process_group(
    pid: int,
    grace_secs: float = PROCESS_TERM_GRACE_SECS,
    proc: subprocess.Popen[str] | None = None,
) -> None:
    """Stop a process group started by this rehearsal via SIGTERM then SIGKILL."""
    if pid <= 1:
        return
    if proc is not None and proc.poll() is not None:
        return
    try:
        pgid = os.getpgid(pid)
    except (ProcessLookupError, PermissionError):
        if proc is not None:
            proc.poll()
        return
    try:
        os.killpg(pgid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        if proc is not None and proc.poll() is not None:
            return
    deadline = time.monotonic() + grace_secs
    while time.monotonic() < deadline:
        if proc is not None and proc.poll() is not None:
            return
        try:
            os.killpg(pgid, 0)
        except (ProcessLookupError, PermissionError):
            if proc is not None:
                proc.poll()
            return
        time.sleep(0.2)
    try:
        os.killpg(pgid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        if proc is not None:
            proc.poll()
        return
    kill_deadline = time.monotonic() + PROCESS_KILL_WAIT_SECS
    while time.monotonic() < kill_deadline:
        if proc is not None and proc.poll() is not None:
            return
        try:
            os.killpg(pgid, 0)
        except (ProcessLookupError, PermissionError):
            if proc is not None:
                proc.poll()
            return
        time.sleep(0.1)


def stop_verified_local_node(home: Path) -> bool:
    """Stop the rehearsal's local Node only when its identity strictly matches home."""
    pid_file = home / "data/node/service.pid"
    if not pid_file.is_file():
        return False
    lines = pid_file.read_text(encoding="utf-8", errors="replace").splitlines()
    if not lines:
        return False
    try:
        pid = int(lines[0].strip())
    except ValueError:
        return False
    belongs, _detail = process_belongs_to_home(pid, home, require_local_node=True)
    if not belongs:
        return False

    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return False
    deadline = time.monotonic() + PROCESS_TERM_GRACE_SECS
    while time.monotonic() < deadline:
        if not process_is_alive(pid):
            return True
        time.sleep(0.2)
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        return True
    kill_deadline = time.monotonic() + PROCESS_KILL_WAIT_SECS
    while time.monotonic() < kill_deadline:
        if not process_is_alive(pid):
            return True
        time.sleep(0.1)
    return not process_is_alive(pid)


def read_owner_token(home: Path) -> tuple[str, str]:
    """Read (user, token) from <home>/secrets/owner.token."""
    token_path = home / "secrets/owner.token"
    if not token_path.is_file():
        raise RehearsalError(f"owner token file missing: {token_path}")
    fields: dict[str, str] = {}
    for line in token_path.read_text(encoding="utf-8").splitlines():
        if "=" in line:
            k, v = line.split("=", 1)
            fields[k.strip()] = v.strip()
    user = fields.get("user", "")
    token = fields.get("token", "")
    if not user or not token:
        raise RehearsalError(f"incomplete owner token file: {token_path}")
    return user, token


class HubClient:
    """Loopback HTTP client for a single rehearsal Hub and its separate Web port."""

    def __init__(self, api_base: str, web_base: str) -> None:
        self.api_base = api_base.rstrip("/")
        self.web_base = web_base.rstrip("/")
        self.opener = loopback_opener()
        self.credential: str | None = None
        self.workspace_id: str | None = None

    def raw_get(self, url: str) -> tuple[int, bytes]:
        req = urllib.request.Request(url, method="GET")
        try:
            with self.opener.open(req, timeout=HTTP_REQUEST_TIMEOUT_SECS) as resp:
                return resp.status, resp.read()
        except urllib.error.HTTPError as exc:
            return exc.code, exc.read()
        except (urllib.error.URLError, TimeoutError, http.client.HTTPException, OSError) as exc:
            raise RehearsalError(f"GET {url} failed: {exc}") from exc

    def api_json(
        self,
        method: str,
        path: str,
        body: dict[str, object] | None = None,
        *,
        expected_statuses: tuple[int, ...] = (200,),
        include_ws: bool = True,
    ) -> tuple[int, object]:
        headers: dict[str, str] = {}
        data: bytes | None = None
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        if self.credential:
            headers["Authorization"] = f"Bearer {self.credential}"
        if include_ws and self.workspace_id and path.startswith("/api/"):
            headers["x-steve-ws"] = self.workspace_id
        url = f"{self.api_base}{path}"
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with self.opener.open(req, timeout=HTTP_REQUEST_TIMEOUT_SECS) as resp:
                status = resp.status
                raw = resp.read()
        except urllib.error.HTTPError as exc:
            status = exc.code
            raw = exc.read()
        except (urllib.error.URLError, TimeoutError, http.client.HTTPException, OSError) as exc:
            raise RehearsalError(f"{method} {url} transport failure: {exc}") from exc

        if status not in expected_statuses:
            detail = raw.decode("utf-8", errors="replace")[:500]
            raise RehearsalError(
                f"{method} {path} returned HTTP {status} (expected {expected_statuses}): {detail}"
            )
        if not raw:
            return status, {}
        try:
            return status, json.loads(raw.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise RehearsalError(f"{method} {path} returned invalid JSON: {exc}") from exc

    def try_healthz(self) -> dict[str, object] | None:
        try:
            status, payload = self.raw_get(f"{self.api_base}/healthz")
        except RehearsalError:
            return None
        if status != 200:
            return None
        try:
            data = json.loads(payload.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            return None
        return data if isinstance(data, dict) else None

    def wait_health(
        self,
        *,
        expected_version: str,
        expected_commit: str | None = None,
        timeout: float,
        require_node_linked: bool = False,
    ) -> dict[str, object]:
        deadline = time.monotonic() + timeout
        last_seen: dict[str, object] | None = None
        while time.monotonic() < deadline:
            health = self.try_healthz()
            if health is not None:
                last_seen = health
                commit_ok = (
                    expected_commit is None
                    or health.get("build_commit") is None
                    or health.get("build_commit") == expected_commit
                )
                recovery = health.get("recovery")
                recovery_ok = not (isinstance(recovery, dict) and recovery.get("state") == "failed")
                node = health.get("node")
                node_ok = True
                if require_node_linked and isinstance(node, dict) and node.get("enabled") is True:
                    if "linked" in node:
                        node_ok = node.get("linked") is True
                    elif "reconciled" in node:
                        node_ok = node.get("reconciled") is True
                if (
                    health.get("ok") is True
                    and health.get("version") == expected_version
                    and (health.get("web_version") is None or health.get("web_version") == expected_version)
                    and health.get("drain") is not True
                    and health.get("maintenance") is not True
                    and commit_ok
                    and recovery_ok
                    and node_ok
                ):
                    return health
            time.sleep(HEALTH_POLL_INTERVAL_SECS)
        raise RehearsalError(
            f"timed out after {timeout}s waiting for /healthz version={expected_version}; "
            f"last_seen={json.dumps(last_seen)}"
        )

    def authenticate(self, user: str, token: str, platform_key: str) -> None:
        _, payload = self.api_json(
            "POST",
            "/auth/token",
            body={
                "user": user,
                "token": token,
                "client": {
                    "kind": "cli",
                    "name": "steve-upgrade-rehearsal",
                    "platform": platform_key,
                    "version": "1",
                },
            },
            include_ws=False,
        )
        assert isinstance(payload, dict)
        credential = payload.get("credential")
        if not isinstance(credential, str) or not credential:
            raise RehearsalError(f"POST /auth/token returned no credential: {payload!r}")
        self.credential = credential

        _, me = self.api_json("GET", "/api/me", include_ws=False)
        assert isinstance(me, dict)
        ws = me.get("workspace")
        if not isinstance(ws, str) or not ws:
            raise RehearsalError(f"GET /api/me returned no workspace: {me!r}")
        self.workspace_id = ws


class RehearsalRunner:
    """Orchestrates an isolated end-to-end upgrade rehearsal and guaranteed cleanup."""

    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.platform_key, self.target = detect_target()
        self.run_id = uuid.uuid4().hex[:12]
        self.label = f"{REHEARSAL_LABEL_PREFIX}{self.run_id}"
        self.work_parent = validate_sandbox_paths(args.work_dir)
        self.sandbox = Path(
            tempfile.mkdtemp(prefix=f"steve-rehearsal-{self.run_id}-", dir=str(self.work_parent))
        ).resolve()
        self.home = self.sandbox / "home"
        self.vault_dir = self.sandbox / "vault-keys"
        self.harness_root = self.sandbox / "harness"
        self.bundles_dir = self.sandbox / "bundles"
        self.port = allocate_port_block()
        self.api_base = f"http://127.0.0.1:{self.port}"
        self.web_base = f"http://127.0.0.1:{self.port + 1}"
        self.client = HubClient(self.api_base, self.web_base)
        self.prev: BundleInfo | None = None
        self.cand: BundleInfo | None = None
        self.seeded: SeededData | None = None
        self.baseline: dict[str, object] | None = None
        self.workaround_pids: list[int] = []
        self.linux_previous_release: Path | None = None
        self.linux_backup_snapshot: str | None = None
        self.steps: list[StepResult] = []
        self.diagnostics: dict[str, object] = {}
        self.cleanup_ok = False
        self.operator_ok = False

    def service_plist_path(self) -> Path:
        return Path.home().resolve() / "Library/LaunchAgents" / f"{self.label}.plist"

    def service_unit_path(self) -> Path:
        return Path.home().resolve() / ".config/systemd/user" / f"{self.label}.service"

    def active_cli(self) -> Path | None:
        installed_cli = self.home / "current/bin/steve"
        if installed_cli.is_file() and os.access(installed_cli, os.X_OK):
            return installed_cli
        if self.cand and self.cand.cli_bin is not None and self.cand.cli_bin.is_file():
            return self.cand.cli_bin
        if self.prev and self.prev.cli_bin is not None and self.prev.cli_bin.is_file():
            return self.prev.cli_bin
        return None

    def _run_step(self, name: str, fn) -> object:
        started = time.monotonic()
        self._progress(f"[{name}] running...")
        try:
            result = fn()
        except RehearsalInterrupted:
            elapsed = time.monotonic() - started
            self.steps.append(
                StepResult(name=name, status="fail", duration_s=elapsed, summary="interrupted by signal")
            )
            raise
        except Exception as exc:
            elapsed = time.monotonic() - started
            self.steps.append(
                StepResult(name=name, status="fail", duration_s=elapsed, summary=str(exc))
            )
            self._progress(f"[{name}] FAIL ({elapsed:.2f}s): {exc}")
            raise
        elapsed = time.monotonic() - started
        if isinstance(result, tuple) and len(result) == 2 and isinstance(result[0], str):
            summary, details = result
        elif isinstance(result, str):
            summary, details = result, {}
        else:
            summary, details = "ok", result if isinstance(result, dict) else {}
        self.steps.append(
            StepResult(name=name, status="pass", duration_s=elapsed, summary=summary, details=details)
        )
        self._progress(f"[{name}] PASS ({elapsed:.2f}s): {summary}")
        return result

    def _skip_step(self, name: str, reason: str) -> None:
        self.steps.append(StepResult(name=name, status="skip", duration_s=0.0, summary=reason))
        self._progress(f"[{name}] SKIP: {reason}")

    def _progress(self, message: str) -> None:
        print(message, file=sys.stderr, flush=True)

    def _step_resolve_bundles(self) -> tuple[str, dict[str, object]]:
        if self.platform_key == "linux":
            preflight_linux_systemd()
        prev = resolve_bundle_input(
            self.args.prev_release,
            role="from",
            repo=self.args.repo,
            target=self.target,
            kind="installer",
            extract_dir=self.bundles_dir / "from",
            cache_dir=self.args.cache_dir,
            require_cli=True,
        )
        cand = resolve_bundle_input(
            self.args.candidate,
            role="candidate",
            repo=self.args.repo,
            target=self.target,
            kind="runtime",
            extract_dir=self.bundles_dir / "candidate",
            cache_dir=self.args.cache_dir,
            require_cli=False,
        )
        if (
            prev.version == cand.version
            and prev.commit == cand.commit
            and not prev.has_legacy_run_sh
        ):
            raise RehearsalError(
                f"candidate ({cand.version} {cand.commit[:8]}) has the same version and commit as --from"
            )
        user_steve = (Path.home() / ".steve").resolve()
        if self.home.resolve() == user_steve or self.label == DEFAULT_SERVICE_LABEL:
            raise RehearsalError("isolation check failed: rehearsal home/label matches operator default")
        self.prev = prev
        self.cand = cand
        return (
            f"from={prev.version} ({prev.commit[:8]}), candidate={cand.version} ({cand.commit[:8]}), "
            f"ports={self.port}..{self.port + CONTIGUOUS_PORT_SPAN - 1}",
            {
                "from_version": prev.version,
                "from_commit": prev.commit,
                "candidate_version": cand.version,
                "candidate_commit": cand.commit,
                "target": self.target,
                "port": self.port,
                "label": self.label,
            },
        )

    def _step_operator_baseline(self) -> tuple[str, dict[str, object]]:
        self.baseline = snapshot_operator_state(self.platform_key)
        return "captured operator baseline", dict(self.baseline)

    def _isolated_env_additions(self) -> dict[str, str]:
        self.harness_root.mkdir(parents=True, exist_ok=True, mode=0o700)
        additions: dict[str, str] = {}
        for var_name, subdir in HARNESS_STATE_VARS.items():
            target_dir = self.harness_root / subdir
            target_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
            additions[var_name] = str(target_dir)
        additions["STEVE_UPDATE_CHECK"] = "0"
        additions["STEVE_BACKUP"] = "off"
        additions["STEVE_DISK_FLOOR_GB"] = "1"
        return additions

    def _workaround_broken_setup(self, prev: BundleInfo) -> bool:
        """Initialize SQLite for pre-#634 releases where `setup hub` fails before creating the DB."""
        installed_hub = self.home / "releases" / prev.version / "bin/steve-hub"
        installed_web = self.home / "releases" / prev.version / "web/dist"
        env_path = self.home / "steve.env"
        setup_json = self.home / "setup.json"
        db_path = self.home / "data/steve.sqlite3"
        owner_token = self.home / "secrets/owner.token"
        if not (
            setup_json.is_file()
            and env_path.is_file()
            and installed_hub.is_file()
            and (not db_path.is_file() or not owner_token.is_file())
        ):
            return False

        parsed_env = parse_env_file(env_path)
        env = clean_subprocess_env(parsed_env)
        env.update(self._isolated_env_additions())
        env["STEVE_HOME"] = str(self.home)
        env["STEVE_WEB_DIR"] = str(installed_web if installed_web.is_dir() else prev.web_dir)
        env["STEVE_NO_RESUME"] = "1"

        log_path = self.sandbox / "workaround-db-init.log"
        with log_path.open("w", encoding="utf-8") as log_file:
            proc = subprocess.Popen(
                [str(installed_hub)],
                cwd=str(self.home),
                env=env,
                stdin=subprocess.DEVNULL,
                stdout=log_file,
                stderr=log_file,
                start_new_session=True,
            )
            self.workaround_pids.append(proc.pid)
            try:
                deadline = time.monotonic() + DB_INIT_BOOT_TIMEOUT_SECS
                while time.monotonic() < deadline:
                    if proc.poll() is not None:
                        tail = log_path.read_text(encoding="utf-8", errors="replace")[-DIAGNOSTIC_LOG_TAIL_CHARS:]
                        raise RehearsalError(
                            f"workaround hub exited early (code={proc.returncode}) before DB init:\n{tail}"
                        )
                    if self.client.try_healthz() is not None and db_path.is_file():
                        break
                    time.sleep(0.2)
                else:
                    tail = log_path.read_text(encoding="utf-8", errors="replace")[-DIAGNOSTIC_LOG_TAIL_CHARS:]
                    raise RehearsalError(
                        f"workaround hub did not initialize DB within {DB_INIT_BOOT_TIMEOUT_SECS}s:\n{tail}"
                    )
            finally:
                terminate_process_group(proc.pid, proc=proc)
                if proc.poll() is None:
                    proc.wait(timeout=PROCESS_KILL_WAIT_SECS)
        return True

    def _step_fresh_setup(self) -> tuple[str, dict[str, object]]:
        assert self.prev is not None
        assert self.prev.cli_bin is not None
        setup_cmd = [
            str(self.prev.cli_bin),
            "setup",
            "hub",
            "--home",
            str(self.home),
            "--listen",
            f"127.0.0.1:{self.port}",
            "--label",
            self.label,
            "--vault-dir",
            str(self.vault_dir),
            "--adapters",
            "none",
        ]
        first = run_command(setup_cmd, timeout=self.args.timeout, check=False)
        workaround_applied = False
        if first.returncode != 0:
            if self._workaround_broken_setup(self.prev):
                workaround_applied = True
                run_command(setup_cmd, timeout=self.args.timeout, check=True)
            else:
                raise RehearsalError(
                    f"fresh setup failed (exit={first.returncode}):\n"
                    f"stdout: {first.stdout.strip()}\nstderr: {first.stderr.strip()}"
                )

        user, token = read_owner_token(self.home)
        svc_file = self.service_plist_path() if self.platform_key == "macos" else self.service_unit_path()
        if not svc_file.is_file():
            raise RehearsalError(f"setup completed without creating service definition: {svc_file}")

        added_keys = append_isolated_env(self.home / "steve.env", self._isolated_env_additions())
        return (
            f"setup {self.prev.version} ok (workaround={workaround_applied}, user={user}, token_len={len(token)})",
            {
                "workaround_applied": workaround_applied,
                "service_file": str(svc_file),
                "added_env_keys": len(added_keys),
            },
        )

    def _step_start_previous(self) -> tuple[str, dict[str, object]]:
        assert self.prev is not None
        assert self.prev.cli_bin is not None
        proc = run_command(
            [str(self.prev.cli_bin), "hub", "start", "--home", str(self.home), "--label", self.label, "--json"],
            timeout=self.args.timeout,
        )
        start_report = parse_json_output(proc.stdout, "steve hub start")
        if start_report.get("ok") is False:
            raise RehearsalError(f"steve hub start reported failure: {start_report}")

        health = self.client.wait_health(
            expected_version=self.prev.version,
            expected_commit=self.prev.commit,
            timeout=self.args.timeout,
            require_node_linked=True,
        )
        status_proc = run_command(
            [str(self.prev.cli_bin), "hub", "status", "--home", str(self.home), "--label", self.label, "--json"],
            timeout=30,
        )
        status_report = parse_json_output(status_proc.stdout, "steve hub status")
        if status_report.get("service", {}).get("running") is not True:
            raise RehearsalError(f"service not running after start: {status_report}")
        return (
            f"started {self.prev.version} on {self.api_base}",
            {"health_version": health.get("version"), "node": health.get("node")},
        )

    def _probe_and_set_preupgrade_policy(self) -> tuple[str, str, str]:
        status, body = self.client.api_json(
            "GET", "/api/permission-policy", expected_statuses=(200, 404)
        )
        if status == 200:
            assert isinstance(body, dict)
            rev = body["policy"]["revision"]
            _, updated = self.client.api_json(
                "PUT",
                "/api/permission-policy",
                body={"revision": rev, "mode": "auto"},
            )
            assert isinstance(updated, dict)
            if updated["policy"]["mode"] != "auto":
                raise RehearsalError(f"failed to set pre-upgrade permission-policy=auto: {updated}")
            return "permission-policy", "auto", "auto"

        _, body = self.client.api_json("GET", "/api/approval-policy", expected_statuses=(200,))
        assert isinstance(body, dict)
        rev = body["policy"]["revision"]
        _, updated = self.client.api_json(
            "PUT",
            "/api/approval-policy",
            body={"revision": rev, "mode": "harness_auto"},
        )
        assert isinstance(updated, dict)
        if updated["policy"]["mode"] != "harness_auto":
            raise RehearsalError(f"failed to set pre-upgrade approval-policy=harness_auto: {updated}")
        return "approval-policy", "harness_auto", "auto"

    def _step_seed_data(self) -> tuple[str, dict[str, object]]:
        user, token = read_owner_token(self.home)
        self.client.authenticate(user, token, self.platform_key)

        # Ensure previous local Node is linked before seeding and upgrading.
        node = self._wait_local_node_linked(expected_version=None, timeout=min(self.args.timeout, LOCAL_NODE_WAIT_SECS))

        _, bot = self.client.api_json(
            "POST",
            "/api/bots",
            body={
                "name": "RehearsalBot",
                "role": "Upgrade rehearsal sentinel",
                "rules": "Sentinel bot created by upgrade rehearsal.",
            },
        )
        assert isinstance(bot, dict)
        bot_id = str(bot["id"])

        memory_text = f"rehearsal-memory-{self.run_id}"
        _, memory = self.client.api_json(
            "POST", f"/api/bots/{bot_id}/memories", body={"text": memory_text}
        )
        assert isinstance(memory, dict)
        memory_id = str(memory["id"])

        tag_name = f"reh-{self.run_id[:6]}"
        _, tag = self.client.api_json(
            "POST",
            "/api/tags",
            body={"name": tag_name, "cat": "主题", "description": "Upgrade rehearsal sentinel tag"},
        )
        assert isinstance(tag, dict)
        tag_id = str(tag["id"])

        item_title = f"Rehearsal {self.run_id[:6]}"
        item_goal = f"Rehearsal sentinel item {self.run_id}"
        _, item = self.client.api_json(
            "POST",
            "/api/items",
            body={
                "goal": item_goal,
                "title": item_title,
                "bot": bot_id,
                "project": None,
                "mode": "propose",
            },
        )
        assert isinstance(item, dict)
        item_id = str(item["id"])

        policy_api, prev_mode, expected_cand_mode = self._probe_and_set_preupgrade_policy()
        self.seeded = SeededData(
            user=user,
            credential=self.client.credential or "",
            workspace_id=self.client.workspace_id or "",
            bot_id=bot_id,
            bot_name="RehearsalBot",
            memory_id=memory_id,
            memory_text=memory_text,
            tag_id=tag_id,
            tag_name=tag_name,
            item_id=item_id,
            item_title=item_title,
            item_goal=item_goal,
            prev_policy_api=policy_api,
            prev_policy_mode=prev_mode,
            expected_candidate_mode=expected_cand_mode,
        )
        return (
            f"seeded bot={bot_id}, memory={memory_id}, tag={tag_id}, item={item_id}, {policy_api}={prev_mode}",
            {
                "workspace": self.seeded.workspace_id,
                "bot_id": bot_id,
                "memory_id": memory_id,
                "tag_id": tag_id,
                "item_id": item_id,
                "policy_api": policy_api,
                "policy_mode": prev_mode,
                "previous_local_node": node.get("node_id"),
            },
        )

    def _upgrade_macos(self) -> tuple[str, dict[str, object]]:
        assert self.cand is not None
        proc = run_command(
            [
                str(self.cand.hub_bin),
                "--local-release",
                "upgrade-macos",
                str(self.cand.root),
                "--home",
                str(self.home),
                "--label",
                self.label,
                "--timeout",
                str(self.args.timeout),
            ],
            timeout=self.args.timeout + 60,
        )
        receipt = parse_json_output(proc.stdout, "upgrade-macos")
        if receipt.get("ok") is not True or receipt.get("version") != self.cand.version:
            raise RehearsalError(f"upgrade-macos receipt unexpected: {receipt}")
        snapshot = receipt.get("backup_snapshot")
        if not isinstance(snapshot, str) or not snapshot:
            raise RehearsalError(f"upgrade-macos receipt missing backup_snapshot: {receipt}")
        journal_path = self.home / "releases/.macos-rollback.json"
        if not journal_path.is_file():
            raise RehearsalError(f"missing rollback journal after upgrade-macos: {journal_path}")
        journal = json.loads(journal_path.read_text(encoding="utf-8"))
        if journal.get("stage") != "succeeded":
            raise RehearsalError(f"unexpected .macos-rollback.json stage: {journal}")
        plist_data = plistlib.loads(self.service_plist_path().read_bytes())
        prog_args = plist_data.get("ProgramArguments", [])
        if not prog_args or not str(prog_args[0]).endswith("/bin/steve-hub"):
            raise RehearsalError(f"upgraded plist does not invoke steve-hub directly: {prog_args}")
        return (
            f"upgrade-macos -> {self.cand.version} (snapshot={snapshot[:12]}...)",
            {"backup_snapshot": snapshot, "plist_rewritten": receipt.get("plist_rewritten")},
        )

    def _upgrade_linux(self) -> tuple[str, dict[str, object]]:
        assert self.cand is not None
        op_id = str(uuid.uuid4())
        target_dir = self.home / "releases" / f"{self.cand.version}-{op_id}"
        stage_dir = self.home / "releases" / f"{self.cand.version}-{op_id}.staging"
        if stage_dir.exists():
            shutil.rmtree(stage_dir)
        shutil.copytree(self.cand.root, stage_dir)
        os.replace(stage_dir, target_dir)

        now_ms = int(time.time() * 1000)
        ops_dir = self.home / "updates/operations"
        ops_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
        op_file = ops_dir / f"{op_id}.json"
        op_payload = {
            "id": op_id,
            "idempotency_key": f"rehearsal-{op_id}",
            "version": self.cand.version,
            "phase": "ready",
            "created_at": now_ms,
            "updated_at": now_ms,
            "requested_by": "rehearsal",
            "error": None,
            "previous": None,
            "target": str(target_dir),
        }
        _atomic_write_bytes(op_file, json.dumps(op_payload, indent=2).encode("utf-8"), mode=0o600)
        _atomic_write_bytes(
            self.home / "updates/active.json",
            json.dumps({"id": op_id}, indent=2).encode("utf-8"),
            mode=0o600,
        )

        deadline = time.monotonic() + self.args.timeout
        while time.monotonic() < deadline:
            op = json.loads(op_file.read_text(encoding="utf-8"))
            phase = op.get("phase")
            if phase == "succeeded":
                prev_path = op.get("previous")
                snap = op.get("backup_snapshot")
                if isinstance(prev_path, str) and prev_path:
                    self.linux_previous_release = Path(prev_path)
                if isinstance(snap, str) and snap:
                    self.linux_backup_snapshot = snap
                return (
                    f"linux supervisor -> {self.cand.version} (op={op_id[:8]}...)",
                    {"operation_id": op_id, "backup_snapshot": snap, "previous": prev_path},
                )
            if phase in ("failed", "rolled_back"):
                raise RehearsalError(f"linux supervisor upgrade ended in {phase}: {op.get('error')}")
            time.sleep(HEALTH_POLL_INTERVAL_SECS)
        raise RehearsalError(f"linux supervisor upgrade timed out after {self.args.timeout}s")

    def _step_upgrade(self) -> tuple[str, dict[str, object]]:
        if self.platform_key == "macos":
            return self._upgrade_macos()
        return self._upgrade_linux()

    def _wait_local_node_linked(self, expected_version: str | None, timeout: float) -> dict[str, object]:
        deadline = time.monotonic() + timeout
        last_node: dict[str, object] | None = None
        while time.monotonic() < deadline:
            _, nodes_resp = self.client.api_json("GET", "/api/nodes")
            assert isinstance(nodes_resp, dict)
            nodes = nodes_resp.get("nodes", [])
            local_node = next(
                (n for n in nodes if n.get("local") is True or n.get("node_id") == "local"),
                None,
            )
            if isinstance(local_node, dict):
                last_node = local_node
                linked_ok = local_node.get("linked") is True
                state_ok = local_node.get("state") == "active"
                ver = local_node.get("node_version")
                ver_ok = expected_version is None or ver == expected_version
                if linked_ok and state_ok and ver_ok:
                    return local_node
            time.sleep(HEALTH_POLL_INTERVAL_SECS)
        raise RehearsalError(
            f"local Node did not reach linked+active (expected_version={expected_version}) within {timeout}s; "
            f"last_node={json.dumps(last_node)}"
        )

    def _verify_seeded_data(self, expected_version: str) -> None:
        assert self.seeded is not None
        _, me = self.client.api_json("GET", "/api/me", include_ws=False)
        assert isinstance(me, dict)
        if me.get("version") != expected_version or me.get("workspace") != self.seeded.workspace_id:
            raise RehearsalError(f"/api/me mismatch: {me}")

        _, shell = self.client.api_json("GET", "/api/shell")
        assert isinstance(shell, dict)
        bots = shell.get("bots", [])
        if not any(b.get("id") == self.seeded.bot_id and b.get("name") == self.seeded.bot_name for b in bots):
            raise RehearsalError(f"seeded bot {self.seeded.bot_id} missing from /api/shell")

        _, memories = self.client.api_json("GET", f"/api/bots/{self.seeded.bot_id}/memories")
        if not isinstance(memories, list) or not any(
            m.get("id") == self.seeded.memory_id and m.get("text") == self.seeded.memory_text for m in memories
        ):
            raise RehearsalError(f"seeded memory {self.seeded.memory_id} missing or altered: {memories!r}")

        _, tags_resp = self.client.api_json("GET", "/api/tags")
        assert isinstance(tags_resp, dict)
        tags = tags_resp.get("tags", [])
        if not any(t.get("id") == self.seeded.tag_id and t.get("name") == self.seeded.tag_name for t in tags):
            raise RehearsalError(f"seeded tag {self.seeded.tag_id} missing or altered: {tags!r}")

        _, item = self.client.api_json("GET", f"/api/items/{self.seeded.item_id}")
        assert isinstance(item, dict)
        if (
            item.get("id") != self.seeded.item_id
            or item.get("title") != self.seeded.item_title
            or item.get("goal") != self.seeded.item_goal
        ):
            raise RehearsalError(f"seeded item {self.seeded.item_id} missing or altered: {item!r}")

    def _verify_local_node_pid(self, expected_version: str) -> int:
        pid_file = self.home / "data/node/service.pid"
        if not pid_file.is_file():
            raise RehearsalError(f"local Node service.pid missing: {pid_file}")
        lines = pid_file.read_text(encoding="utf-8", errors="replace").splitlines()
        if not lines:
            raise RehearsalError(f"local Node service.pid empty: {pid_file}")
        pid = int(lines[0].strip())
        if len(lines) > 1 and lines[1].strip() and lines[1].strip() != expected_version:
            raise RehearsalError(
                f"local Node service.pid version={lines[1].strip()} != expected {expected_version}"
            )
        belongs, detail = process_belongs_to_home(pid, self.home, require_local_node=True)
        if not belongs:
            raise RehearsalError(f"local Node pid {pid} verification failed: {detail}")
        return pid

    def _verify_candidate(self, *, test_permission_writes: bool) -> tuple[str, dict[str, object]]:
        assert self.cand is not None
        assert self.seeded is not None

        health = self.client.wait_health(
            expected_version=self.cand.version,
            expected_commit=self.cand.commit,
            timeout=self.args.timeout,
            require_node_linked=True,
        )
        web_status, web_body = self.client.raw_get(f"{self.web_base}/")
        if web_status != 200:
            raise RehearsalError(f"web UI root {self.web_base}/ returned HTTP {web_status}")
        web_html = web_body.decode("utf-8", errors="replace")
        meta_re = re.compile(WEB_VERSION_META_RE_TEMPLATE.format(version=re.escape(self.cand.version)))
        if not meta_re.search(web_html):
            raise RehearsalError(f"served Web UI missing steve-web-version={self.cand.version}")

        cli = self.active_cli()
        if cli is not None:
            st_proc = run_command(
                [str(cli), "hub", "status", "--home", str(self.home), "--label", self.label, "--json"],
                timeout=30,
            )
            st_report = parse_json_output(st_proc.stdout, "steve hub status")
            if st_report.get("service", {}).get("running") is not True:
                raise RehearsalError(f"service not reported running: {st_report}")
            if st_report.get("configured_build", {}).get("version") != self.cand.version:
                raise RehearsalError(f"configured_build mismatch: {st_report}")

        self._verify_seeded_data(self.cand.version)

        _, perm = self.client.api_json("GET", "/api/permission-policy")
        assert isinstance(perm, dict)
        if perm.get("editable") is not True:
            raise RehearsalError(f"permission-policy not editable by owner: {perm}")
        if perm["policy"]["mode"] != self.seeded.expected_candidate_mode:
            raise RehearsalError(
                f"migrated permission mode expected {self.seeded.expected_candidate_mode!r}, got {perm['policy']['mode']!r}"
            )

        if test_permission_writes:
            rev = perm["policy"]["revision"]
            self.client.api_json(
                "PUT",
                "/api/permission-policy",
                body={"revision": rev, "mode": "full_access"},
                expected_statuses=(400,),
            )
            _, ask_resp = self.client.api_json(
                "PUT",
                "/api/permission-policy",
                body={"revision": rev, "mode": "ask"},
            )
            assert isinstance(ask_resp, dict)
            if ask_resp["policy"]["mode"] != "ask" or ask_resp["policy"]["revision"] <= rev:
                raise RehearsalError(f"unexpected permission-policy ask update response: {ask_resp}")
            rev2 = ask_resp["policy"]["revision"]
            _, auto_resp = self.client.api_json(
                "PUT",
                "/api/permission-policy",
                body={"revision": rev2, "mode": "auto"},
            )
            assert isinstance(auto_resp, dict)
            if auto_resp["policy"]["mode"] != "auto":
                raise RehearsalError(f"failed restoring permission-policy=auto: {auto_resp}")

        node = self._wait_local_node_linked(
            expected_version=self.cand.version,
            timeout=min(self.args.timeout, LOCAL_NODE_WAIT_SECS),
        )
        node_pid = self._verify_local_node_pid(self.cand.version)
        return (
            f"verified {self.cand.version}: health+web+4 entities, perm={perm['policy']['mode']}, "
            f"local_node=linked({node.get('node_version')}, pid={node_pid})",
            {
                "version": health.get("version"),
                "permission_mode": perm["policy"]["mode"],
                "permission_writes_checked": test_permission_writes,
                "local_node_pid": node_pid,
                "local_node_version": node.get("node_version"),
            },
        )

    def _rollback_macos(self) -> tuple[str, dict[str, object]]:
        assert self.prev is not None
        hub_exe = self.home / "current/bin/steve-hub"
        proc = run_command(
            [
                str(hub_exe),
                "--local-release",
                "rollback-macos",
                "--home",
                str(self.home),
                "--label",
                self.label,
                "--timeout",
                str(self.args.timeout),
                "--confirm-data-loss",
            ],
            timeout=self.args.timeout + 60,
        )
        receipt = parse_json_output(proc.stdout, "rollback-macos")
        if receipt.get("ok") is not True or receipt.get("version") != self.prev.version:
            raise RehearsalError(f"rollback-macos receipt unexpected: {receipt}")
        if (self.home / "releases/.macos-rollback.json").exists():
            raise RehearsalError(".macos-rollback.json still exists after rollback-macos")
        plist_prev = self.service_plist_path().with_suffix(".plist.previous")
        if plist_prev.exists():
            raise RehearsalError(f".plist.previous still exists after rollback-macos: {plist_prev}")
        return (
            f"rollback-macos -> {self.prev.version} (db_restored={receipt.get('database_restored')})",
            {
                "version": receipt.get("version"),
                "database_restored": receipt.get("database_restored"),
                "plist_restored_from_backup": receipt.get("plist_restored_from_backup"),
            },
        )

    def _rollback_linux(self) -> tuple[str, dict[str, object]]:
        assert self.prev is not None
        assert self.cand is not None
        cli = self.active_cli()
        if cli is None:
            raise RehearsalError("no steve CLI available for Linux rollback")
        if self.linux_previous_release is None or not self.linux_previous_release.is_dir():
            raise RehearsalError(f"missing previous release path for Linux rollback: {self.linux_previous_release}")
        if not self.linux_backup_snapshot:
            raise RehearsalError("missing pre-upgrade backup_snapshot for Linux rollback")

        run_command(
            [str(cli), "hub", "stop", "--home", str(self.home), "--label", self.label, "--json"],
            timeout=SERVICE_STOP_TIMEOUT_SECS,
        )
        stop_verified_local_node(self.home)

        restore_env = clean_subprocess_env(parse_env_file(self.home / "steve.env"))
        restore_env["STEVE_HOME"] = str(self.home)
        restore_proc = run_command(
            [str(self.cand.hub_bin), "--admin", "restore", self.linux_backup_snapshot, "--database-only"],
            env=restore_env,
            timeout=self.args.timeout,
        )
        if restore_proc.stdout.strip() != self.linux_backup_snapshot:
            raise RehearsalError(
                f"unexpected --admin restore output: {restore_proc.stdout.strip()!r}"
            )

        for link_path, target in (
            (self.home / "current", self.linux_previous_release),
            (self.home / "web/current", self.linux_previous_release / "web/dist"),
        ):
            tmp_link = link_path.parent / f".{link_path.name}.{uuid.uuid4().hex}.tmp"
            os.symlink(target, tmp_link)
            os.replace(tmp_link, link_path)

        unit_path = self.service_unit_path()
        unit_prev = unit_path.with_suffix(".service.previous")
        if unit_prev.is_file():
            shutil.copyfile(unit_prev, unit_path)
            unit_prev.unlink(missing_ok=True)
            run_command(["systemctl", "--user", "daemon-reload"], timeout=30)

        run_command(
            [str(cli), "hub", "start", "--home", str(self.home), "--label", self.label, "--json"],
            timeout=self.args.timeout,
        )
        return (
            f"linux rollback -> {self.prev.version} (snapshot={self.linux_backup_snapshot[:12]}...)",
            {"version": self.prev.version, "snapshot": self.linux_backup_snapshot},
        )

    def _step_rollback(self) -> tuple[str, dict[str, object]]:
        if self.platform_key == "macos":
            return self._rollback_macos()
        return self._rollback_linux()

    def _step_verify_rollback(self) -> tuple[str, dict[str, object]]:
        assert self.prev is not None
        assert self.seeded is not None
        self.client.wait_health(
            expected_version=self.prev.version,
            expected_commit=self.prev.commit,
            timeout=self.args.timeout,
            require_node_linked=True,
        )
        self._verify_seeded_data(self.prev.version)
        _, policy_resp = self.client.api_json("GET", f"/api/{self.seeded.prev_policy_api}")
        assert isinstance(policy_resp, dict)
        if policy_resp["policy"]["mode"] != self.seeded.prev_policy_mode:
            raise RehearsalError(
                f"rolled-back {self.seeded.prev_policy_api} expected {self.seeded.prev_policy_mode!r}, "
                f"got {policy_resp['policy']['mode']!r}"
            )
        node = self._wait_local_node_linked(
            expected_version=None, timeout=min(self.args.timeout, LOCAL_NODE_WAIT_SECS)
        )
        return (
            f"verified rollback {self.prev.version}: data+{self.seeded.prev_policy_api}={self.seeded.prev_policy_mode}+node",
            {"version": self.prev.version, "node_linked": node.get("linked")},
        )

    def _step_confirm(self) -> tuple[str, dict[str, object]]:
        assert self.cand is not None
        hub_exe = self.home / "current/bin/steve-hub"
        proc = run_command(
            [str(hub_exe), "--local-release", "confirm-macos", "--home", str(self.home)],
            timeout=60,
        )
        receipt = parse_json_output(proc.stdout, "confirm-macos")
        if receipt.get("ok") is not True or receipt.get("confirmed_version") != self.cand.version:
            raise RehearsalError(f"confirm-macos receipt unexpected: {receipt}")
        archived = Path(str(receipt.get("archived_journal", "")))
        if not archived.is_file():
            raise RehearsalError(f"archived journal missing after confirm-macos: {archived}")
        if (self.home / "releases/.macos-rollback.json").exists():
            raise RehearsalError(".macos-rollback.json still exists after confirm-macos")
        if self.service_plist_path().with_suffix(".plist.previous").exists():
            raise RehearsalError(".plist.previous still exists after confirm-macos")
        health = self.client.wait_health(
            expected_version=self.cand.version,
            expected_commit=self.cand.commit,
            timeout=30,
        )
        return (
            f"confirm-macos {self.cand.version} ok (archived={archived.name})",
            {"archived_journal": str(archived), "health_version": health.get("version")},
        )

    def _collect_failure_diagnostics(self) -> None:
        if not self.home.exists():
            return
        diag: dict[str, object] = {}
        for key, rel in (("hub_log_tail", "logs/hub.log"), ("local_node_log_tail", "data/node/logs/local-node.log")):
            path = self.home / rel
            if path.is_file():
                text = path.read_text(encoding="utf-8", errors="replace")
                diag[key] = text[-DIAGNOSTIC_LOG_TAIL_CHARS:]
        release_logs = self.home / "logs/release"
        if release_logs.is_dir():
            receipts = sorted(release_logs.glob("*.json"), key=lambda p: p.stat().st_mtime, reverse=True)
            if receipts:
                diag["latest_release_receipt"] = {
                    "file": receipts[0].name,
                    "content": receipts[0].read_text(encoding="utf-8", errors="replace")[-DIAGNOSTIC_LOG_TAIL_CHARS:],
                }
        if diag:
            self.diagnostics = diag

    def _step_cleanup(self) -> tuple[str, dict[str, object]]:
        if any(step.status == "fail" for step in self.steps):
            self._collect_failure_diagnostics()

        errors: list[str] = []
        notes: list[str] = []
        for pid in self.workaround_pids:
            try:
                terminate_process_group(pid)
            except Exception as exc:  # noqa: BLE001
                errors.append(f"stop workaround pid {pid}: {exc}")

        cli = self.active_cli()
        if cli is not None and (self.home / "services/hub.json").is_file():
            stop_proc = run_command(
                [str(cli), "hub", "stop", "--home", str(self.home), "--label", self.label, "--json"],
                check=False,
                timeout=SERVICE_STOP_TIMEOUT_SECS,
            )
            if stop_proc.returncode != 0:
                notes.append(
                    f"hub stop returned {stop_proc.returncode}: "
                    f"{stop_proc.stderr.strip() or stop_proc.stdout.strip()}"
                )

        if self.platform_key == "macos":
            domain = f"gui/{os.getuid()}/{self.label}"
            run_command(["launchctl", "bootout", domain], check=False, timeout=20)
            unload_deadline = time.monotonic() + LAUNCHD_UNLOAD_WAIT_SECS
            while time.monotonic() < unload_deadline:
                probe = run_command(["launchctl", "print", domain], check=False, timeout=10)
                if probe.returncode != 0:
                    break
                time.sleep(0.2)
            else:
                errors.append(f"launchd job {domain} remained loaded after bootout")
        else:
            run_command(["systemctl", "--user", "stop", f"{self.label}.service"], check=False, timeout=20)
            run_command(["systemctl", "--user", "disable", f"{self.label}.service"], check=False, timeout=20)

        try:
            stop_verified_local_node(self.home)
            pid_file = self.home / "data/node/service.pid"
            if pid_file.is_file():
                first = pid_file.read_text(encoding="utf-8", errors="replace").splitlines()[:1]
                if first and first[0].strip().isdigit():
                    node_pid = int(first[0].strip())
                    belongs, _ = process_belongs_to_home(node_pid, self.home, require_local_node=True)
                    if belongs and process_is_alive(node_pid):
                        errors.append(f"rehearsal local Node pid {node_pid} remained alive after cleanup stop")
        except Exception as exc:  # noqa: BLE001
            errors.append(f"stop local node: {exc}")

        if cli is not None and (self.home / "services/hub.json").is_file():
            svc_file = self.service_plist_path() if self.platform_key == "macos" else self.service_unit_path()
            if svc_file.exists():
                un_proc = run_command(
                    [str(cli), "hub", "uninstall", "--home", str(self.home), "--label", self.label, "--json"],
                    check=False,
                    timeout=SERVICE_UNINSTALL_TIMEOUT_SECS,
                )
                if un_proc.returncode != 0:
                    errors.append(
                        f"hub uninstall failed: {un_proc.stderr.strip() or un_proc.stdout.strip()}"
                    )

        if self.platform_key == "macos":
            for path in (
                self.service_plist_path(),
                self.service_plist_path().with_suffix(".plist.previous"),
            ):
                if path.exists() or path.is_symlink():
                    path.unlink(missing_ok=True)
            if self.service_plist_path().exists():
                errors.append(f"leftover rehearsal plist: {self.service_plist_path()}")
        else:
            removed_unit = False
            for path in (
                self.service_unit_path(),
                self.service_unit_path().with_suffix(".service.previous"),
            ):
                if path.exists() or path.is_symlink():
                    path.unlink(missing_ok=True)
                    removed_unit = True
            if removed_unit:
                run_command(["systemctl", "--user", "daemon-reload"], check=False, timeout=20)
            if self.service_unit_path().exists():
                errors.append(f"leftover rehearsal unit: {self.service_unit_path()}")

        if self.sandbox.exists():
            shutil.rmtree(self.sandbox)
        if self.sandbox.exists():
            errors.append(f"sandbox still exists after cleanup: {self.sandbox}")

        if errors:
            raise RehearsalError("; ".join(errors))
        self.cleanup_ok = True
        details: dict[str, object] = {"sandbox_removed": True}
        if notes:
            details["notes"] = notes
        return f"uninstalled {self.label} and removed {self.sandbox}", details

    def _step_operator_untouched(self) -> tuple[str, dict[str, object]]:
        if self.baseline is None:
            return "baseline not captured; skipping comparison", {}
        after = snapshot_operator_state(self.platform_key)
        if after != self.baseline:
            raise RehearsalError(
                f"operator state changed! before={json.dumps(self.baseline)} after={json.dumps(after)}"
            )
        self.operator_ok = True
        return "verified ~/.steve, default service, and keychains unchanged", after

    def run(self) -> tuple[int, dict[str, object]]:
        previous_handlers = {}

        def _handle_signal(signum: int, _frame) -> None:
            raise RehearsalInterrupted(signum)

        for sig in (signal.SIGINT, signal.SIGTERM):
            previous_handlers[sig] = signal.getsignal(sig)
            signal.signal(sig, _handle_signal)

        workflow_failed = False
        try:
            self._run_step("resolve_bundles", self._step_resolve_bundles)
            self._run_step("operator_baseline", self._step_operator_baseline)
            self._run_step("fresh_setup", self._step_fresh_setup)
            self._run_step("start_previous", self._step_start_previous)
            self._run_step("seed_data", self._step_seed_data)
            self._run_step("upgrade_pass1", self._step_upgrade)
            self._run_step(
                "verify_pass1",
                lambda: self._verify_candidate(test_permission_writes=self.args.skip_rollback),
            )
            if self.args.skip_rollback:
                for skipped_name in ("rollback", "verify_rollback", "upgrade_pass2", "verify_pass2"):
                    self._skip_step(skipped_name, "--skip-rollback requested")
            else:
                self._run_step("rollback", self._step_rollback)
                self._run_step("verify_rollback", self._step_verify_rollback)
                self._run_step("upgrade_pass2", self._step_upgrade)
                self._run_step("verify_pass2", lambda: self._verify_candidate(test_permission_writes=True))
            if self.platform_key == "macos":
                self._run_step("confirm", self._step_confirm)
            else:
                self._skip_step("confirm", "confirm-macos is macOS-only; Linux commits automatically")
        except (RehearsalError, RehearsalInterrupted, Exception):
            workflow_failed = True
        finally:
            for sig, handler in previous_handlers.items():
                signal.signal(sig, handler)
            try:
                self._run_step("cleanup", self._step_cleanup)
            except Exception:
                pass
            if self.baseline is not None:
                try:
                    self._run_step("operator_untouched", self._step_operator_untouched)
                except Exception:
                    pass
            else:
                self.operator_ok = True

        all_steps_ok = not workflow_failed and all(s.status in ("pass", "skip") for s in self.steps)
        if not self.cleanup_ok or not self.operator_ok:
            exit_code = EXIT_CLEANUP_OR_USAGE_ERROR
        elif all_steps_ok:
            exit_code = EXIT_OK
        else:
            exit_code = EXIT_REHEARSAL_FAILED

        report = self.build_report(exit_code == EXIT_OK)
        return exit_code, report

    def build_report(self, passed: bool) -> dict[str, object]:
        total_duration = sum(s.duration_s for s in self.steps)
        report: dict[str, object] = {
            "passed": passed,
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "platform": self.platform_key,
            "target": self.target,
            "from": (
                {"version": self.prev.version, "commit": self.prev.commit}
                if self.prev is not None
                else {"spec": self.args.prev_release}
            ),
            "candidate": (
                {"version": self.cand.version, "commit": self.cand.commit}
                if self.cand is not None
                else {"spec": self.args.candidate}
            ),
            "rehearsal_label": self.label,
            "duration_s": round(total_duration, 3),
            "cleanup_ok": self.cleanup_ok,
            "operator_untouched": self.operator_ok,
            "steps": [step.to_dict() for step in self.steps],
        }
        if self.diagnostics:
            report["diagnostics"] = self.diagnostics
        return report


def format_human_report(report: dict[str, object]) -> str:
    """Format a concise pass/fail summary table for maintainers."""
    verdict = "PASS" if report["passed"] else "FAIL"
    from_info = report["from"]
    cand_info = report["candidate"]
    from_str = from_info.get("version") or from_info.get("spec")
    cand_str = cand_info.get("version") or cand_info.get("spec")
    lines = [
        f"Steve Upgrade Rehearsal ({report['platform']} / {report['target']})",
        f"Route: {from_str} -> {cand_str} | Label: {report['rehearsal_label']}",
        f"Result: {verdict} in {report['duration_s']}s | Cleanup: {report['cleanup_ok']} | Operator untouched: {report['operator_untouched']}",
        "",
        "Steps:",
    ]
    for step in report["steps"]:
        badge = {"pass": "PASS", "fail": "FAIL", "skip": "SKIP"}[step["status"]]
        lines.append(f"  [{badge}] {step['name']:<20} {step['duration_s']:>6.2f}s  {step['summary']}")
    diagnostics = report.get("diagnostics")
    if isinstance(diagnostics, dict) and diagnostics:
        lines.append("")
        lines.append("Failure diagnostics:")
        for key, value in diagnostics.items():
            lines.append(f"--- {key} ---")
            lines.append(str(value) if not isinstance(value, dict) else json.dumps(value, indent=2))
    return "\n".join(lines) + "\n"


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "--from",
        dest="prev_release",
        required=True,
        help="Previous release version (e.g. 0.13.0), installer .tar.gz, or unpacked directory",
    )
    parser.add_argument(
        "--candidate",
        "--to",
        dest="candidate",
        required=True,
        help="Candidate bundle directory, .tar.gz archive, or published SemVer version (e.g. 0.14.1)",
    )
    parser.add_argument(
        "--repo",
        default=DEFAULT_REPO,
        help=f"GitHub repository owner/name for published downloads (default: {DEFAULT_REPO})",
    )
    parser.add_argument(
        "--cache-dir",
        type=Path,
        default=None,
        help="Optional directory to cache downloaded release manifests and tarballs",
    )
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=None,
        help="Optional parent directory for the disposable rehearsal sandbox",
    )
    parser.add_argument(
        "--timeout",
        type=int,
        default=DEFAULT_TIMEOUT_SECS,
        help=f"Per-phase timeout in seconds ({MIN_TIMEOUT_SECS}..{MAX_TIMEOUT_SECS}, default: {DEFAULT_TIMEOUT_SECS})",
    )
    parser.add_argument(
        "--skip-rollback",
        action="store_true",
        help="Skip rollback and second upgrade pass (still runs confirm-macos on macOS)",
    )
    parser.add_argument(
        "--json",
        action="store_true",
        help="Emit machine-readable JSON report to stdout",
    )
    parser.add_argument(
        "--report-out",
        type=Path,
        default=None,
        help="Optional path to write the JSON report",
    )
    args = parser.parse_args(argv)
    if not (MIN_TIMEOUT_SECS <= args.timeout <= MAX_TIMEOUT_SECS):
        parser.error(f"--timeout must be between {MIN_TIMEOUT_SECS} and {MAX_TIMEOUT_SECS}")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    try:
        runner = RehearsalRunner(args)
    except RehearsalError as exc:
        print(f"steve upgrade rehearsal error: {exc}", file=sys.stderr)
        return EXIT_CLEANUP_OR_USAGE_ERROR

    exit_code, report = runner.run()
    if args.report_out is not None:
        args.report_out.parent.mkdir(parents=True, exist_ok=True)
        args.report_out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    if args.json:
        print(json.dumps(report, indent=2))
    else:
        print(format_human_report(report), end="")
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
