#!/usr/bin/env python3
"""Install CI's pinned nextest into a new RUNNER_TEMP child, then append GITHUB_PATH.

Only official GitHub assets are used. The public fallback changes a harmless query
on each attempt to avoid a failed cached redirect; it never changes the artifact.
--directory selects the temporary parent, not an existing tool installation.
"""
import argparse
import hashlib
import http.client
import os
from pathlib import Path
import platform
import re
import shutil
import ssl
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

VERSION = "0.9.151"
REPOSITORY = "nextest-rs/nextest"
# Official release API for cargo-nextest-0.9.151; digests cover the complete tar.gz.
ASSETS = {
    "universal-apple-darwin": {
        "id": 703616437, "size": 16809298,
        "sha256": "d1d7d025d4bfc9fb154cd76aea3949bd78534829a0589abefa6e5dd4edb135dd",
    },
    "x86_64-unknown-linux-gnu": {
        "id": 703600788, "size": 12161169,
        "sha256": "1d4ce302b820c41f4b89c5fb9bce45f0f0c94882c1cf08fc360caf08be6e7df4",
    },
}
CHUNK = 64 * 1024
MAX_BINARY = 128 * 1024 * 1024
SOCKET_TIMEOUT = 20
DOWNLOAD_SECONDS = 90


class Refused(Exception):
    pass


def select_asset(system=None, machine=None):
    system, machine = system or platform.system(), machine or platform.machine()
    if system == "Darwin" and machine in ("arm64", "aarch64", "x86_64", "amd64"):
        target = "universal-apple-darwin"
    elif system == "Linux" and machine in ("x86_64", "amd64"):
        target = "x86_64-unknown-linux-gnu"
    else:
        raise Refused("unsupported CI platform; no version, source-build or mirror fallback")
    return dict(ASSETS[target], target=target, name=f"cargo-nextest-{VERSION}-{target}.tar.gz")


def github_url(value):
    url = urllib.parse.urlsplit(value)
    try:
        valid_port = url.port in (None, 443)
    except ValueError:
        valid_port = False
    if (url.scheme != "https" or url.username or url.password or url.fragment or not valid_port
            or any(c.isspace() or ord(c) < 32 for c in value)
            or not (url.hostname in ("api.github.com", "github.com")
                    or (url.hostname or "").endswith(".githubusercontent.com"))):
        raise Refused("download redirected outside the permitted GitHub HTTPS hosts")
    return url.hostname


class GitHubRedirect(urllib.request.HTTPRedirectHandler):
    max_redirections = 5
    max_repeats = 2

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        host = github_url(newurl)
        result = super().redirect_request(req, fp, code, msg, headers, newurl)
        if result is not None and host != "api.github.com":
            # urllib copies ordinary request headers on redirects, including Authorization.
            for group in (result.headers, result.unredirected_hdrs):
                for name in list(group):
                    if name.lower() == "authorization":
                        del group[name]
        return result


def download(asset, destination, opener=None, token=None, nonce=None, now=None):
    opener = opener or urllib.request.build_opener(
        GitHubRedirect(), urllib.request.HTTPSHandler(context=ssl.create_default_context()))
    nonce = nonce or (lambda: uuid.uuid4().hex)
    now = now or time.monotonic
    base = f"https://github.com/{REPOSITORY}/releases/download/cargo-nextest-{VERSION}/{asset['name']}"
    errors = []
    # One API route, then two fresh public requests. Never retry the known failed bare URL.
    for route in ("API", "public-1", "public-2"):
        headers = {"Accept": "application/octet-stream", "User-Agent": "Steve-CI-nextest/1"}
        if route == "API":
            url = f"https://api.github.com/repos/{REPOSITORY}/releases/assets/{asset['id']}"
            if token:
                headers["Authorization"] = "Bearer " + token
        else:
            url = base + "?steve_ci=" + urllib.parse.quote(nonce(), safe="")
        github_url(url)
        total, sha = 0, hashlib.sha256()
        deadline = now() + DOWNLOAD_SECONDS
        try:
            request = urllib.request.Request(url, headers=headers)
            with opener.open(request, timeout=SOCKET_TIMEOUT) as response:
                github_url(response.geturl())
                if response.status != 200:
                    raise Refused("unexpected HTTP response status")
                length = response.headers.get("Content-Length")
                if length is not None and (not length.isdigit() or int(length) != asset["size"]):
                    raise Refused("Content-Length differs from the pinned asset size")
                if response.headers.get("Content-Encoding", "identity") != "identity":
                    raise Refused("unexpected HTTP content encoding")
                with destination.open("wb") as output:
                    while True:
                        if now() > deadline:
                            raise Refused("download exceeded its time limit")
                        # read1 returns after a buffered/socket read, so slow streaming cannot
                        # indefinitely hide the overall deadline inside a full-block read.
                        block = response.read1(CHUNK)
                        if not block:
                            break
                        total += len(block)
                        if total > asset["size"]:
                            raise Refused("download exceeds the pinned asset size")
                        sha.update(block)
                        output.write(block)
            if total != asset["size"] or sha.hexdigest() != asset["sha256"]:
                raise Refused("download size or SHA256 differs from the pinned asset")
            return route
        except urllib.error.HTTPError as error:
            reason = f"HTTP {error.code}"
        except Refused as error:
            reason = str(error)
        except (OSError, urllib.error.URLError, http.client.HTTPException, ValueError):
            reason = "network/TLS or download I/O failed"
        # Never print an exception's URL: redirects can contain signed query credentials.
        destination.unlink(missing_ok=True)
        errors.append(route + ": " + reason)
    raise Refused("official nextest download failed (" + "; ".join(errors) + ")")


def extract_binary(archive, destination):
    with tarfile.open(archive, "r:gz") as bundle:
        members = []
        for member in bundle:
            if (members or member.name != "cargo-nextest"
                    or member.type not in (tarfile.REGTYPE, tarfile.AREGTYPE)
                    or member.pax_headers or not 0 < member.size <= MAX_BINARY):
                raise Refused("archive must contain only one regular cargo-nextest binary")
            members.append(member)
        if len(members) != 1:
            raise Refused("archive has no cargo-nextest binary")
        with bundle.extractfile(members[0]) as source, destination.open("xb") as output:
            total = 0
            while True:
                block = source.read(CHUNK)
                if not block:
                    break
                total += len(block)
                if total > members[0].size:
                    raise Refused("unpacked binary exceeds its declared size")
                output.write(block)
        if total != members[0].size:
            raise Refused("unpacked binary is incomplete")
    destination.chmod(0o755)


def verify_version(binary, runner=None):
    result = (runner or subprocess.run)(
        [str(binary), "nextest", "--version"], capture_output=True, text=True,
        timeout=15, check=False, env={"PATH": os.defpath, "LANG": "C", "NO_COLOR": "1"})
    lines = result.stdout.splitlines()
    if (result.returncode or len(result.stdout) > 16384 or not lines
            or not re.fullmatch(r"cargo-nextest " + re.escape(VERSION) + r"(?: \([^\r\n]*\))?", lines[0])
            or any(line.startswith("release:") and line != "release: " + VERSION for line in lines[1:])):
        raise Refused("downloaded binary did not report exactly cargo-nextest " + VERSION)


def install(directory, github_path, *, asset=None, opener=None, runner=None, token=None):
    directory, github_path = Path(directory).expanduser().resolve(), Path(github_path).expanduser().absolute()
    if any(c in str(directory) + str(github_path) for c in "\r\n\0"):
        raise Refused("installation and GITHUB_PATH paths cannot contain line breaks or NUL")
    if not directory.is_dir() or not github_path.parent.is_dir():
        raise Refused("temporary parent and GITHUB_PATH parent must already exist")
    if os.path.lexists(github_path) and not stat.S_ISREG(github_path.lstat().st_mode):
        raise Refused("GITHUB_PATH must be a regular file, not a link")
    asset = asset or select_asset()
    target = Path(tempfile.mkdtemp(prefix=f"steve-nextest-{VERSION}-", dir=directory))
    try:
        archive = target / "download.tar.gz"
        route = download(asset, archive, opener=opener, token=token)
        binary = target / "cargo-nextest"
        extract_binary(archive, binary)
        verify_version(binary, runner=runner)
        archive.unlink()
        fd = os.open(github_path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0), 0o600)
        with os.fdopen(fd, "w") as output:
            output.write(str(target) + "\n")
            output.flush()
            os.fsync(output.fileno())
        print(f"Verified cargo-nextest {VERSION} ({asset['target']}, {route}); added its temporary directory to GITHUB_PATH")
        return target
    except BaseException:
        shutil.rmtree(target)
        raise


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--directory", default=os.environ.get("RUNNER_TEMP"), help="existing temporary parent (default: RUNNER_TEMP)")
    parser.add_argument("--github-path", default=os.environ.get("GITHUB_PATH"), help="GitHub PATH command file (required outside Actions)")
    args = parser.parse_args(argv)
    try:
        if not args.directory or not args.github_path:
            raise Refused("set RUNNER_TEMP and GITHUB_PATH, or pass --directory and --github-path")
        install(args.directory, args.github_path, token=os.environ.get("GITHUB_TOKEN"))
        return 0
    except (Refused, OSError, ValueError, tarfile.TarError, subprocess.SubprocessError) as error:
        # Unexpected library errors may include URLs/output; expose only our own safe messages.
        message = str(error) if isinstance(error, Refused) else "installation failed (" + type(error).__name__ + ")"
        print("nextest installer: " + message, file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
