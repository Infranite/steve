#!/usr/bin/env python3
"""Which Rust crates a change can affect, so local checks test only those.

    python3 scripts/affected.py [BASE] [--list | --web]

Files changed since BASE (committed, staged, unstaged and untracked) touch the workspace crates whose
directory holds them; every crate that depends on a touched crate, directly or not, and including
dev-dependencies, is affected too. By default it prints `-p a -p b ...` for cargo, nothing when no
crate is affected. `--list` prints the touched and the affected crates, `--web` whether `web/` changed.

BASE defaults to $STEVE_AFFECTED_BASE, else the merge base of HEAD with origin/main. An integration
branch's tooling passes its own base (the merge base with that branch).

`hub-rs/Cargo.lock` is compared package by package: only the crates that depend on a package version the
change added are touched, not the whole workspace. The workspace's own
`hub-rs/Cargo.toml` (members, profiles) touches every crate, and so does a lock file that cannot be read.
"""
import json
import os
import subprocess
import sys
import tomllib

MANIFEST = "hub-rs/Cargo.toml"
LOCK = "hub-rs/Cargo.lock"


def run(*args, check=True):
    r = subprocess.run(args, capture_output=True, text=True)
    if check and r.returncode:
        sys.exit(f"affected: {' '.join(args)}: {r.stderr.strip()}")
    return r.stdout


def default_base():
    if os.environ.get("STEVE_AFFECTED_BASE"):
        return os.environ["STEVE_AFFECTED_BASE"]
    return run("git", "merge-base", "HEAD", "origin/main").strip()


def changed_files(base):
    files = set(run("git", "diff", "--name-only", base).split())
    files |= set(run("git", "ls-files", "--others", "--exclude-standard").split())
    return files


def lock_packages(text):
    """{(name, version, source): [dependency, ...]} of a Cargo.lock; members have no source."""
    return {(p["name"], p["version"], p.get("source")): p.get("dependencies", [])
            for p in tomllib.loads(text).get("package", [])}


def lock_touched(old_text, new_text, members):
    """The members that depend, directly or not, on a package the new lock adds: a new crate, or a
    version or source it did not have. A changed dependency list alone is not counted: it follows from
    such an addition (the lock then names a dependency with its version) or from a member's manifest,
    and that member is touched by its own path. The lock lists every platform's dependencies, so this
    errs on the side of testing more."""
    old, new = lock_packages(old_text), lock_packages(new_text)
    by_name = {}
    for k in new:
        by_name.setdefault(k[0], []).append(k)
    users = {}
    for k, deps in new.items():
        for d in deps:
            name, _, rest = d.partition(" ")
            version = rest.split(" ")[0] if rest else None
            for dep in by_name.get(name, []):
                if version is None or dep[1] == version:
                    users.setdefault(dep, set()).add(k)
    seen, todo = set(), [k for k in new if k not in old]
    while todo:
        k = todo.pop()
        if k not in seen:
            seen.add(k)
            # a member's own dependents are found from the workspace's manifests
            if k[2] is not None:
                todo.extend(users.get(k, ()))
    return {k[0] for k in seen if k[2] is None and k[0] in members}


def lock_change(base, members):
    """[`lock_touched`] between BASE and the working tree; every member when either lock can't be read."""
    old = subprocess.run(["git", "show", f"{base}:{LOCK}"], capture_output=True, text=True)
    try:
        with open(LOCK) as f:
            return lock_touched(old.stdout if old.returncode == 0 else "", f.read(), members)
    except (OSError, tomllib.TOMLDecodeError):
        return set(members)


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    base = args[0] if args else default_base()
    files = changed_files(base)
    if "--web" in sys.argv:
        print("yes" if any(f.startswith("web/") for f in files) else "no")
        return
    meta = json.loads(run("cargo", "metadata", "--format-version", "1", "--no-deps", "--offline",
                          "--manifest-path", MANIFEST))
    root = os.path.abspath(".")
    dirs = {p["name"]: os.path.relpath(os.path.dirname(p["manifest_path"]), root) for p in meta["packages"]}
    rdeps = {n: set() for n in dirs}
    for p in meta["packages"]:
        for d in p["dependencies"]:
            if d["name"] in dirs and d.get("kind") in (None, "normal", "dev", "build"):
                rdeps[d["name"]].add(p["name"])
    touched = {n for f in files for n, d in dirs.items() if f.startswith(d + "/")}
    if MANIFEST in files:
        touched = set(dirs)
    elif LOCK in files:
        touched |= lock_change(base, set(dirs))
    closure, todo = set(touched), list(touched)
    while todo:
        for r in rdeps[todo.pop()]:
            if r not in closure:
                closure.add(r)
                todo.append(r)
    if "--list" in sys.argv:
        print("touched:", " ".join(sorted(touched)) or "-")
        print("affected:", " ".join(sorted(closure)) or "-")
        return
    print(" ".join(f"-p {n}" for n in sorted(closure)))


if __name__ == "__main__":
    main()
