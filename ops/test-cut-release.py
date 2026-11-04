#!/usr/bin/env python3
"""cut-release.py on throwaway repositories: no network, no real remote."""
import os
import subprocess
import sys
import tempfile
from pathlib import Path

CUT = Path(__file__).resolve().parent / "cut-release.py"
ENV = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
           GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com")


def git(cwd, *args):
    return subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True, text=True,
                          env=ENV).stdout.strip()


def cut(cwd, *args):
    return subprocess.run([sys.executable, str(CUT), "--repo", str(cwd), *args], capture_output=True,
                          text=True, env=ENV)


def commit(repo, files, msg):
    for f, text in files.items():
        p = Path(repo, f)
        p.parent.mkdir(parents=True, exist_ok=True)
        if text is None:
            p.unlink()
        else:
            p.write_text(text)
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", msg)


def setup(tmp):
    origin = Path(tmp, "origin.git")
    seed = Path(tmp, "seed")
    seed.mkdir()
    git(seed, "init", "-q", "-b", "main")
    commit(seed, {"README.md": "x\n", "docs/releases/0.2.24.md": "# Steve v0.2.24\n\nold\n"}, "base")
    git(seed, "tag", "-a", "v0.2.24", "-m", "Steve v0.2.24")
    git(tmp, "clone", "-q", "--bare", str(seed), str(origin))
    a = Path(tmp, "a")
    git(tmp, "clone", "-q", str(origin), str(a))
    return origin, a


def ok(name):
    print(f"✓ {name}")


def main():
    with tempfile.TemporaryDirectory(prefix="steve-cut-test-") as tmp:
        origin, a = setup(tmp)

        r = cut(a, "next")
        assert r.returncode == 0 and r.stdout.strip() == "0.2.25", r
        assert cut(a, "next", "--minor").stdout.strip() == "0.3.0"
        ok("the next version is one more than the highest tag on origin")

        r = cut(a, "cut")
        assert r.returncode == 1 and "no release notes" in r.stderr, r
        ok("no fragments, no release")

        # two changes arrive with their fragments, the second one with a title it shouldn't have
        commit(a, {"src/x.txt": "x\n", "docs/releases/unreleased/ST-1.md": "- First change. (ST-1)\n"}, "one")
        commit(a, {"src/y.txt": "y\n", "docs/releases/unreleased/ST-2.md": "# Steve v9.9.9\n\n- Second change. (ST-2)\n"}, "two")
        git(a, "push", "-q", "origin", "main")

        r = cut(a, "cut", "--dry-run")
        assert r.returncode == 0 and r.stdout == "# Steve v0.2.25\n\n- First change. (ST-1)\n\n- Second change. (ST-2)\n", r
        assert git(origin, "tag", "-l", "v0.2.25") == ""
        ok("--dry-run shows the notes and changes nothing")

        # the caller's own checkout is left alone: an unrelated dirty file and another branch
        git(a, "checkout", "-q", "-b", "elsewhere")
        Path(a, "scratch.txt").write_text("mine\n")
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.2.25" in r.stdout, r
        assert git(a, "rev-parse", "--abbrev-ref", "HEAD") == "elsewhere" and Path(a, "scratch.txt").exists()
        assert git(origin, "rev-parse", "v0.2.25^{commit}") == git(origin, "rev-parse", "main")
        assert git(origin, "cat-file", "-t", "v0.2.25") == "tag"
        notes = git(origin, "show", "main:docs/releases/0.2.25.md")
        assert notes == "# Steve v0.2.25\n\n- First change. (ST-1)\n\n- Second change. (ST-2)", notes
        assert git(origin, "ls-tree", "--name-only", "main", "docs/releases/unreleased/") == ""
        ok("cut tags the branch as it is on origin, joins and removes the fragments, in one push")

        # a second clone that hasn't seen the release yet still gets the next free number
        b = Path(tmp, "b")
        git(tmp, "clone", "-q", str(origin), str(b))
        commit(b, {"docs/releases/unreleased/ST-3.md": "- Third. (ST-3)\n"}, "three")
        git(b, "push", "-q", "origin", "main")
        git(a, "checkout", "-q", "main")
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.2.26" in r.stdout, r
        ok("a stale checkout releases from origin's state, not its own")

        r = cut(a, "cut", "--version", "0.2.26")
        assert r.returncode == 1 and "already on origin" in r.stderr, r
        ok("a version that is taken is refused")

        # someone else pushes between our fetch and our push: the hook rejects once, then lets it in
        hook = Path(origin, "hooks", "pre-receive")
        marker = Path(tmp, "rejected-once")
        hook.write_text(f"#!/bin/sh\nif [ ! -f '{marker}' ]; then touch '{marker}'; echo busy >&2; exit 1; fi\nexit 0\n")
        hook.chmod(0o755)
        commit(b, {"docs/releases/unreleased/ST-4.md": "- Fourth. (ST-4)\n"}, "four")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        marker.touch()
        git(b, "push", "-q", "origin", "main")
        marker.unlink()
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.2.27" in r.stdout and "again from the new state" in r.stderr, r
        assert git(origin, "show", "main:docs/releases/0.2.27.md") == "# Steve v0.2.27\n\n- Fourth. (ST-4)"
        assert git(a, "tag", "-l", "v0.2.27") == "v0.2.27"
        ok("a rejected push starts again and lands once")
        hook.unlink()

        # notes written ahead the old way are the first fragment of their version
        commit(b, {"docs/releases/0.2.28.md": "# Steve v0.2.28\n\nAhead.\n"}, "ahead")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        commit(b, {"docs/releases/unreleased/ST-5.md": "- Fifth. (ST-5)\n"}, "five")
        git(b, "push", "-q", "origin", "main")
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.2.28" in r.stdout, r
        assert git(origin, "show", "main:docs/releases/0.2.28.md") == "# Steve v0.2.28\n\nAhead.\n\n- Fifth. (ST-5)"
        ok("notes for a version not yet tagged are taken in, not overwritten")

        # lint: a branch may carry fragments, not versioned notes
        git(a, "fetch", "-q", "origin")
        git(a, "checkout", "-q", "-b", "feat", "origin/main")
        commit(a, {"docs/releases/unreleased/ST-6.md": "- Six.\n"}, "fragment")
        assert cut(a, "lint").returncode == 0
        commit(a, {"docs/releases/0.2.29.md": "# Steve v0.2.29\n"}, "versioned")
        r = cut(a, "lint")
        assert r.returncode == 1 and "docs/releases/0.2.29.md" in r.stderr, r
        ok("lint refuses a branch that picks its own version")

        # origin unreachable: an error, not "no tags"
        git(a, "remote", "set-url", "origin", str(Path(tmp, "gone.git")))
        r = cut(a, "next")
        assert r.returncode == 1 and "can't read the tags" in r.stderr, r
        ok("an unreachable origin is an error, never version 0.1.0")
    print("All cut-release checks passed")


if __name__ == "__main__":
    main()
