#!/usr/bin/env python3
"""Cut a release: the version is chosen here, at release time, never ahead on a branch.

  cut-release.py next [--minor]                    the version the next release gets
  cut-release.py cut [--minor | --version X.Y.Z] [--branch B] [--dry-run]
  cut-release.py lint [--base REF]                 a branch carries release-note fragments only

Release notes are fragments: each change adds `docs/releases/unreleased/<name>.md` (its item, e.g.
`ST-38.md`, or its branch), a few sentences or bullets, no version and no `# ` title. `cut` takes the
target branch as it is on `origin` (in a worktree of its own, so the caller's checkout does not matter),
gives it the next free version (one more than the highest `vX.Y.Z` tag on `origin`), joins the fragments
in the order they arrived on the branch into `docs/releases/X.Y.Z.md`, removes them, commits, tags
`vX.Y.Z` and pushes the commit and the tag together (`--atomic`): both land or neither does. When
someone else pushed or released first, it starts again from the new state, a few times at most.

A `docs/releases/X.Y.Z.md` for a version no tag has yet (written ahead the old way) is taken as the
first fragment of that release.

`lint` fails when the branch adds or changes `docs/releases/X.Y.Z.md` itself: such a file picks a
version before the release does, and two branches picking the same one is how releases collided.

Talks to `origin`; "can't reach it" is an error, never "there are no tags".
"""
import argparse
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

NOTES = "docs/releases"
FRAGMENTS = f"{NOTES}/unreleased"
VERSION_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
NOTES_RE = re.compile(r"^docs/releases/\d+\.\d+\.\d+\.md$")
ATTEMPTS = 3
NET_TIMEOUT = 60


class Refused(Exception):
    pass


class Raced(Exception):
    """The push lost to someone else's: start again from the new state."""


def git(*args, cwd, check=True, timeout=None):
    r = subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True, timeout=timeout,
                       env=dict(os.environ, GIT_TERMINAL_PROMPT="0"))
    if check and r.returncode != 0:
        raise Refused(f"git {' '.join(args)}: {(r.stderr or r.stdout).strip()[:400]}")
    return r


def remote_versions(repo):
    r = git("ls-remote", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
    if r.returncode != 0:
        raise Refused(f"can't read the tags on origin: {r.stderr.strip()[:300]}")
    out = set()
    for line in r.stdout.splitlines():
        ref = line.split("\t", 1)[-1].removeprefix("refs/tags/").removesuffix("^{}")
        if m := VERSION_RE.match(ref):
            out.add(tuple(int(x) for x in m.groups()))
    return out


def next_version(repo, minor=False):
    have = remote_versions(repo)
    if not have:
        return "0.1.0"
    a, b, c = max(have)
    return f"{a}.{b + 1}.0" if minor else f"{a}.{b}.{c + 1}"


def default_branch(repo):
    r = git("symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD", cwd=repo, check=False)
    if r.returncode == 0 and r.stdout.strip().startswith("origin/"):
        return r.stdout.strip().removeprefix("origin/")
    for b in ("main", "master"):
        if git("rev-parse", "--verify", "--quiet", f"refs/remotes/origin/{b}", cwd=repo, check=False).returncode == 0:
            return b
    raise Refused("origin has no default branch (origin/HEAD, main, master)")


def fragments(wt, version):
    """(path, text) in the order they arrived on the branch, then by name."""
    found = []
    ahead = Path(wt, NOTES, f"{version}.md")
    if ahead.is_file():
        found.append((f"{NOTES}/{version}.md", 0))
    d = Path(wt, FRAGMENTS)
    if d.is_dir():
        for f in sorted(d.iterdir()):
            if f.suffix != ".md" or f.name.startswith(".") or not f.is_file():
                continue
            rel = f"{FRAGMENTS}/{f.name}"
            t = git("log", "--diff-filter=A", "--format=%ct", "-1", "--", rel, cwd=wt).stdout.strip()
            found.append((rel, int(t) if t else sys.maxsize))
    found.sort(key=lambda x: (x[1], x[0]))
    return [(p, Path(wt, p).read_text()) for p, _ in found]


def body(text):
    """A fragment without a `# ` title and the blank lines around it."""
    lines = text.strip("\n").splitlines()
    if lines and lines[0].startswith("# "):
        lines = lines[1:]
    return "\n".join(lines).strip("\n")


def assemble(version, parts):
    out = f"# Steve v{version}\n"
    for _, text in parts:
        b = body(text)
        if b:
            out += f"\n{b}\n"
    return out


def cut_once(repo, branch, version_arg, minor, dry):
    if git("fetch", "--quiet", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT).returncode != 0:
        raise Refused("can't fetch origin")
    version = version_arg or next_version(repo, minor)
    if tuple(int(x) for x in version.split(".")) in remote_versions(repo):
        raise Refused(f"v{version} is already on origin")
    with tempfile.TemporaryDirectory(prefix="steve-cut-") as tmp:
        wt = Path(tmp, "wt")
        git("worktree", "add", "--quiet", "--detach", str(wt), f"refs/remotes/origin/{branch}", cwd=repo)
        try:
            parts = fragments(wt, version)
            if not parts:
                raise Refused(f"no release notes: add {FRAGMENTS}/<item>.md on {branch} first")
            notes = Path(wt, NOTES, f"{version}.md")
            text = assemble(version, parts)
            if dry:
                print(text, end="")
                return version
            for p, _ in parts:
                if p != f"{NOTES}/{version}.md":
                    git("rm", "--quiet", "--", p, cwd=wt)
            notes.write_text(text)
            git("add", "--", str(notes.relative_to(wt)), cwd=wt)
            git("commit", "--quiet", "-m", f"Release notes {version}", cwd=wt)
            git("tag", "-a", f"v{version}", "-m", f"Steve v{version}", cwd=wt)
            r = git("push", "--atomic", "--porcelain", "origin", f"HEAD:refs/heads/{branch}",
                    f"refs/tags/v{version}", cwd=wt, check=False, timeout=NET_TIMEOUT)
            if r.returncode != 0:
                git("tag", "-d", f"v{version}", cwd=repo, check=False)
                rejected = "[rejected]" in r.stdout or "[remote rejected]" in r.stdout
                if rejected:
                    raise Raced((r.stdout + r.stderr).strip()[:300])
                raise Refused(f"push failed: {(r.stderr or r.stdout).strip()[:300]}")
        finally:
            git("worktree", "remove", "--force", str(wt), cwd=repo, check=False)
    git("fetch", "--quiet", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
    return version


def cut(repo, branch, version_arg, minor, dry):
    for attempt in range(1, ATTEMPTS + 1):
        try:
            return cut_once(repo, branch, version_arg, minor, dry)
        except Raced as e:
            if version_arg or attempt == ATTEMPTS:
                raise Refused(f"origin moved while releasing: {e}")
            print(f"origin moved while releasing ({e}); again from the new state", file=sys.stderr)


def lint(repo, base):
    if not base:
        base = f"refs/remotes/origin/{default_branch(repo)}"
    mb = git("merge-base", "HEAD", base, cwd=repo).stdout.strip()
    changed = git("diff", "--name-only", "--no-renames", "--diff-filter=AM", mb, "HEAD", cwd=repo).stdout.split()
    bad = [f for f in changed if NOTES_RE.match(f)]
    if bad:
        raise Refused(f"{', '.join(bad)}: the version is chosen when releasing. "
                      f"Move the text to {FRAGMENTS}/<item>.md (no version, no title).")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", default=".", help="a checkout of the repository (default: here)")
    sub = ap.add_subparsers(dest="cmd", required=True)
    n = sub.add_parser("next")
    n.add_argument("--minor", action="store_true")
    c = sub.add_parser("cut")
    g = c.add_mutually_exclusive_group()
    g.add_argument("--minor", action="store_true")
    g.add_argument("--version")
    c.add_argument("--branch")
    c.add_argument("--dry-run", action="store_true")
    l = sub.add_parser("lint")
    l.add_argument("--base")
    a = ap.parse_args()
    repo = Path(a.repo).resolve()
    try:
        if a.cmd == "next":
            print(next_version(repo, a.minor))
        elif a.cmd == "cut":
            if a.version and not re.fullmatch(r"\d+\.\d+\.\d+", a.version):
                raise Refused(f"--version {a.version}: X.Y.Z")
            v = cut(repo, a.branch or default_branch(repo), a.version, a.minor, a.dry_run)
            if not a.dry_run:
                print(f"released v{v}")
        else:
            lint(repo, a.base)
    except Refused as e:
        print(f"✗ {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
