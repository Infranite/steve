#!/usr/bin/env python3
"""Package already-built Rust binaries, static Web assets and verified restic; merge independently built platform manifests.

Only explicit build outputs enter a release. Never archive runtime shell/Python scripts, the checkout, or STEVE_HOME.
"""
import argparse
import bz2
import hashlib
import importlib.util
import json
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path, PurePosixPath


RESTIC_VERSION = "0.19.1"
# Published in restic's v0.19.1 SHA256SUMS. Pin the checksums independently of the downloads.
RESTIC_ASSETS = {
    "aarch64-apple-darwin": ("darwin_arm64", "7be0a144ccc377880f294204aa271d76e4b79554b42a751151d425ce6ebac143"),
    "x86_64-unknown-linux-gnu": ("linux_amd64", "f415415624dcc452f2a02b8c33641791a8c6d6d3b65bbb3543fcf9a25151585c"),
}
VERSION_RE = re.compile(r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)")
COMMIT_RE = re.compile(r"[0-9a-f]{40}")


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
    # The official build job pins the upstream bytes once before packaging. Preserve
    # that exact verified executable in both archives; do not download it again here.
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
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root)
        if path.is_file() and (rel.parts[0] == "ops" or path.suffix in {".py", ".sh"}):
            raise ValueError(f"runtime script forbidden in release archive: {rel}")
    write(root / "release.json", dict(manifest, assets=[]))
    with tarfile.open(asset, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
        for path in sorted(root.rglob("*")):
            archive.add(path, arcname=str(path.relative_to(root)), recursive=False)
    write(platform_manifest, dict(manifest, assets=[dict(
        target=target, name=asset.name,
        sha256=hashlib.sha256(asset.read_bytes()).hexdigest(), size=asset.stat().st_size)]))


def validate_identity_args(version, commit, target):
    if not VERSION_RE.fullmatch(version):
        raise ValueError("version must be a stable SemVer without v")
    if not COMMIT_RE.fullmatch(commit):
        raise ValueError("commit must be a full Git SHA")
    if target not in RESTIC_ASSETS:
        raise ValueError("unsupported release target")


def verify_binary_info(binary, version, commit, target, label):
    identity = json.loads(subprocess.check_output([str(binary.resolve()), "--build-info"], timeout=15))
    if identity.get("version") != version or identity.get("commit") != commit or identity.get("target") != target:
        raise ValueError(f"{label} build identity does not match release")
    return identity


def verify_asset_entry(asset, bundle_dir):
    bundle = bundle_dir / asset["name"]
    if (
        not bundle.is_file()
        or bundle.stat().st_size != asset["size"]
        or hashlib.sha256(bundle.read_bytes()).hexdigest() != asset["sha256"]
    ):
        raise ValueError("bundle does not match platform manifest")
    return bundle


def package(a):
    validate_identity_args(a.version, a.commit, a.target)
    binary, web, out = Path(a.binary), Path(a.web), Path(a.out)
    node = Path(a.node)
    helper = Path(a.keyhelper) if getattr(a, "keyhelper", None) else None
    if not node.is_file():
        raise ValueError("build Node before packaging")
    if a.target == "aarch64-apple-darwin" and (not helper or not helper.is_file()):
        raise ValueError("build keyhelper before packaging for macOS")
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
    for path in [binary, node, *([helper] if helper else []), *installer_files, web, *web.rglob("*")]:
        if path.is_symlink():
            raise ValueError(f"symlink in build output: {path}")
    identity = verify_binary_info(binary, a.version, a.commit, a.target, "binary")
    verify_binary_info(node, a.version, a.commit, a.target, "Node")
    if installer:
        verify_binary_info(cli, a.version, a.commit, a.target, "CLI")
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
            shutil.copyfile(cli, root / "bin/steve")
            (root / "bin/steve").chmod(0o755)
            shutil.copyfile(license_file, root / "LICENSE")
            (root / "LICENSE").chmod(0o644)
            archive_bundle(root, dict(manifest, cli_protocol=2), a.target,
                           out / f"steve-{a.version}-{a.target}-installer.tar.gz",
                           out / f"installer-manifest-{a.target}.json")


def safe_extract(archive_path, dest):
    seen = set()
    with tarfile.open(archive_path, "r:gz") as archive:
        for member in archive:
            name = member.name.rstrip("/")
            part = PurePosixPath(name)
            if (
                not name
                or part.is_absolute()
                or ".." in part.parts
                or "\\" in name
                or str(part) != name
                or name in seen
            ):
                raise ValueError(f"unsafe or duplicate archive path: {member.name}")
            seen.add(name)
            if member.isdir():
                (dest / part).mkdir(parents=True, exist_ok=True)
            elif member.isfile():
                if part.parts[0] == "ops" or name.endswith((".py", ".sh")):
                    raise ValueError(f"runtime script forbidden in release archive: {name}")
                target = dest / part
                target.parent.mkdir(parents=True, exist_ok=True)
                with archive.extractfile(member) as src, target.open("wb") as dst:
                    shutil.copyfileobj(src, dst)
                target.chmod(0o755 if member.mode & 0o111 else 0o644)
            else:
                raise ValueError(f"unsupported archive member type: {member.name}")


def load_tap_verifier():
    tap_path = Path(__file__).resolve().parent.parent / "packaging/homebrew/tap/sync.py"
    spec = importlib.util.spec_from_file_location("homebrew_sync", tap_path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def load_and_check_platform_manifest(manifest_path, bundle_dir, expected, expected_asset_name, target):
    manifest = json.loads(manifest_path.read_text())
    if dict(manifest, assets=[]) != expected:
        raise ValueError(f"platform manifest does not match release identity: {manifest_path.name}")
    assets = manifest.get("assets")
    if not isinstance(assets, list) or len(assets) != 1:
        raise ValueError(f"platform manifest must list one asset: {manifest_path.name}")
    asset = assets[0]
    if asset.get("target") != target or asset.get("name") != expected_asset_name:
        raise ValueError(f"platform manifest asset does not match target/version: {manifest_path.name}")
    bundle = verify_asset_entry(asset, bundle_dir)
    return manifest, bundle


def verify(a):
    validate_identity_args(a.version, a.commit, a.target)
    out = Path(a.out).resolve(strict=True)
    manifest_path = out / f"manifest-{a.target}.json"
    if not manifest_path.is_file():
        raise ValueError(f"missing platform manifest: {manifest_path}")
    raw_manifest = json.loads(manifest_path.read_text())
    schema = raw_manifest.get("schema")
    if (
        not isinstance(schema, dict)
        or not all(type(schema.get(k)) is int and schema[k] > 0 for k in ("min_read", "max_read", "target"))
        or not (schema["min_read"] <= schema["target"] <= schema["max_read"])
    ):
        raise ValueError("platform manifest has invalid schema")
    expected_runtime = dict(
        format_version=1,
        version=a.version,
        commit=a.commit,
        hub_version=a.version,
        web_version=a.version,
        api_protocol=1,
        schema=schema,
        restic_version=RESTIC_VERSION,
        assets=[],
    )
    _, runtime_archive = load_and_check_platform_manifest(
        manifest_path, out, expected_runtime, f"steve-{a.version}-{a.target}.tar.gz", a.target
    )
    identity_script = Path(
        getattr(a, "identity_script", None) or Path(__file__).resolve().parent.parent / "ops/test-release-identity.py"
    ).resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="steve-verify-") as tmp:
        tmp_path = Path(tmp)
        runtime_dir = tmp_path / "runtime"
        safe_extract(runtime_archive, runtime_dir)
        inner_path = runtime_dir / "release.json"
        if not inner_path.is_file() or json.loads(inner_path.read_text()) != expected_runtime:
            raise ValueError("runtime release.json does not match platform manifest")
        web_dir = runtime_dir / "web/dist"
        web_release_path = web_dir / "release.json"
        if not (web_dir / "index.html").is_file() or not web_release_path.is_file():
            raise ValueError("runtime bundle is missing packaged web assets")
        web_identity = json.loads(web_release_path.read_text())
        if web_identity.get("version") != a.version or web_identity.get("commit") != a.commit:
            raise ValueError("runtime web identity does not match release")
        executables = ["bin/steve-hub", "bin/steve-node", "bin/restic"]
        if a.target == "aarch64-apple-darwin" or (runtime_dir / "bin/steve-keyhelper").exists():
            executables.append("bin/steve-keyhelper")
        for rel in executables:
            path = runtime_dir / rel
            if path.is_symlink() or not path.is_file() or not (path.stat().st_mode & 0o111):
                raise ValueError(f"runtime bundle missing executable {rel}")
        license_path = runtime_dir / "bin/restic.LICENSE"
        if license_path.is_symlink() or not license_path.is_file() or not license_path.stat().st_size:
            raise ValueError("runtime bundle missing bin/restic.LICENSE")
        verify_binary_info(runtime_dir / "bin/steve-node", a.version, a.commit, a.target, "Node")
        hub_info = verify_binary_info(runtime_dir / "bin/steve-hub", a.version, a.commit, a.target, "binary")
        if hub_info.get("schema") != schema["target"]:
            raise ValueError("runtime Hub schema does not match manifest")
        subprocess.run(
            [
                sys.executable,
                str(identity_script),
                str(runtime_dir / "bin/steve-hub"),
                "--version",
                a.version,
                "--commit",
                a.commit,
                "--target",
                a.target,
                "--web",
                str(web_dir),
            ],
            check=True,
        )
        print(f"PASS verified runtime bundle {runtime_archive.name}")
        if getattr(a, "installer", False):
            installer_manifest_path = out / f"installer-manifest-{a.target}.json"
            if not installer_manifest_path.is_file():
                raise ValueError(f"missing installer platform manifest: {installer_manifest_path}")
            expected_installer = dict(expected_runtime, cli_protocol=2)
            installer_manifest, installer_archive = load_and_check_platform_manifest(
                installer_manifest_path,
                out,
                expected_installer,
                f"steve-{a.version}-{a.target}-installer.tar.gz",
                a.target,
            )
            tap = load_tap_verifier()
            tap.verify_bundle(installer_archive, installer_manifest, a.target)
            installer_dir = tmp_path / "installer"
            safe_extract(installer_archive, installer_dir)
            verify_binary_info(installer_dir / "bin/steve", a.version, a.commit, a.target, "CLI")
            print(f"PASS verified installer bundle {installer_archive.name}")


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
            verify_asset_entry(asset, Path(a.out).parent)
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
    pack.add_argument("--installer", action="store_true", help="also package the standalone CLI installer")
    pack.add_argument("--cli", help="prebuilt native steve binary (required with --installer)")
    pack.add_argument("--license")
    pack.add_argument("--restic-dir", help="trusted build output containing the verified restic and license")
    check = sub.add_parser("verify", help="verify packaged release archives and boot the unpacked Hub")
    for name in ("version", "commit", "target", "out"):
        check.add_argument("--" + name, required=True)
    check.add_argument("--installer", action="store_true", help="also verify the standalone CLI installer archive")
    check.add_argument("--identity-script", type=Path, help="override ops/test-release-identity.py path")
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
    elif args.action == "verify":
        verify(args)
    elif args.action == "restic":
        restic(args.target, args.out)
    else:
        merge(args)
