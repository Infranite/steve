#!/usr/bin/env python3
"""Package already-built Hub/Node/Web and explicitly selected runtime scripts; merge independently built platform manifests.

Only explicit build outputs enter a release. Never archive the checkout or STEVE_HOME.
"""
import argparse
import hashlib
import json
import re
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path


RUNTIME_OPS = ("run.sh", "run-managed.sh", "backup.sh", "schema_gate.py", "local-release.py", "steve-service.py", "upgrade.py", "sign-setup.sh", "release-lib.sh")


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n")


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
    for path in [binary, node, *([helper] if helper else []), *runtime, web, *web.rglob("*")]:
        if path.is_symlink():
            raise ValueError(f"symlink in build output: {path}")
    identity = json.loads(subprocess.check_output([str(binary.resolve()), "--build-info"], timeout=15))
    if identity.get("version") != a.version or identity.get("commit") != a.commit or identity.get("target") != a.target:
        raise ValueError("binary build identity does not match release")
    node_identity = json.loads(subprocess.check_output([str(node.resolve()), "--build-info"], timeout=15))
    if any(node_identity.get(k) != getattr(a, k) for k in ("version", "commit", "target")):
        raise ValueError("Node build identity does not match release")
    schema = identity.get("schema")
    if not isinstance(schema, int) or schema < 1:
        raise ValueError("binary did not report a valid schema")
    out.mkdir(parents=True, exist_ok=True)
    manifest = dict(format_version=1, version=a.version, commit=a.commit,
                    hub_version=a.version, web_version=a.version, api_protocol=1,
                    schema=dict(min_read=schema, max_read=schema, target=schema), assets=[])
    with tempfile.TemporaryDirectory(prefix="steve-bundle-") as tmp:
        root = Path(tmp)
        (root / "bin").mkdir()
        shutil.copyfile(binary, root / "bin/steve-hub")
        (root / "bin/steve-hub").chmod(0o755)
        shutil.copyfile(node, root / "bin/steve-node")
        (root / "bin/steve-node").chmod(0o755)
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
        write(root / "release.json", manifest)
        asset = out / f"steve-{a.version}-{a.target}.tar.gz"
        with tarfile.open(asset, "w:gz", format=tarfile.USTAR_FORMAT) as archive:
            for path in sorted(root.rglob("*")):
                archive.add(path, arcname=str(path.relative_to(root)), recursive=False)
        manifest["assets"] = [dict(target=a.target, name=asset.name,
            sha256=hashlib.sha256(asset.read_bytes()).hexdigest(), size=asset.stat().st_size)]
        write(out / f"manifest-{a.target}.json", manifest)


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
    join = sub.add_parser("merge")
    join.add_argument("--out", required=True)
    join.add_argument("--expected-target", action="append")
    join.add_argument("manifests", nargs="+")
    args = parser.parse_args()
    if args.action == "package":
        package(args)
    else:
        merge(args)
