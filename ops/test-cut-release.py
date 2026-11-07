#!/usr/bin/env python3
"""cut-release.py on throwaway repositories: no network, no real remote, a fake `gh` for CI."""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

CUT = Path(__file__).resolve().parent / "cut-release.py"
ENV = dict({k: v for k, v in os.environ.items() if k != "STEVE_GH"}, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
           GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com")

# GitHub's answer for the CI workflow's runs on a commit, from $FAKE_GH_CI: {"conclusion": ..., "status": ...}
# for the push run of every commit, or {"none": true} (only a pull request's run); each call is logged to
# $FAKE_GH_LOG.
FAKE_GH = r"""#!/usr/bin/env python3
import json, os, re, sys
open(os.environ["FAKE_GH_LOG"], "a").write(" ".join(sys.argv[1:]) + "\n")
ci = json.load(open(os.environ["FAKE_GH_CI"]))
m = re.search(r"workflows/ci\.yml/runs\?head_sha=([0-9a-f]{40})&event=push&branch=main&", " ".join(sys.argv[1:]))
if sys.argv[1:2] != ["api"] or not m:
    sys.exit("fake gh: unexpected " + " ".join(sys.argv[1:]))
if ci.get("down"):
    sys.exit("HTTP 502")
# a green pull-request run of the same commit (Linux only) is always there, and newer: it never counts
pr = {"id": 99, "run_number": 3, "head_sha": m.group(1), "event": "pull_request", "head_branch": "feature",
      "status": "completed", "conclusion": "success", "html_url": "https://github.example/runs/99"}
runs = [pr] if ci.get("none") else [
    {"id": 7, "run_number": 1, "head_sha": m.group(1), "event": "push", "head_branch": "main",
     "status": "completed", "conclusion": "failure", "html_url": "https://github.example/runs/7"},
    {"id": 42, "run_number": 2, "head_sha": m.group(1), "event": "push", "head_branch": "main",
     "status": ci.get("status", "completed"), "conclusion": ci.get("conclusion"),
     "html_url": "https://github.example/runs/42"}, pr]
print(json.dumps({"total_count": len(runs), "workflow_runs": runs}))
"""


def fake_gh(tmp):
    bin_ = Path(tmp, "bin")
    bin_.mkdir()
    gh = bin_ / "gh"
    gh.write_text(FAKE_GH.replace("#!/usr/bin/env python3", f"#!{sys.executable}", 1))
    gh.chmod(0o755)
    ENV.update(PATH=f"{bin_}{os.pathsep}{ENV['PATH']}", FAKE_GH_CI=str(Path(tmp, "ci.json")),
               FAKE_GH_LOG=str(Path(tmp, "gh.log")))
    set_ci(conclusion="success")


def set_ci(**state):
    Path(ENV["FAKE_GH_CI"]).write_text(json.dumps(state))


def gh_calls():
    p = Path(ENV["FAKE_GH_LOG"])
    return p.read_text().splitlines() if p.exists() else []


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
        fake_gh(tmp)
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
        assert gh_calls() == [], gh_calls()
        ok("--dry-run shows the notes and changes nothing, without asking CI")

        head = git(origin, "rev-parse", "main")
        set_ci(conclusion="failure")
        r = cut(a, "cut", "--ci-wait", "0")
        assert r.returncode == 1 and f"failure on {head[:7]}" in r.stderr, r
        assert "gh run rerun 42 --failed" in r.stderr and "runs/42" in r.stderr, r.stderr
        assert git(origin, "tag", "-l", "v0.2.25") == "" and git(origin, "rev-parse", "main") == head
        assert any(f"head_sha={head}" in c for c in gh_calls()), gh_calls()
        ok("a red CI run on the head refuses the release and says how to rerun its failed jobs")

        set_ci(conclusion=None, status="in_progress")
        r = cut(a, "cut", "--ci-wait", "0")
        assert r.returncode == 1 and "still in_progress" in r.stderr and "runs/42" in r.stderr, r
        set_ci(none=True)
        r = cut(a, "cut", "--ci-wait", "0")
        assert r.returncode == 1 and "has no push run on" in r.stderr, r
        assert git(origin, "tag", "-l", "v0.2.25") == ""
        set_ci(down=True)
        r = cut(a, "cut", "--ci-wait", "0")
        assert r.returncode == 1 and "can't read CI" in r.stderr, r
        assert git(origin, "tag", "-l", "v0.2.25") == ""
        ok("CI still running, no push run (a green pull-request run does not count), or GitHub unreadable: no release")

        only_git = Path(tmp, "only-git")
        only_git.mkdir()
        (only_git / "git").symlink_to(shutil.which("git"))
        no_gh = dict(ENV, PATH=f"{only_git}{os.pathsep}/usr/bin{os.pathsep}/bin")
        fake = str(Path(tmp, "bin", "gh"))
        # launchd's PATH has no gh: $STEVE_GH (what the Hub uses) still finds it
        set_ci(conclusion="failure")
        r = subprocess.run([sys.executable, str(CUT), "--repo", str(a), "cut", "--ci-wait", "0"], capture_output=True,
                           text=True, env=dict(no_gh, STEVE_GH=fake))
        assert r.returncode == 1 and f"failure on {head[:7]}" in r.stderr, r
        ok("no gh on PATH: $STEVE_GH is used")
        places = ("/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh", "/bin/gh")
        if not any(Path(p).exists() for p in places):
            r = subprocess.run([sys.executable, str(CUT), "--repo", str(a), "cut"], capture_output=True, text=True,
                               env=no_gh)
            assert r.returncode == 1 and "no `gh` on PATH" in r.stderr and "--skip-ci" in r.stderr, r
            ok("no gh: refused, naming the override")
        set_ci(conclusion="success")

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
        assert git(origin, "rev-parse", "v0.2.25^{commit}^") == head
        assert "CI passed on" in r.stderr and "gh run rerun <run id> --failed" in r.stderr, r.stderr
        ok("cut tags the branch as it is on origin once its CI passed, joins and removes the fragments, in one push")

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

        commit(b, {"docs/releases/unreleased/ST-3b.md": "- Untested. (ST-3b)\n"}, "untested")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        git(b, "push", "-q", "origin", "main")
        set_ci(conclusion="failure")
        calls = len(gh_calls())
        r = cut(a, "cut", "--skip-ci")
        assert r.returncode == 0 and "released v0.2.27" in r.stdout and "--skip-ci" in r.stderr, r
        assert len(gh_calls()) == calls, "--skip-ci asks nothing"
        set_ci(conclusion="success")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        ok("--skip-ci releases without the CI check, and says so")

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
        assert r.returncode == 0 and "released v0.2.28" in r.stdout and "again from the new state" in r.stderr, r
        assert git(origin, "show", "main:docs/releases/0.2.28.md") == "# Steve v0.2.28\n\n- Fourth. (ST-4)"
        assert git(a, "tag", "-l", "v0.2.28") == "v0.2.28"
        ok("a rejected push starts again and lands once")
        hook.unlink()

        # notes written ahead the old way are the first fragment of their version
        commit(b, {"docs/releases/0.2.29.md": "# Steve v0.2.29\n\nAhead.\n"}, "ahead")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        commit(b, {"docs/releases/unreleased/ST-5.md": "- Fifth. (ST-5)\n"}, "five")
        git(b, "push", "-q", "origin", "main")
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.2.29" in r.stdout, r
        assert git(origin, "show", "main:docs/releases/0.2.29.md") == "# Steve v0.2.29\n\nAhead.\n\n- Fifth. (ST-5)"
        ok("notes for a version not yet tagged are taken in, not overwritten")

        # lint: a branch may carry fragments, not versioned notes
        git(a, "fetch", "-q", "origin")
        git(a, "checkout", "-q", "-b", "feat", "origin/main")
        commit(a, {"docs/releases/unreleased/ST-6.md": "- Six.\n"}, "fragment")
        assert cut(a, "lint").returncode == 0
        commit(a, {"docs/releases/0.2.30.md": "# Steve v0.2.30\n"}, "versioned")
        r = cut(a, "lint")
        assert r.returncode == 1 and "docs/releases/0.2.30.md" in r.stderr, r
        ok("lint refuses a branch that picks its own version")

        # in process: the version is taken between choosing it and checking it, and a push that fails
        spec = importlib.util.spec_from_file_location("cut_release", CUT)
        m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(m)
        commit(b, {"docs/releases/unreleased/ST-7.md": "- Seventh.\n"}, "seven")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        git(b, "push", "-q", "origin", "main")
        real, calls = m.remote_versions, []

        def taken_once(repo):
            have = real(repo)
            calls.append(1)
            if len(calls) == 2:  # the check right after choosing: someone else took it
                have = have | {(0, 2, 30)}
            return have
        m.remote_versions = taken_once
        os.environ.update({k: v for k, v in ENV.items() if k.startswith(("GIT_", "FAKE_GH_")) or k == "PATH"})
        assert m.cut(b, "main", None, False, False) == "0.2.30"
        assert git(origin, "tag", "-l", "v0.2.30") == "v0.2.30" and len(calls) >= 3
        m.remote_versions = real
        ok("a version taken meanwhile means the next one, not a refusal")

        # CI still runs on the head: cut waits for it, then releases
        commit(b, {"docs/releases/unreleased/ST-7b.md": "- Waited for.\n"}, "waited")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        git(b, "push", "-q", "origin", "main")
        real_run, seen = m.ci_run, []

        def finishes(repo, sha, branch):
            seen.append(sha)
            states = [None, {"id": 9, "status": "queued"}, {"id": 9, "status": "in_progress"},
                      {"id": 9, "status": "completed", "conclusion": "success"}]
            return states[min(len(seen) - 1, 3)]
        m.ci_run, m.CI_POLL = finishes, 0
        assert m.cut(b, "main", None, False, False) == "0.2.31"
        assert len(seen) == 4 and set(seen) == {git(origin, "rev-parse", "v0.2.31^{commit}^")}, seen
        seen.clear()

        def never(repo, sha, branch):
            seen.append(sha)
            return {"id": 9, "status": "in_progress", "html_url": "https://github.example/runs/9"}
        m.ci_run = never
        commit(b, {"docs/releases/unreleased/ST-7c.md": "- Never.\n"}, "never")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        git(b, "push", "-q", "origin", "main")
        try:
            m.cut(b, "main", None, False, False, ci_wait=0)
            raise AssertionError("released while CI runs")
        except m.Refused as e:
            assert "still in_progress" in str(e) and "runs/9" in str(e), e
        assert git(origin, "tag", "-l", "v0.2.32") == ""
        m.ci_run, m.CI_POLL = real_run, 30
        ok("CI queued or running on the head: cut waits for it, and gives up after --ci-wait")

        # launchd's PATH and no $STEVE_GH: gh in its usual install place (Homebrew's) is found
        saved = os.environ.get("PATH"), m.GH_PLACES
        os.environ["PATH"] = str(only_git)
        m.GH_PLACES = (str(Path(tmp, "nowhere", "gh")), str(Path(tmp, "bin", "gh")))
        try:
            assert m.gh_path() == str(Path(tmp, "bin", "gh")), m.gh_path()
            m.GH_PLACES = (str(Path(tmp, "nowhere", "gh")),)
            assert m.gh_path() is None
        finally:
            os.environ["PATH"], m.GH_PLACES = saved
        ok("no gh on PATH nor $STEVE_GH: the usual install places are looked in")

        commit(b, {"docs/releases/unreleased/ST-8.md": "- Eighth.\n"}, "eight")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        git(b, "push", "-q", "origin", "main")

        def lost(*a):
            raise m.Refused("pushing timed out")
        m.push = lost
        try:
            m.cut(b, "main", None, False, False)
            raise AssertionError("released without a push")
        except m.Refused:
            pass
        assert git(b, "tag", "-l", "v0.2.32") == "" and git(origin, "tag", "-l", "v0.2.32") == ""
        assert len(git(b, "worktree", "list").splitlines()) == 1, git(b, "worktree", "list")
        ok("a push that did not land leaves no tag and no worktree behind")

        # origin unreachable: an error, not "no tags"
        git(a, "remote", "set-url", "origin", str(Path(tmp, "gone.git")))
        r = cut(a, "next")
        assert r.returncode == 1 and "can't read the tags" in r.stderr, r
        ok("an unreachable origin is an error, never version 0.1.0")
    print("All cut-release checks passed")


if __name__ == "__main__":
    main()
