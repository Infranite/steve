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
# for the push run of every commit, or {"none": true} (only a pull request's run). `commits` overrides
# these answers per SHA (including `older_conclusion` for a superseded run); each call is logged to
# $FAKE_GH_LOG. With {"pull": {"number": N, "head": sha, ...}} every commit asked about is that pull request
# as merged ("merged_as": another commit instead; "base": its target), and its head has one pull-request run
# ("conclusion", "status").
# `gh pr create|merge|view|close` act on $FAKE_GH_ORIGIN (a bare repository) and keep the pull requests in
# $FAKE_GH_PRS. A merge is a merge commit: the base's head and the pull request's head as its two parents, or
# "not mergeable" when they conflict. A release pull request's head has one pull-request run:
# {"release_pr": {"conclusion": ..., "status": ...}} (default success); {"move_main": true} pushes a commit onto
# main the first time that run is asked about ({"move_main": {path: text}}: one that writes those files);
# {"merge_refused": "..."} makes the merge fail with that message; {"base_modified": true} makes the first
# merge fail the way GitHub does when the base moved at that very moment.
FAKE_GH = r"""#!/usr/bin/env python3
import json, os, re, subprocess, sys
args = " ".join(sys.argv[1:])
open(os.environ["FAKE_GH_LOG"], "a").write(args + "\n")
ci = json.load(open(os.environ["FAKE_GH_CI"]))
pull = ci.get("pull")
origin, prs_file = os.environ["FAKE_GH_ORIGIN"], os.environ["FAKE_GH_PRS"]
prs = json.load(open(prs_file)) if os.path.exists(prs_file) else {}
def save():
    json.dump(prs, open(prs_file, "w"))
def og(*a, env=None):
    return subprocess.run(["git", "-C", origin, *a], check=True, capture_output=True, text=True,
                          env=dict(os.environ, **(env or {}))).stdout.strip()
def opt(name):
    return sys.argv[sys.argv.index(name) + 1]
if sys.argv[1:2] == ["pr"]:
    verb, rest = sys.argv[2], sys.argv[3:]
    if verb == "create":
        n = str(100 + len(prs))
        prs[n] = {"base": opt("--base"), "head": opt("--head"), "sha": og("rev-parse", "refs/heads/" + opt("--head")),
                  "title": opt("--title"), "body": opt("--body"), "state": "OPEN", "merged": None}
        save()
        print("https://github.example/acme/app/pull/" + n)
    elif verb == "merge":
        pr = prs[rest[0]]
        assert "--merge" in rest and opt("--match-head-commit") == pr["sha"], args
        if ci.get("merge_refused"):
            sys.exit(ci["merge_refused"])
        if ci.get("base_modified") and not os.path.exists(prs_file + ".modified"):
            open(prs_file + ".modified", "w").close()
            sys.exit("GraphQL: Base branch was modified. Review and try the merge again. (mergePullRequest)")
        base = og("rev-parse", "refs/heads/" + pr["base"])
        trial = subprocess.run(["git", "-C", origin, "merge-tree", "--write-tree", base, pr["sha"]],
                               capture_output=True, text=True)
        if trial.returncode != 0:
            sys.exit("X Pull request #" + rest[0] + " is not mergeable: the merge commit cannot be cleanly created.")
        tree = trial.stdout.splitlines()[0]
        msg = "Merge pull request #" + rest[0] + " from acme/" + pr["head"]
        new = og("commit-tree", tree, "-p", base, "-p", pr["sha"], "-m", msg, env={"GIT_COMMITTER_NAME": "merger"})
        og("update-ref", "refs/heads/" + pr["base"], new, base)
        pr.update(state="MERGED", merged=new)
        save()
    elif verb == "view":
        pr = prs[rest[0]]
        print(json.dumps({"state": pr["state"], "mergeCommit": {"oid": pr["merged"]} if pr["merged"] else None}))
    elif verb == "close":
        prs[rest[0]]["state"] = "CLOSED"
        save()
    else:
        sys.exit("fake gh: unexpected " + args)
    sys.exit(0)
m = re.search(r"workflows/ci\.yml/runs\?head_sha=([0-9a-f]{40})&event=push&branch=main&", args)
merged = re.search(r"/commits/([0-9a-f]{40})/pulls\?", args)
tested = re.search(r"workflows/ci\.yml/runs\?head_sha=([0-9a-f]{40})&event=pull_request&", args)
if sys.argv[1:2] != ["api"] or not (m or merged or tested):
    sys.exit("fake gh: unexpected " + args)
if ci.get("down"):
    sys.exit("HTTP 502")
if merged:
    print(json.dumps([{"number": pull["number"], "merged_at": "2026-01-01T00:00:00Z",
                       "merge_commit_sha": pull.get("merged_as", merged.group(1)),
                       "head": {"sha": pull["head"]}, "base": {"ref": pull.get("base", "main")}}] if pull else []))
    sys.exit(0)
if tested and any(p["sha"] == tested.group(1) for p in prs.values()):
    if ci.get("move_main") and not os.path.exists(prs_file + ".moved"):
        open(prs_file + ".moved", "w").close()
        main = og("rev-parse", "refs/heads/main")
        index = prs_file + ".index"
        og("read-tree", main, env={"GIT_INDEX_FILE": index})
        for path, text in (ci["move_main"] if isinstance(ci["move_main"], dict) else {}).items():
            blob = subprocess.run(["git", "-C", origin, "hash-object", "-w", "--stdin"], input=text, check=True,
                                  capture_output=True, text=True).stdout.strip()
            og("update-index", "--add", "--cacheinfo", "100644," + blob + "," + path, env={"GIT_INDEX_FILE": index})
        tree = og("write-tree", env={"GIT_INDEX_FILE": index})
        og("update-ref", "refs/heads/main", og("commit-tree", tree, "-p", main, "-m", "meanwhile"), main)
    rel = ci.get("release_pr", {})
    runs = [{"id": 88, "run_number": 9, "head_sha": tested.group(1), "event": "pull_request",
             "head_branch": "release", "status": rel.get("status", "completed"),
             "conclusion": rel.get("conclusion", "success"), "html_url": "https://github.example/runs/88"}]
    print(json.dumps({"total_count": 1, "workflow_runs": runs}))
    sys.exit(0)
if tested:
    runs = [{"id": 77, "run_number": 5, "head_sha": pull["head"], "event": "pull_request", "head_branch": "pr",
             "status": pull.get("status", "completed"), "conclusion": pull.get("conclusion", "success"),
             "html_url": "https://github.example/runs/77"}] if pull and pull["head"] == tested.group(1) else []
    print(json.dumps({"total_count": len(runs), "workflow_runs": runs}))
    sys.exit(0)
# a green pull-request run of the same commit (Linux only) is always there, and newer: by itself it never counts
state = ci.get("commits", {}).get(m.group(1), ci)
pr = {"id": 99, "run_number": 3, "head_sha": m.group(1), "event": "pull_request", "head_branch": "feature",
      "status": "completed", "conclusion": "success", "html_url": "https://github.example/runs/99"}
runs = [pr] if state.get("none") else [
    {"id": 7, "run_number": 1, "head_sha": m.group(1), "event": "push", "head_branch": "main",
     "status": "completed", "conclusion": state.get("older_conclusion", "failure"), "html_url": "https://github.example/runs/7"},
    {"id": 42, "run_number": 2, "head_sha": m.group(1), "event": "push", "head_branch": "main",
     "status": state.get("status", "completed"), "conclusion": state.get("conclusion"),
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
               FAKE_GH_LOG=str(Path(tmp, "gh.log")), FAKE_GH_ORIGIN=str(Path(tmp, "origin.git")),
               FAKE_GH_PRS=str(Path(tmp, "prs.json")))
    set_ci(conclusion="success")


def set_ci(**state):
    Path(ENV["FAKE_GH_CI"]).write_text(json.dumps(state))
    for once in (".moved", ".modified"):  # move_main and base_modified happen once after each set_ci
        Path(ENV["FAKE_GH_PRS"] + once).unlink(missing_ok=True)


def prs():
    p = Path(ENV["FAKE_GH_PRS"])
    return json.loads(p.read_text()) if p.exists() else {}


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

        # Even a success value cannot release while other parallel jobs are still running.
        set_ci(conclusion="success", status="in_progress")
        r = cut(a, "cut", "--ci-wait", "0")
        assert r.returncode == 1 and "still in_progress" in r.stderr and "runs/42" in r.stderr, r
        for conclusion in ("cancelled", "skipped", "neutral"):
            set_ci(conclusion=conclusion)
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 1 and f"{conclusion} on" in r.stderr, r
            assert git(origin, "tag", "-l", "v0.2.25") == ""
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
        pr = prs()["100"]
        assert git(origin, "rev-parse", "v0.2.25^{commit}") == pr["sha"] == git(origin, "rev-parse", "main^2")
        assert git(origin, "rev-parse", "main^1") == head
        assert git(origin, "cat-file", "-t", "v0.2.25") == "tag"
        notes = git(origin, "show", "main:docs/releases/0.2.25.md")
        assert notes == "# Steve v0.2.25\n\n- First change. (ST-1)\n\n- Second change. (ST-2)", notes
        assert git(origin, "ls-tree", "--name-only", "main", "docs/releases/unreleased/") == ""
        assert git(origin, "rev-parse", "v0.2.25^{commit}^") == head
        assert "CI passed on" in r.stderr and "gh run rerun <run id> --failed" in r.stderr, r.stderr
        assert pr["title"] == "Release notes 0.2.25" and pr["head"] == "release/v0.2.25" and pr["base"] == "main", pr
        assert pr["state"] == "MERGED" and pr["merged"] == git(origin, "rev-parse", "main"), pr
        assert "- First change. (ST-1)" in pr["body"] and "release pull request #100" in r.stderr, (pr, r.stderr)
        assert git(origin, "branch", "--list", "release/*") == "", "the release branch is deleted"
        assert any("pull_request" in c and pr["sha"] in c for c in gh_calls()), "it waited for the release PR's CI"
        ok("cut releases the branch as it is on origin once its CI passed: the notes through a pull request "
           "merged with a merge commit, the tag on the notes commit its run tested")

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
        assert not [c for c in gh_calls()[calls:] if "actions/workflows" in c], "--skip-ci asks no CI"
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

        real_push = m.push

        def lost(wt, *refspecs):
            if refspecs[-1].startswith("refs/tags/"):
                raise m.Refused("pushing timed out")
            return real_push(wt, *refspecs)
        m.push = lost
        try:
            m.cut(b, "main", None, False, False)
            raise AssertionError("released without a push")
        except m.Refused:
            pass
        m.push = real_push
        assert git(b, "tag", "-l", "v0.2.32") == "" and git(origin, "tag", "-l", "v0.2.32") == ""
        assert len(git(b, "worktree", "list").splitlines()) == 1, git(b, "worktree", "list")
        assert "- Eighth." in git(origin, "show", "main:docs/releases/0.2.32.md")
        ok("a tag push that did not land leaves no tag and no worktree behind")
        notes_commit = git(origin, "rev-parse", "main^2")
        assert git(origin, "log", "-1", "--format=%s", notes_commit) == "Release notes 0.2.32"
        # main moves on before the next cut, with a fragment of its own
        git(b, "checkout", "-q", "-B", "main", "origin/main")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        commit(b, {"src/later.txt": "l\n", "docs/releases/unreleased/ST-8b.md": "- Later.\n"}, "later")
        git(b, "push", "-q", "origin", "main")
        r = cut(a, "cut", "--dry-run")
        assert r.returncode == 0 and "- Eighth." in r.stdout and "Later" not in r.stdout, r
        assert "without its tag" in r.stderr, r.stderr
        opened = len(prs())
        assert m.cut(b, "main", None, False, False) == "0.2.32"
        assert len(prs()) == opened, "no second pull request"
        assert git(origin, "rev-parse", "v0.2.32^{commit}") == notes_commit
        assert git(origin, "show", "main:docs/releases/unreleased/ST-8b.md") == "- Later."
        ok("the next cut tags notes that merged without their tag, as they are, even with main moved on; "
           "what arrived since waits for the next release")

        # a pull request, rebase-merged: GitHub replays its commits onto the branch, under new ids
        def pull_request(number, files):
            git(b, "fetch", "-q", "origin")
            git(b, "checkout", "-q", "-B", "pr", "origin/main")
            for n, (f, text) in enumerate(files.items()):
                commit(b, {f: text}, f"pr {number}.{n}")
            git(b, "push", "-q", "origin", f"HEAD:refs/pull/{number}/head")
            return git(b, "rev-parse", "HEAD")

        def rebase_merge(count):
            git(b, "checkout", "-q", "-B", "main", "origin/main")
            subprocess.run(["git", "-C", str(b), "cherry-pick", f"pr~{count}..pr"], check=True, capture_output=True,
                           env=dict(ENV, GIT_COMMITTER_NAME="merger"))
            git(b, "push", "-q", "origin", "main")
            return git(b, "rev-parse", "HEAD")

        def refused(why, **state):
            set_ci(**state)
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 1 and why in r.stderr, r
            assert git(origin, "tag", "-l", "v0.2.33") == ""

        # the branch moved on before the merge: the pull request's run tested less than what is released
        behind = pull_request(7, {"src/p7.txt": "7\n", "docs/releases/unreleased/ST-9.md": "- Ninth.\n"})
        git(b, "checkout", "-q", "-B", "main", "origin/main")
        commit(b, {"src/meanwhile.txt": "m\n"}, "meanwhile")
        git(b, "push", "-q", "origin", "main")
        assert rebase_merge(2) != behind
        refused("has no push run on", none=True, pull={"number": 7, "head": behind})
        ok("a pull request merged onto a branch that had moved: its run does not count")

        head = pull_request(8, {"src/p8.txt": "8\n", "src/p8b.txt": "8b\n"})
        merged = rebase_merge(2)
        assert merged != head and git(b, "rev-parse", f"{merged}^{{tree}}") == git(b, "rev-parse", f"{head}^{{tree}}")
        pull = {"number": 8, "head": head}
        refused("has no push run on", none=True, pull=dict(pull, conclusion="failure"))
        refused("has no push run on", none=True, pull=dict(pull, status="in_progress", conclusion=None))
        refused("has no push run on", none=True, pull=dict(pull, merged_as=head))
        refused("has no push run on", none=True, pull=dict(pull, base="other"))
        refused(f"failure on {merged[:7]}", conclusion="failure", pull=pull)
        ok("a pull request's run that did not pass, another merge or target, or a red run on the head: no release")
        refused("still in_progress", status="in_progress", conclusion=None, pull=pull)
        ok("an up-to-date pull request's green run does not stand for the head's own push run")
        set_ci(conclusion="success", pull=pull)
        r = cut(a, "cut", "--ci-wait", "0")
        assert r.returncode == 0 and "released v0.2.33" in r.stdout, r
        assert f"CI passed on {merged[:7]}" in r.stderr, r.stderr
        assert git(origin, "rev-parse", "v0.2.33^{commit}^") == merged
        ok("once the head's own push run passed, it releases")

        def fragment(name):
            subprocess.run(["git", "-C", str(b), "checkout", "-q", "-B", "main", "origin/main"], check=True, env=ENV)
            subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
            commit(b, {f"docs/releases/unreleased/{name}.md": f"- {name}.\n"}, name)
            git(b, "push", "-q", "origin", "main")
            return git(origin, "rev-parse", "main")

        def closed_clean(before, head, version):
            new = list(prs().values())[before:]
            assert new and all(p["state"] == "CLOSED" for p in new), new
            assert git(origin, "branch", "--list", "release/*") == ""
            assert git(origin, "rev-parse", "main") == head and git(origin, "tag", "-l", f"v{version}") == ""

        head, before = fragment("ST-10"), len(prs())
        set_ci(conclusion="success", release_pr={"conclusion": "failure"})
        r = cut(a, "cut", "--ci-wait", "0")
        assert r.returncode == 1 and "failure on release pull request #" in r.stderr and "runs/88" in r.stderr, r
        closed_clean(before, head, "0.2.34")
        ok("a red run on the release pull request: closed, its branch deleted, nothing released")

        before = len(prs())
        set_ci(conclusion="success", merge_refused='Required status check "CI gate" is expected')
        r = cut(a, "cut", "--skip-ci")
        assert r.returncode == 1 and "merging release pull request #" in r.stderr and "CI gate" in r.stderr, r
        closed_clean(before, head, "0.2.34")
        ok("a merge the branch rules refuse: closed, its branch deleted, nothing released")

        git(b, "push", "-q", "origin", "HEAD:refs/heads/release/v0.2.34")
        set_ci(conclusion="success")
        r = cut(a, "cut")
        assert r.returncode == 1 and "release/v0.2.34 is already on origin" in r.stderr, r
        assert git(origin, "tag", "-l", "v0.2.34") == ""
        git(b, "push", "-q", "origin", "--delete", "release/v0.2.34")
        ok("a release branch already on origin: refused, naming it")

        before = len(prs())
        # main moves between the CI check on its head and the merge, with a change and a fragment of its own
        late = "docs/releases/unreleased/ST-late.md"
        set_ci(conclusion="success", move_main={"src/meanwhile.txt": "moved\n", late: "- Late.\n"})
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.2.34" in r.stdout and "again from the new state" not in r.stderr, r
        new = list(prs().values())[before:]
        assert [p["state"] for p in new] == ["MERGED"], new
        tagged = git(origin, "rev-parse", "v0.2.34^{commit}")
        # the tag is the very notes commit whose run passed, on the head whose push run passed
        assert tagged == new[0]["sha"] and git(origin, "rev-parse", "v0.2.34^{commit}^") == head
        assert any("pull_request" in c and tagged in c for c in gh_calls())
        assert git(origin, "log", "--format=%s", "-1", "main^1") == "meanwhile"
        assert git(origin, "rev-parse", "main") == new[0]["merged"] and git(origin, "rev-parse", "main^2") == tagged
        assert subprocess.run(["git", "-C", str(origin), "merge-base", "--is-ancestor", tagged, "main"]).returncode == 0
        changed = git(origin, "diff", "--name-status", head, tagged)
        assert changed == "A\tdocs/releases/0.2.34.md\nD\tdocs/releases/unreleased/ST-10.md", changed
        assert "Late" not in git(origin, "show", "v0.2.34:docs/releases/0.2.34.md")
        assert git(origin, "show", "v0.2.34:src/meanwhile.txt") == "m", "the tag has the tested tree, not main's"
        # the merge keeps main's own changes: its fragment for the next release, and its code
        assert git(origin, "show", f"main:{late}") == "- Late."
        assert git(origin, "show", "main:src/meanwhile.txt") == "moved"
        assert git(origin, "ls-tree", "--name-only", "main", "docs/releases/unreleased/") == late
        notes = "docs/releases/0.2.34.md"
        assert git(origin, "show", f"main:{notes}") == git(origin, "show", f"v0.2.34:{notes}")
        set_ci(conclusion="success")
        ok("main moved between the CI check and the merge: released anyway, without starting again; the tag is the "
           "tested notes commit, an ancestor of main")
        ok("a fragment that landed on main meanwhile survives the merge for the next release")

        # another pull request merges at the very moment of the release's merge: the merge is tried again
        head, before = fragment("ST-11"), len(prs())
        set_ci(conclusion="success", base_modified=True)
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.2.35" in r.stdout and "again from the new state" not in r.stderr, r
        assert "merging again" in r.stderr and [p["state"] for p in list(prs().values())[before:]] == ["MERGED"]
        notes = git(origin, "show", "v0.2.35:docs/releases/0.2.35.md")
        assert "- Late." in notes and "- ST-11." in notes, notes
        assert git(origin, "ls-tree", "--name-only", "main", "docs/releases/unreleased/") == ""
        ok("GitHub's \"base branch was modified\" retries the merge, not the release")

        # main changes a fragment the release removes: the merge conflicts, so the release starts again
        head, before = fragment("ST-12"), len(prs())
        set_ci(conclusion="success", move_main={"docs/releases/unreleased/ST-12.md": "- ST-12, reworded.\n"})
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.2.36" in r.stdout, r
        assert "again from the new state" in r.stderr and "does not merge cleanly" in r.stderr, r.stderr
        assert [p["state"] for p in list(prs().values())[before:]] == ["CLOSED", "MERGED"]
        assert git(origin, "show", "v0.2.36:docs/releases/0.2.36.md") == "# Steve v0.2.36\n\n- ST-12, reworded."
        assert git(origin, "log", "-1", "--format=%s", "v0.2.36^{commit}^") == "meanwhile"
        assert git(origin, "ls-tree", "--name-only", "main", "docs/releases/unreleased/") == ""
        assert git(origin, "branch", "--list", "release/*") == ""
        set_ci(conclusion="success")
        ok("a release pull request that conflicts with main: closed, and released from the new state")

        # a conflict every time: it gives up after a few attempts and tags nothing
        fragment("ST-13")
        spec2 = importlib.util.spec_from_file_location("cut_release2", CUT)
        m2 = importlib.util.module_from_spec(spec2)
        spec2.loader.exec_module(m2)
        real_gh = m2.gh

        def conflicting(*args, cwd):
            if args[:2] == ("pr", "merge"):
                raise m2.Refused("can't ask GitHub: gh pr merge: X Pull request #1 is not mergeable: the merge "
                                 "commit cannot be cleanly created.")
            return real_gh(*args, cwd=cwd)
        m2.gh = conflicting
        before = len(prs())
        try:
            m2.cut(b, "main", None, False, False)
            raise AssertionError("released a conflicting pull request")
        except m2.Refused as e:
            assert "origin moved while releasing" in str(e) and "does not merge cleanly" in str(e), e
        assert [p["state"] for p in list(prs().values())[before:]] == ["CLOSED"] * m2.ATTEMPTS
        assert git(origin, "tag", "-l", "v0.2.37") == "" and git(origin, "branch", "--list", "release/*") == ""
        ok("a release pull request that never merges cleanly: closed each time, nothing tagged")

        commit(b, {"src/unmerged.txt": "u\n"}, "unmerged")
        git(b, "push", "-q", "origin", "HEAD:refs/heads/elsewhere")
        sleep, m2.time.sleep = m2.time.sleep, lambda s: None
        try:
            m2.tag(b, b, git(b, "rev-parse", "HEAD"), "0.2.37", "main")
            raise AssertionError("tagged a commit main does not contain")
        except m2.Refused as e:
            assert "is not on origin's main" in str(e), e
        finally:
            m2.time.sleep = sleep
        assert git(origin, "tag", "-l", "v0.2.37") == "" and git(b, "tag", "-l", "v0.2.37") == ""
        ok("a commit main does not contain is never tagged")

        # origin unreachable: an error, not "no tags"
        git(a, "remote", "set-url", "origin", str(Path(tmp, "gone.git")))
        r = cut(a, "next")
        assert r.returncode == 1 and "can't read the tags" in r.stderr, r
        ok("an unreachable origin is an error, never version 0.1.0")


def latest_tested_checks():
    with tempfile.TemporaryDirectory(prefix="steve-cut-tested-") as tmp:
        fake_gh(tmp)
        origin, a = setup(tmp)
        policy = "env:\n  RELEASE_BASE_POLICY: latest-tested-push\n"
        commit(a, {".github/workflows/ci.yml": policy, "src/tested.txt": "tested\n",
                   "docs/releases/unreleased/.gitkeep": "",
                   "docs/releases/unreleased/tested.md": "- Tested change.\n"}, "tested")
        tested = git(a, "rev-parse", "HEAD")
        commit(a, {"src/pending.txt": "pending\n", "docs/releases/unreleased/pending.md": "- Later.\n"}, "pending")
        pending = git(a, "rev-parse", "HEAD")
        commit(a, {"src/head.txt": "head\n"}, "head")
        head = git(a, "rev-parse", "HEAD")
        git(a, "push", "-q", "origin", "main")
        states = {tested: {"conclusion": "success"}, pending: {"conclusion": "cancelled", "older_conclusion": "success"},
                  head: {"status": "queued"}}
        set_ci(commits=states)
        spec = importlib.util.spec_from_file_location("cut_tested", CUT)
        m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(m)
        saved_env = dict(os.environ)
        os.environ.update(ENV)
        try:
            assert m.release_base(a, head, "main", 0) == tested
            states[head] = {"conclusion": "success"}
            set_ci(commits=states)
            assert m.release_base(a, head, "main", 0) == head
            states[head] = {"status": "queued"}
            # Commit order wins even if an older run completed later or has a higher run number.
            states[pending] = {"conclusion": "success"}
            set_ci(commits=states)
            assert m.release_base(a, head, "main", 0) == pending
            # A real failed run is not success either; a newer PR-only green never stands for a push.
            for state in ({"conclusion": "failure"}, {"none": True}, {"conclusion": "neutral"},
                          {"status": "in_progress", "conclusion": "success"}):
                states[pending] = state
                set_ci(commits=states)
                assert m.release_base(a, head, "main", 0) == tested
            # A tested feature SHA on a second parent is not a tested main push commit.
            git(a, "checkout", "-q", "-b", "feature", tested)
            commit(a, {"src/feature.txt": "feature\n"}, "feature")
            feature = git(a, "rev-parse", "HEAD")
            git(a, "checkout", "-q", "main")
            git(a, "merge", "-q", "--no-ff", "feature", "-m", "merge feature")
            merged = git(a, "rev-parse", "HEAD")
            states[feature] = {"conclusion": "success"}
            states[merged] = {"conclusion": "cancelled"}
            states[pending] = {"conclusion": "cancelled", "older_conclusion": "success"}
            set_ci(commits=states)
            assert m.release_base(a, merged, "main", 0) == tested
            git(a, "push", "-q", "origin", "main")
            # Nothing eligible passed: the bounded head gate refuses, without suggesting a cancelled
            # workflow passed. The already released tag's code is not a candidate for a new release.
            states[tested] = {"conclusion": "cancelled"}
            set_ci(commits=states)
            try:
                m.release_base(a, merged, "main", 0)
                raise AssertionError("released without a successful push")
            except m.Refused as e:
                assert "cancelled on" in str(e), e
            set_ci(down=True)
            try:
                m.release_base(a, merged, "main", 0)
                raise AssertionError("released with GitHub unavailable")
            except m.Refused as e:
                assert "can't read CI" in str(e), e
            states[tested] = {"conclusion": "success"}
            set_ci(commits=states)
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 0 and "released v0.2.25" in r.stdout, r
            assert git(origin, "rev-parse", "v0.2.25^{commit}^") == tested
            assert git(origin, "show", "v0.2.25:docs/releases/0.2.25.md") == "# Steve v0.2.25\n\n- Tested change."
            assert git(origin, "ls-tree", "--name-only", "v0.2.25", "src/pending.txt") == ""
            assert git(origin, "show", "main:docs/releases/unreleased/pending.md") == "- Later."
            assert git(origin, "show", "main:src/pending.txt") == "pending"
            assert git(origin, "ls-tree", "--name-only", "main", "docs/releases/unreleased/tested.md") == ""
            # Recovery finds the notes on current main before selecting another tested base.
            git(origin, "tag", "-d", "v0.2.25")
            git(a, "tag", "-d", "v0.2.25")
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 0 and "notes commit" in r.stderr, r
            assert git(origin, "rev-parse", "v0.2.25^{commit}^") == tested
        finally:
            os.environ.clear()
            os.environ.update(saved_env)
        ok("latest tested main push is released; cancelled/running/PR-only runs and feature SHAs are excluded; "
           "newer code and fragments stay on main, and interrupted tagging still recovers")


if __name__ == "__main__":
    main()
    latest_tested_checks()
    print("All cut-release checks passed")
