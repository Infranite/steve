#!/usr/bin/env python3
"""Package already-built Hub/Node/Web and explicitly selected runtime scripts; merge independently built platform manifests.

Only explicit build outputs enter a release. Never archive the checkout or STEVE_HOME.
"""
import argparse
import bz2
import hashlib
import json
import re
import shutil
import subprocess
import tarfile
import tempfile
import urllib.request
from pathlib import Path


RUNTIME_OPS = ("run.sh", "run-managed.sh", "schema_gate.py", "local-release.py", "steve-service.py", "upgrade.py", "sign-setup.sh", "release-lib.sh")
INSTALLER_OPS = ("run.sh", "run-managed.sh", "sign-setup.sh", "release-lib.sh")
RESTIC_VERSION = "0.19.1"
# Published in restic's v0.19.1 SHA256SUMS. Pin the checksums independently of the downloads.
RESTIC_ASSETS = {
    "aarch64-apple-darwin": ("darwin_arm64", "7be0a144ccc377880f294204aa271d76e4b79554b42a751151d425ce6ebac143"),
    "x86_64-unknown-linux-gnu": ("linux_amd64", "f415415624dcc452f2a02b8c33641791a8c6d6d3b65bbb3543fcf9a25151585c"),
}


def verified_download(url, sha256):
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read(48 * 1024 * 1024)
    if hashlib.sha256(data).hexdigest() != sha256:
        raise ValueError(f"upstream SHA-256 mismatch: {url}")
    return data


def restic(target, out):
    suffix, sha256 = RESTIC_ASSETS[target]
    tag = f"v{RESTIC_VERSION}"
    filename = f"restic_{RESTIC_VERSION}_{suffix}.bz2"
    binary = verified_download(f"https://github.com/restic/restic/releases/download/{tag}/{filename}", sha256)
    license_text = verified_download(
        f"https://raw.githubusercontent.com/restic/restic/{tag}/LICENSE",
        "6f08a01a9fab5b24e139a09f15cc24a73087c7bc09e3bacf099fdf2d767bf897",
    )
    out.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=out) as tmp:
        executable = Path(tmp) / "restic"
        executable.write_bytes(bz2.decompress(binary))
        executable.chmod(0o755)
        license_file = Path(tmp) / "restic.LICENSE"
        license_file.write_bytes(license_text)
        license_file.replace(out / license_file.name)
        executable.replace(out / executable.name)



def copy_staged_restic(source, out):
    # The official build job pins the upstream bytes before signing on macOS. Preserve
    # that exact signed executable in both archives; do not download it again here.
    for name, mode in (("restic", 0o755), ("restic.LICENSE", 0o644)):
        path = source / name
        if path.is_symlink() or not path.is_file() or not path.stat().st_size:
            raise ValueError(f"invalid staged backup engine file: {path}")
        if name == "restic" and not path.stat().st_mode & 0o111:
            raise ValueError("staged backup engine is not executable")
        shutil.copyfile(path, out / name)
        (out / name).chmod(mode)

def write(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


def archive_bundle(root, manifest, target, asset, platform_manifest):
    write(root / "release.json", dict(manifest, assets=[]))
    with tarfile.open(asset, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
        for path in sorted(root.rglob("*")):
            archive.add(path, arcname=str(path.relative_to(root)), recursive=False)
    write(platform_manifest, dict(manifest, assets=[dict(
        target=target, name=asset.name,
        sha256=hashlib.sha256(asset.read_bytes()).hexdigest(), size=asset.stat().st_size)]))


def package(a):
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", a.version):
        raise ValueError("version must be a stable SemVer without v")
    if not re.fullmatch(r"[0-9a-f]{40}", a.commit):
        raise ValueError("commit must be a full Git SHA")
    if a.target not in {"aarch64-apple-darwin", "x86_64-unknown-linux-gnu"}:
        raise ValueError("unsupported release target")
    binary, web, out = Path(a.binary), Path(a.web), Path(a.out)
    node = Path(a.node)
    helper = Path(a.keyhelper) if getattr(a, "keyhelper", None) else None
    ops = Path(getattr(a, "ops", None) or Path(__file__).resolve().parent.parent / "ops")
    if not node.is_file():
        raise ValueError("build Node before packaging")
    if a.target == "aarch64-apple-darwin" and (not helper or not helper.is_file()):
        raise ValueError("build keyhelper before packaging for macOS")
    runtime = [ops / name for name in RUNTIME_OPS]
    if any(not p.is_file() for p in runtime):
        raise ValueError("missing runtime ops scripts")
    if not binary.is_file() or not (web / "index.html").is_file():
        raise ValueError("build Hub and Web before packaging")
    installer = getattr(a, "installer", False)
    cli = Path(a.cli) if getattr(a, "cli", None) else None
    license_file = Path(getattr(a, "license", None) or Path(__file__).resolve().parent.parent / "LICENSE")
    if installer and cli is None:
        raise ValueError("installer requires a prebuilt --cli binary and LICENSE")
    installer_files = [cli, license_file] if installer else []
    if any(not p.is_file() for p in installer_files):
        raise ValueError("installer requires a prebuilt --cli binary and LICENSE")
    for path in [binary, node, *([helper] if helper else []), *runtime, *installer_files, web, *web.rglob("*")]:
        if path.is_symlink():
            raise ValueError(f"symlink in build output: {path}")
    identity = json.loads(subprocess.check_output([str(binary.resolve()), "--build-info"], timeout=15))
    if identity.get("version") != a.version or identity.get("commit") != a.commit or identity.get("target") != a.target:
        raise ValueError("binary build identity does not match release")
    node_identity = json.loads(subprocess.check_output([str(node.resolve()), "--build-info"], timeout=15))
    if any(node_identity.get(k) != getattr(a, k) for k in ("version", "commit", "target")):
        raise ValueError("Node build identity does not match release")
    if installer:
        cli_identity = json.loads(subprocess.check_output([str(cli.resolve()), "--build-info"], timeout=15))
        if any(cli_identity.get(k) != getattr(a, k) for k in ("version", "commit", "target")):
            raise ValueError("CLI build identity does not match release")
    schema = identity.get("schema")
    if not isinstance(schema, int) or schema < 1:
        raise ValueError("binary did not report a valid schema")
    out.mkdir(parents=True, exist_ok=True)
    manifest = dict(format_version=1, version=a.version, commit=a.commit,
                    hub_version=a.version, web_version=a.version, api_protocol=1,
                    schema=dict(min_read=schema, max_read=schema, target=schema),
                    restic_version=RESTIC_VERSION, assets=[])
    with tempfile.TemporaryDirectory(prefix="steve-bundle-") as tmp:
        root = Path(tmp)
        (root / "bin").mkdir()
        shutil.copyfile(binary, root / "bin/steve-hub")
        (root / "bin/steve-hub").chmod(0o755)
        shutil.copyfile(node, root / "bin/steve-node")
        (root / "bin/steve-node").chmod(0o755)
        if getattr(a, "restic_dir", None):
            copy_staged_restic(Path(a.restic_dir), root / "bin")
        else:
            restic(a.target, root / "bin")
        if helper:
            shutil.copyfile(helper, root / "bin/steve-keyhelper")
            (root / "bin/steve-keyhelper").chmod(0o755)
        (root / "ops").mkdir()
        for path in runtime:
            shutil.copyfile(path, root / "ops" / path.name)
            (root / "ops" / path.name).chmod(0o755)
        shutil.copytree(web, root / "web/dist")
        index = root / "web/dist/index.html"
        html = index.read_text()
        html = re.sub(r'<meta\s+name=["\']steve-web-version["\'][^>]*>', '', html)
        if "</head>" not in html:
            raise ValueError("Web index has no head")
        index.write_text(html.replace("</head>", f'<meta name="steve-web-version" content="{a.version}" /></head>', 1))
        write(root / "web/dist/release.json", dict(version=a.version, commit=a.commit))
        asset = out / f"steve-{a.version}-{a.target}.tar.gz"
        archive_bundle(root, manifest, a.target, asset, out / f"manifest-{a.target}.json")
        if installer:
            # The native installer omits the Python bridge after the runtime archive is complete.
            for name in RUNTIME_OPS:
                if name not in INSTALLER_OPS:
                    (root / "ops" / name).unlink()
            shutil.copyfile(cli, root / "bin/steve")
            (root / "bin/steve").chmod(0o755)
            shutil.copyfile(license_file, root / "LICENSE")
            (root / "LICENSE").chmod(0o644)
            archive_bundle(root, dict(manifest, cli_protocol=2), a.target,
                           out / f"steve-{a.version}-{a.target}-installer.tar.gz",
                           out / f"installer-manifest-{a.target}.json")


def merge(a):
    manifests = [json.loads(Path(p).read_text()) for p in a.manifests]
    result = dict(manifests[0], assets=[])
    targets = set()
    for manifest in manifests:
        if dict(manifest, assets=[]) != dict(result, assets=[]):
            raise ValueError("platform builds disagree on release identity")
        for asset in manifest["assets"]:
            if asset["target"] in targets:
                raise ValueError("duplicate platform")
            targets.add(asset["target"])
            bundle = Path(a.out).parent / asset["name"]
            if not bundle.is_file() or bundle.stat().st_size != asset["size"] or hashlib.sha256(bundle.read_bytes()).hexdigest() != asset["sha256"]:
                raise ValueError("bundle does not match platform manifest")
            result["assets"].append(asset)
    if getattr(a, "expected_target", None) and targets != set(a.expected_target):
        raise ValueError("release is missing expected platforms")
    write(Path(a.out), result)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="action", required=True)
    pack = sub.add_parser("package")
    for name in ("version", "commit", "target", "binary", "node", "web", "out"):
        pack.add_argument("--" + name, required=True)
    pack.add_argument("--keyhelper")
    pack.add_argument("--ops")
    pack.add_argument("--installer", action="store_true", help="also package the standalone CLI installer")
    pack.add_argument("--cli", help="prebuilt native steve binary (required with --installer)")
    pack.add_argument("--license")
    pack.add_argument("--restic-dir", help="trusted build output containing the verified restic and license")
    tool = sub.add_parser("restic", help="stage the verified upstream restic binary and license")
    tool.add_argument("--target", required=True, choices=RESTIC_ASSETS)
    tool.add_argument("--out", required=True, type=Path)
    join = sub.add_parser("merge")
    join.add_argument("--out", required=True)
    join.add_argument("--expected-target", action="append")
    join.add_argument("manifests", nargs="+")
    args = parser.parse_args()
    if args.action == "package":
        package(args)
    elif args.action == "restic":
        restic(args.target, args.out)
    else:
        merge(args)
