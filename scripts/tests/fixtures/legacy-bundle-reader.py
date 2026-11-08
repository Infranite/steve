"""Frozen v0.2.76 updater contract, independent of the current packager/allowlist.

Source: https://github.com/Infranite/steve/blob/935fe4bbf05e46459a242ac933f581eb85979c2a/ops/local-release.py
Original file SHA256: 590fd6fc1c35c652cbed960b6a904ea90e0f384336d608a6c75a77f257898b01
Only the published unpacker and its inner/outer manifest comparison are retained.
"""
import pathlib
import shutil
import tarfile

class Refused(Exception):
    pass


BUNDLE_OPS = {"run.sh", "run-managed.sh", "backup.sh", "schema_gate.py", "local-release.py", "steve-service.py", "upgrade.py", "sign-setup.sh", "release-lib.sh"}
MAX_BUNDLE_BYTES = 2 * 1024 * 1024 * 1024



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



def verify_manifest(inner, manifest):
    if dict(inner, assets=[]) != dict(manifest, assets=[]):
        raise Refused("archive manifest differs from published release")
