#!/usr/bin/env python3
"""Update this tap only from a complete, immutable public Steve installer release.

This script runs in the tap with its own GITHUB_TOKEN. It downloads and verifies
release files; it never installs Steve, starts a service or executes a bundle.
"""
import argparse
import contextlib
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import tarfile
import tempfile
import urllib.parse
import urllib.request


TARGETS = ("aarch64-apple-darwin", "x86_64-unknown-linux-gnu")
MAX_JSON = 1024 * 1024
MAX_BUNDLE = 2 * 1024 * 1024 * 1024
MANIFEST_NAME = "steve-installer.json"
LOCK_NAME = "steve-installer.lock.json"
VERSION = r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)"
INSTALLER_OPS = {"run.sh", "run-managed.sh", "backup.sh", "sign-setup.sh", "release-lib.sh"}
REQUIRED = {"release.json", "LICENSE", "bin/steve", "bin/steve-hub", "bin/steve-node", "bin/steve-keyhelper",
            "web/dist/index.html", "web/dist/release.json", *("ops/" + name for name in INSTALLER_OPS)}


class Refused(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Refused(message)


def json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True) + "\n").encode()


def read_json(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON key")
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=unique)


def digest(data):
    return hashlib.sha256(data).hexdigest()


class Redirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        url = urllib.parse.urlparse(newurl)
        try:
            valid_port = url.port in (None, 443)
        except ValueError:
            valid_port = False
        require(url.scheme == "https" and not url.username and not url.password and not url.fragment
                and valid_port
                and (url.hostname in {"api.github.com", "github.com"}
                     or (url.hostname or "").endswith(".githubusercontent.com")),
                "release redirect left GitHub HTTPS")
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is not None and url.hostname != urllib.parse.urlparse(req.full_url).hostname:
            # urllib copies ordinary headers. Drop the token before following asset
            # storage redirects; a later redirect back to the API must not restore it.
            for group in (redirected.headers, redirected.unredirected_hdrs):
                for name in list(group):
                    if name.lower() == "authorization":
                        del group[name]
        return redirected


class GitHub:
    def __init__(self):
        self.opener = urllib.request.build_opener(Redirect())

    def download(self, url, limit, destination=None, *, accept="application/octet-stream"):
        headers = {"User-Agent": "Steve-Homebrew-sync", "Accept": accept}
        if urllib.parse.urlparse(url).hostname == "api.github.com":
            token = os.environ.get("GITHUB_TOKEN")
            if token:
                headers["Authorization"] = "Bearer " + token
        total, checksum, chunks = 0, hashlib.sha256(), []
        with self.opener.open(urllib.request.Request(url, headers=headers), timeout=60) as response:
            with (destination.open("xb") if destination else contextlib.nullcontext()) as output:
                while chunk := response.read(1024 * 1024):
                    total += len(chunk)
                    require(total <= limit, "download exceeds its declared size limit")
                    checksum.update(chunk)
                    if output:
                        output.write(chunk)
                    else:
                        chunks.append(chunk)
        return (total, checksum.hexdigest()) if destination else b"".join(chunks)

    def json(self, url):
        return read_json(self.download(url, MAX_JSON, accept="application/vnd.github+json"))


def release_base(repository, version):
    return f"https://github.com/{repository}/releases/download/v{version}/"


def asset_download_url(repository, asset):
    require(type(asset.get("id")) is int and asset["id"] > 0, "invalid GitHub release asset ID")
    return f"https://api.github.com/repos/{repository}/releases/assets/{asset['id']}"


def check_manifest(manifest, *, legacy_lock=False):
    require(isinstance(manifest, dict), "installer manifest must be an object")
    require(type(manifest.get("format_version")) is int and manifest["format_version"] == 1,
            "unsupported installer manifest format")
    # An already-installed v1 lock is evidence for monotonic version checks,
    # never permission to publish a new Python-based installer.
    require(type(manifest.get("cli_protocol")) is int
            and manifest["cli_protocol"] in ({1, 2} if legacy_lock else {2}),
            "release does not provide CLI protocol 1")
    version = manifest.get("version", "")
    require(isinstance(version, str) and re.fullmatch(VERSION, version), "invalid stable release version")
    require(re.fullmatch(r"[0-9a-f]{40}", str(manifest.get("commit", ""))), "invalid release commit")
    require(manifest.get("hub_version") == version and manifest.get("web_version") == version
            and type(manifest.get("api_protocol")) is int and manifest["api_protocol"] == 1,
            "installer component identity is inconsistent")
    schema = manifest.get("schema")
    require(isinstance(schema, dict) and all(type(schema.get(k)) is int and schema[k] > 0
            for k in ("min_read", "max_read", "target"))
            and schema["min_read"] <= schema["target"] <= schema["max_read"], "invalid installer schema")
    assets = manifest.get("assets")
    require(isinstance(assets, list) and len(assets) == len(TARGETS), "installer requires both platforms")
    seen = set()
    for asset in assets:
        require(isinstance(asset, dict), "invalid installer asset")
        target = asset.get("target")
        require(target in TARGETS and target not in seen, "missing or duplicate installer platform")
        seen.add(target)
        require(asset.get("name") == f"steve-{version}-{target}-installer.tar.gz",
                "installer asset name does not match its version and platform")
        require(type(asset.get("size")) is int and 0 < asset["size"] <= MAX_BUNDLE,
                "invalid installer size")
        require(re.fullmatch(r"[0-9a-f]{64}", str(asset.get("sha256", ""))), "invalid installer SHA256")
    return manifest


def check_remote_asset(asset, repository, version, name, limit):
    require(isinstance(asset, dict) and asset.get("name") == name and asset.get("state") == "uploaded",
            f"release asset is not completely uploaded: {name}")
    require(asset.get("browser_download_url") == release_base(repository, version) + name,
            f"release asset URL is not pinned to the expected tag: {name}")
    asset_download_url(repository, asset)
    require(type(asset.get("size")) is int and 0 < asset["size"] <= limit, f"invalid asset size: {name}")
    require(re.fullmatch(r"sha256:[0-9a-f]{64}", str(asset.get("digest", ""))),
            f"GitHub did not report an asset SHA256: {name}")


def published_installer(client, repository):
    api = f"https://api.github.com/repos/{repository}/"
    release = client.json(api + "releases/latest")
    require(isinstance(release, dict) and release.get("draft") is False
            and release.get("prerelease") is False and release.get("published_at"),
            "release must be published and stable")
    tag = release.get("tag_name", "")
    require(isinstance(tag, str) and re.fullmatch("v" + VERSION, tag), "release tag is not stable SemVer")
    version = tag[1:]
    remote = {}
    require(isinstance(release.get("assets"), list), "release assets are missing")
    for asset in release["assets"]:
        require(isinstance(asset, dict) and isinstance(asset.get("name"), str)
                and asset["name"] not in remote, "duplicate or malformed release assets")
        remote[asset["name"]] = asset
    metadata = remote.get(MANIFEST_NAME)
    check_remote_asset(metadata, repository, version, MANIFEST_NAME, MAX_JSON)
    body = client.download(asset_download_url(repository, metadata), metadata["size"])
    require(len(body) == metadata["size"] and "sha256:" + digest(body) == metadata["digest"],
            "installer manifest download differs from GitHub's digest/size")
    manifest = check_manifest(read_json(body))
    require(manifest["version"] == version, "installer manifest version differs from release tag")
    obj = client.json(api + "git/ref/tags/" + tag).get("object", {})
    for _ in range(10):
        require(isinstance(obj, dict) and re.fullmatch(r"[0-9a-f]{40}", str(obj.get("sha", ""))),
                "invalid tag object")
        if obj.get("type") == "commit":
            break
        require(obj.get("type") == "tag", "release tag does not resolve to a commit")
        obj = client.json(api + "git/tags/" + obj["sha"]).get("object", {})
    else:
        raise Refused("release tag nesting is too deep")
    require(obj["sha"] == manifest["commit"], "installer commit differs from the published tag")
    for asset in manifest["assets"]:
        metadata = remote.get(asset["name"])
        check_remote_asset(metadata, repository, version, asset["name"], MAX_BUNDLE)
        require(metadata["size"] == asset["size"] and metadata["digest"] == "sha256:" + asset["sha256"],
                "installer asset differs from the published manifest")
    return manifest, remote


def verify_bundle(path, manifest, target):
    seen, content, total = set(), {}, 0
    with tarfile.open(path, "r:gz") as archive:
        for member in archive:
            name = member.name.rstrip("/")
            part = PurePosixPath(name)
            require(name and not part.is_absolute() and ".." not in part.parts and "\\" not in name
                    and str(part) == name and name not in seen and len(seen) < 100000,
                    "unsafe or duplicate installer path")
            seen.add(name)
            directory = name in {"bin", "ops", "web", "web/dist"} or name.startswith("web/dist/")
            regular = name in REQUIRED or name.startswith("web/dist/")
            require((member.isdir() and directory) or (member.isfile() and regular),
                    "unexpected installer member: " + name)
            if not member.isfile():
                continue
            total += member.size
            require(member.size >= 0 and total <= MAX_BUNDLE, "installer exceeds unpacked size limit")
            if name in REQUIRED:
                require(member.size > 0, "empty required installer file: " + name)
            if name.startswith(("bin/", "ops/")):
                require(member.mode & 0o111, "installer runtime file is not executable: " + name)
            if name == "bin/steve":
                header = archive.extractfile(member).read(20)
                native = ((target == "aarch64-apple-darwin" and header[:8] == b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01")
                          or (target == "x86_64-unknown-linux-gnu" and header[:6] == b"\x7fELF\x02\x01"
                              and header[18:20] == b"\x3e\x00"))
                require(native, "installer CLI is not a native binary for " + target)
            if name in {"release.json", "web/dist/release.json"}:
                require(member.size <= MAX_JSON, "installer metadata exceeds size limit")
                content[name] = read_json(archive.extractfile(member).read())
    require(REQUIRED <= seen, "installer is missing required runtime files")
    inner = content["release.json"]
    require(isinstance(inner, dict) and dict(inner, assets=[]) == dict(manifest, assets=[]),
            "installer inner manifest differs from the published manifest")
    web = content["web/dist/release.json"]
    require(isinstance(web, dict) and all(web.get(k) == manifest[k] for k in ("version", "commit")),
            "installer Web identity differs from the published manifest")


def render_formula(manifest, remote, repository, template):
    check_manifest(manifest)
    assets = {asset["target"]: asset for asset in manifest["assets"]}
    substitutions = {"VERSION": manifest["version"], "REPOSITORY": repository}
    for name, target in (("MAC", TARGETS[0]), ("LINUX", TARGETS[1])):
        asset = assets[target]
        metadata = remote[asset["name"]]
        asset_download_url(repository, metadata)
        substitutions[name + "_ASSET_ID"] = str(metadata["id"])
        substitutions[name + "_SHA"] = asset["sha256"]
    for name, value in substitutions.items():
        template = template.replace("@" + name + "@", value)
    require(not re.search(r"@[A-Z_]+@", template), "unfilled Formula template field")
    return template.encode()


def sync(client, root, repository="Infranite/steve"):
    require(re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository)
            and all(p not in {".", ".."} for p in repository.split("/")), "invalid upstream repository")
    manifest, remote = published_installer(client, repository)
    download_urls = {asset["target"]: asset_download_url(repository, remote[asset["name"]])
                     for asset in manifest["assets"]}
    formula_path, lock_path = root / "Formula/steve.rb", root / LOCK_NAME
    require(formula_path.exists() == lock_path.exists(), "Formula and release lock must be installed together")
    if lock_path.exists():
        lock = read_json(lock_path.read_bytes())
        require(lock.get("repository") == repository, "tap lock belongs to another upstream")
        require(digest(formula_path.read_bytes()) == lock.get("formula_sha256"),
                "existing Formula differs from its verified lock")
        old = check_manifest(lock["manifest"], legacy_lock=True)
        before, after = (tuple(map(int, m["version"].split("."))) for m in (old, manifest))
        require(after >= before, "refusing to downgrade the tap")
        if after == before:
            require(manifest == old, "published installer changed without a version change")
            require(lock.get("download_urls") == download_urls,
                    "published installer asset IDs changed without a version change")
            return dict(changed=False, version=manifest["version"])
    with tempfile.TemporaryDirectory(prefix="steve-homebrew-") as tmp:
        for asset in manifest["assets"]:
            path = Path(tmp) / asset["name"]
            size, sha = client.download(asset_download_url(repository, remote[asset["name"]]), asset["size"], path)
            require(size == asset["size"] and sha == asset["sha256"], "installer download failed size/SHA256 verification")
            verify_bundle(path, manifest, asset["target"])
    formula = render_formula(manifest, remote, repository, (root / "steve.rb.in").read_text())
    lock = json_bytes(dict(repository=repository, manifest=manifest, download_urls=download_urls,
                          formula_sha256=digest(formula)))
    # Nothing is written until every platform has passed. The workflow commits both files
    # together; a failed run cannot publish a partially generated Formula.
    formula_path.parent.mkdir(parents=True, exist_ok=True)
    formula_path.write_bytes(formula)
    lock_path.write_bytes(lock)
    return dict(changed=True, version=manifest["version"])


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tap", type=Path, default=Path(__file__).resolve().parent)
    parser.add_argument("--repository", default="Infranite/steve")
    args = parser.parse_args()
    try:
        print(json.dumps(sync(GitHub(), args.tap, args.repository)))
    except (ValueError, OSError, tarfile.TarError) as error:
        parser.exit(1, f"Homebrew sync refused: {error}\n")
