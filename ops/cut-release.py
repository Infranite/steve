#!/usr/bin/env python3
"""Cut a release: the version is chosen here, at release time, never ahead on a branch.

  cut-release.py next [--minor]                    the version the next release gets
  cut-release.py cut [--minor | --version X.Y.Z] [--branch B] [--dry-run] [--ci-wait S | --skip-ci]
  cut-release.py lint [--base REF]                 a branch carries release-note fragments only

Versions are MAJOR.MINOR.PATCH (CONTRIBUTING.md, "Changes and releases"): PATCH for every ordinary
release; --minor when an operator must act (Hub/Node or client protocol change, something removed or
changed in meaning, manual configuration, an upgrade that reinstalling the previous release cannot
undo), announced by a fragment whose first line is `Needs a minor release: <why>`; MAJOR stays 0 until
the interfaces are declared stable.

Release notes are fragments: each change adds `docs/releases/unreleased/<name>.md` (its item, e.g.
`ST-38.md`, or its branch), a few sentences or bullets, no version and no `# ` title. `cut` takes the
target branch on `origin` (in a worktree of its own, so the caller's checkout does not matter),
gives it the next free version (one more than the highest `vX.Y.Z` tag on `origin`), joins the fragments
in the order they arrived on the branch into `docs/releases/X.Y.Z.md` and removes them.

The branch takes changes only through pull requests (it may be protected), so the notes go in as one. `cut`
takes a tested commit as `base`, commits the notes on it (adding `docs/releases/X.Y.Z.md`, removing the
fragments present at `base`, nothing else), pushes that commit to `release/vX.Y.Z`, opens a pull request into
the branch, waits for its CI run, merges it with a merge commit (`--match-head-commit`, so exactly that commit
goes in), and then tags the notes commit itself with the annotated tag `vX.Y.Z`: the tag is the tree that was
tested, whatever landed on the branch meanwhile. Only the tag is pushed directly; branch rules do not cover tags.

The branch keeps moving while a release waits for CI (other pull requests merge every few minutes), and that is
no reason to start again: the merge commit joins the notes commit to whatever the branch has become. Fragments
that arrived after `base` are not touched by the notes commit, so the merge keeps them for the next release.
`cut` starts again from the new state, a few times at most, only on a real problem: the pull request does not
merge cleanly (someone changed a fragment it removes), someone else released the version first, or the release
branch was taken while pushing it. The pull request is then closed and its branch deleted. A release branch
already on `origin` is someone else's release in progress (or one left behind: delete it once nobody is
releasing); `cut` refuses it.

The tag is pushed after the merge, never before: the tag starts the Release workflow, which publishes it, and a
tag pushed first would publish a version the branch might never contain when the merge then fails. Before
tagging, `cut` checks that the notes commit is on origin's branch. A release whose pull request merged but whose
tag never landed is finished by the next `cut`: the branch then carries `docs/releases/X.Y.Z.md` of an untagged
version, added by a notes commit of `cut`'s shape, and that commit is tagged as it is (after the same CI
checks); fragments that arrived since wait for the release after it.

Only a tested commit is released: `cut` first asks GitHub (`gh`: $STEVE_GH, else on PATH, else in
/opt/homebrew/bin, /usr/local/bin or /usr/bin, as the Hub finds it) for the CI workflow's run
(`.github/workflows/ci.yml`) pushed to the branch at `base`. When main's workflow declares
`RELEASE_BASE_POLICY: latest-tested-push`, it picks the newest first-parent commit since the last release
whose own latest full push run completed successfully. Newer pending, cancelled or failed commits wait for
the next release; cancellation never counts as success. With no eligible successful commit it checks the
head as before. Older workflows and other branches always require the head's run, preserving their release
policy without relying on a workflow change that has not reached main. It waits while that run is queued or
running (up to
`--ci-wait` seconds, default 45 minutes), and refuses when it failed or there is none, naming the run and how to
run its failed jobs again (`gh run rerun <id> --failed`) when it was only a flake. The notes commit then needs
its release pull request's run to pass: that run checks the notes (ci.yml skips its Rust and Apple jobs when the
pull request changes release notes alone and stands right on a main commit whose run passed), so the tagged
tree is `base`, tested on every platform, plus notes that passed their checks. `--skip-ci` releases without
either check, on purpose (it says so; a protected branch that requires the run then refuses the merge).
`--dry-run` previews the current head's notes without choosing a tested base, asking CI or pushing anything.

Only `base`'s own push run counts for it. Pull requests run the Linux jobs only (macOS runs on main), so a pull
request's green run is never taken for the commit it was merged as.

A `docs/releases/X.Y.Z.md` for a version no tag has yet (written ahead the old way) is taken as the
first fragment of that release.

`lint` fails when the branch adds or changes `docs/releases/X.Y.Z.md` itself: such a file picks a
version before the release does, and two branches picking the same one is how releases collided.

Talks to `origin`; "can't reach it" is an error, never "there are no tags".
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

NOTES = "docs/releases"
FRAGMENTS = f"{NOTES}/unreleased"
VERSION_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
NOTES_RE = re.compile(r"^docs/releases/\d+\.\d+\.\d+\.md$")
ATTEMPTS = 3
NET_TIMEOUT = 60
CI_WORKFLOW = "ci.yml"
CI_WAIT = 45 * 60
CI_POLL = 30
MERGE_TRIES = 3  # a merge refused because the target moved at that very moment is tried again
MERGE_RETRY = 5
CI_APPEAR = 180  # a push's run shows up within seconds; one that never does is no run
# where gh is when PATH is launchd's bare one (a service running `cut` with its environment); the same places
# the Hub looks (steve-core config.rs gh_cli())
GH_PLACES = ("/opt/homebrew/bin/gh", "/usr/local/bin/gh", "/usr/bin/gh")


class Refused(Exception):
    pass


class Raced(Exception):
    """The push lost to someone else's: start again from the new state."""


def git(*args, cwd, check=True, timeout=None):
    try:
        r = subprocess.run(["git", "-C", str(cwd), *args], capture_output=True, text=True, timeout=timeout,
                           env=dict(os.environ, GIT_TERMINAL_PROMPT="0"))
    except subprocess.TimeoutExpired:
        raise Refused(f"git {args[0]} timed out after {timeout}s")
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


def gh_path():
    """$STEVE_GH, else `gh` on PATH, else the usual install places; None when there is none."""
    if g := os.environ.get("STEVE_GH"):
        return g
    return shutil.which("gh") or next((p for p in GH_PLACES if os.access(p, os.X_OK)), None)


def gh(*args, cwd):
    """`gh` in the repository: `{owner}/{repo}` is its GitHub remote (or GH_REPO)."""
    exe = gh_path()
    if exe is None:
        raise Refused(f"can't reach GitHub: no `gh` on PATH, in {', '.join(GH_PLACES)} or $STEVE_GH "
                      f"(install it and log in)")
    try:
        r = subprocess.run([exe, *args], cwd=str(cwd), capture_output=True, text=True, timeout=NET_TIMEOUT,
                           env=dict(os.environ, GH_PROMPT_DISABLED="1", NO_COLOR="1"))
    except FileNotFoundError:
        raise Refused(f"can't reach GitHub: no `gh` at {exe} (install it and log in)")
    except subprocess.TimeoutExpired:
        raise Refused(f"gh {args[0]} timed out after {NET_TIMEOUT}s")
    if r.returncode != 0:
        what = "read CI from GitHub" if args[:1] == ("api",) else "ask GitHub"
        raise Refused(f"can't {what}: gh {' '.join(args[:2])}: {(r.stderr or r.stdout).strip()[:300]}")
    return r.stdout


def api(repo, path):
    out = gh("api", f"repos/{{owner}}/{{repo}}/{path}", cwd=repo)
    try:
        return json.loads(out)
    except ValueError:
        raise Refused(f"can't read CI from GitHub: unexpected answer {out.strip()[:200]!r}")


def ci_runs(repo, sha, event, more=""):
    """The runs of the CI workflow on `sha` for `event`."""
    found = api(repo, f"actions/workflows/{CI_WORKFLOW}/runs?head_sha={sha}&event={event}{more}&per_page=20")
    runs = found.get("workflow_runs") if isinstance(found, dict) else None
    if not isinstance(runs, list):
        raise Refused(f"can't read CI from GitHub: unexpected answer {json.dumps(found)[:200]!r}")
    return [r for r in runs if r.get("head_sha") == sha and r.get("event") == event]


def newest(runs):
    return max(runs, key=lambda r: (r.get("run_number") or 0, r.get("id") or 0), default=None)


def ci_run(repo, sha, branch):
    """The newest run of the CI workflow pushed to `branch` at `sha`, or None."""
    return newest([r for r in ci_runs(repo, sha, "push", f"&branch={branch}")
                   if r.get("head_branch") in (None, branch)])


def rerun_hint(run):
    return (f"If it was a flake, run its failed jobs again: gh run rerun {run.get('id')} --failed "
            f"(then cut again once it passes); otherwise fix it on main.")


def ci_gate(repo, sha, branch, wait):
    """Return once the CI workflow passed on `sha`; refuse when it failed, is missing, or still runs after
    `wait` seconds. This is the workflow conclusion, never a successful job."""
    short = sha[:7]
    start = time.monotonic()
    said = None
    # Only the head's own push run counts: pull requests run the Linux jobs only, so a pull request's green
    # run never stands for the macOS half of a release.
    while True:
        run = ci_run(repo, sha, branch)
        waited = time.monotonic() - start
        if run is None:
            if waited >= min(wait, CI_APPEAR):
                raise Refused(f"CI ({CI_WORKFLOW}) has no push run on {short}: only a tested commit is released "
                              f"(push it to the branch and wait for CI, or --skip-ci to release untested)")
            state = "no run yet"
        elif run.get("status") == "completed":
            conclusion = run.get("conclusion")
            if conclusion == "success":
                print(f"CI passed on {short}: {run.get('html_url', run.get('id'))}", file=sys.stderr)
                return run
            if conclusion == "cancelled":
                raise Refused(f"CI ({CI_WORKFLOW}) cancelled on {short}: {run.get('html_url', run.get('id'))}. "
                              "No successful push run verifies this commit; cut again once a tested commit is available.")
            raise Refused(f"CI ({CI_WORKFLOW}) {conclusion} on {short}: {run.get('html_url', run.get('id'))}. "
                          + rerun_hint(run))
        else:
            state = run.get("status") or "pending"
        if waited >= wait:
            where = f": {run.get('html_url', run.get('id'))}" if run else ""
            raise Refused(f"CI ({CI_WORKFLOW}) on {short} is still {state} after {int(waited)}s{where}; "
                          f"cut again once it has passed")
        if state != said:
            print(f"waiting for CI on {short} ({state})", file=sys.stderr)
            said = state
        time.sleep(CI_POLL)


def release_base(repo, head, branch, wait):
    """Select a tested first-parent commit when the branch's workflow opts in; older workflows keep
    requiring the current head. A cancelled run is never success, nor is a pull-request run."""
    workflow = git("show", f"{head}:.github/workflows/{CI_WORKFLOW}", cwd=repo, check=False)
    if branch != "main" or not re.search(r"^  RELEASE_BASE_POLICY: latest-tested-push$", workflow.stdout, re.M):
        return head
    # Do not go back to code already released. Walk branch order, not run completion order: slow runs
    # and reruns can finish out of order. First-parent excludes a feature branch's pre-merge SHA.
    commits = git("rev-list", "--first-parent", head, "--not", "--glob=refs/tags/v[0-9]*", cwd=repo).stdout.split()
    for sha in commits:
        run = ci_run(repo, sha, branch)
        if run and run.get("status") == "completed" and run.get("conclusion") == "success":
            if sha != head:
                print(f"releasing tested commit {sha[:7]}; newer commits on {branch} wait for the next release",
                      file=sys.stderr)
            return sha
    # Nothing tested yet: retain the bounded wait and diagnostics on the head, including real failures.
    ci_gate(repo, head, branch, wait)
    return head


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


def cut_once(repo, branch, version_arg, minor, dry, ci_wait=CI_WAIT):
    if git("fetch", "--quiet", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT).returncode != 0:
        raise Refused("can't fetch origin")
    base = git("rev-parse", f"refs/remotes/origin/{branch}^{{commit}}", cwd=repo).stdout.strip()
    version = version_arg or next_version(repo, minor)
    taken(repo, version, version_arg)
    with tempfile.TemporaryDirectory(prefix="steve-cut-") as tmp:
        wt = Path(tmp, "wt")
        git("worktree", "add", "--quiet", "--detach", str(wt), base, cwd=repo)
        try:
            notes_rel = f"{NOTES}/{version}.md"
            if left := left_behind(wt, version):
                # a release whose pull request merged but whose tag never landed: its notes commit is tagged as
                # it is, once it passes the checks it had to pass before its merge. Fragments that arrived since
                # wait for the release after it.
                print(f"v{version}'s notes commit {left[:7]} is on {branch} without its tag: tagging it",
                      file=sys.stderr)
                if dry:
                    print(git("show", f"{left}:{notes_rel}", cwd=wt).stdout, end="")
                    return version
                if ci_wait is not None:
                    ci_gate(repo, git("rev-parse", f"{left}^", cwd=wt).stdout.strip(), branch, ci_wait)
                    notes_ci(repo, left, f"notes commit {left[:7]}", ci_wait, branch)
                tag(repo, wt, left, version, branch)
                return fetched(repo, version)
            if not dry and ci_wait is not None:
                base = release_base(repo, base, branch, ci_wait)
                git("checkout", "--quiet", "--detach", base, cwd=wt)
            parts = fragments(wt, version)
            if not parts:
                raise Refused(f"no release notes: add {FRAGMENTS}/<item>.md on {branch} first")
            # notes of this untagged version written ahead the old way, alone: `base` is tagged as it is
            ready = [p for p, _ in parts] == [notes_rel]
            text = parts[0][1] if ready else assemble(version, parts)
            if dry:
                print(text, end="")
                return version
            if ci_wait is not None:
                ci_gate(repo, base, branch, ci_wait)
            if ready:
                target = base
            else:
                # The notes commit: the notes added, the fragments present at `base` removed, nothing else.
                # Fragments that reach the branch after `base` are no part of it, so the merge keeps them.
                for p, _ in parts:
                    if p != notes_rel:
                        git("rm", "--quiet", "--", p, cwd=wt)
                Path(wt, notes_rel).write_text(text)
                git("add", "--", notes_rel, cwd=wt)
                git("commit", "--quiet", "-m", f"Release notes {version}", cwd=wt)
                target = release_pr(repo, wt, branch, version, text, ci_wait)
            tag(repo, wt, target, version, branch)
        finally:
            git("worktree", "remove", "--force", str(wt), cwd=repo, check=False)
    return fetched(repo, version)


def fetched(repo, version):
    try:
        git("fetch", "--quiet", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
    except Refused:
        pass  # released; the next fetch brings it here
    return version


def left_behind(wt, version):
    """The notes commit of a release of `version` that reached the branch (checked out in `wt`) without its
    tag, or None. It is the commit that added `docs/releases/X.Y.Z.md`, in the shape `cut` makes it: titled
    `Release notes X.Y.Z`, one parent, that file added and fragments removed, nothing else. A release pull
    request goes in with a merge commit, so the notes commit is the merge's second parent; `git log` on the
    file follows the parent it came from. Notes written ahead by hand (the old way) are not one: they are the
    first fragment of their release."""
    rel = f"{NOTES}/{version}.md"
    if not Path(wt, rel).is_file():
        return None
    c = git("log", "-1", "--format=%H", "--diff-filter=A", "--", rel, cwd=wt).stdout.strip()
    if not c or git("log", "-1", "--format=%s", c, cwd=wt).stdout.strip() != f"Release notes {version}":
        return None
    if len(git("rev-list", "--parents", "-n", "1", c, cwd=wt).stdout.split()) != 2:
        return None
    changes = git("diff", "--name-status", "--no-renames", f"{c}^", c, cwd=wt).stdout.splitlines()
    removed = re.compile(rf"D\t{re.escape(FRAGMENTS)}/[^/]+\.md")
    if f"A\t{rel}" not in changes or any(l != f"A\t{rel}" and not removed.fullmatch(l) for l in changes):
        return None
    return c


def taken(repo, version, version_arg):
    if tuple(int(x) for x in version.split(".")) in remote_versions(repo):
        if not version_arg:
            raise Raced(f"v{version} was released meanwhile")
        raise Refused(f"v{version} is already on origin")


def remote_ref(repo, ref):
    """The commit `ref` names on origin, "" when it has none."""
    r = git("ls-remote", "origin", ref, cwd=repo, check=False, timeout=NET_TIMEOUT)
    if r.returncode != 0:
        raise Refused(f"can't read {ref} on origin: {r.stderr.strip()[:300]}")
    return next((l.split("\t", 1)[0] for l in r.stdout.splitlines() if l.endswith(f"\t{ref}")), "")


def push(wt, *refspecs):
    try:
        r = subprocess.run(["git", "-C", str(wt), "push", "--porcelain", "origin", *refspecs], capture_output=True,
                           text=True, timeout=NET_TIMEOUT, env=dict(os.environ, GIT_TERMINAL_PROMPT="0"))
    except subprocess.TimeoutExpired:
        raise Refused(f"pushing {refspecs[-1]} timed out")
    if r.returncode != 0:
        if "[rejected]" in r.stdout or "[remote rejected]" in r.stdout:
            raise Raced((r.stdout + r.stderr).strip()[:300])
        raise Refused(f"push failed: {(r.stderr or r.stdout).strip()[:300]}")


def release_pr(repo, wt, branch, version, text, ci_wait):
    """Bring the release notes commit in `wt` into `branch` through a pull request merged with a merge commit,
    and return the notes commit itself: what its pull request's run tested, now on the branch. Anything short
    of the merge closes the pull request and deletes its branch."""
    rb = f"release/v{version}"
    sha = git("rev-parse", "HEAD", cwd=wt).stdout.strip()
    if remote_ref(repo, f"refs/heads/{rb}"):
        raise Refused(f"{rb} is already on origin: another release of v{version} is in progress, or one was "
                      f"left behind (delete it with `git push origin --delete {rb}` once nobody is releasing)")
    push(wt, f"HEAD:refs/heads/{rb}")  # rejected: someone took the release branch meanwhile, start again
    number = None
    try:
        url = gh("pr", "create", "--base", branch, "--head", rb, "--title", f"Release notes {version}",
                 "--body", f"Release notes of v{version}, opened by `ops/cut-release.py cut`, which merges it and "
                           f"tags this pull request's commit.\n\n{text}", cwd=repo).strip().splitlines()[-1]
        m = re.search(r"/pull/(\d+)$", url)
        if not m:
            raise Refused(f"can't tell the release pull request from {url!r}")
        number = m.group(1)
        print(f"release pull request #{number}: {url}", file=sys.stderr)
        if ci_wait is not None:
            notes_ci(repo, sha, f"release pull request #{number}", ci_wait)
        # The branch may well have moved since `base`: that changes nothing about the notes commit, which is
        # what is tagged. Only someone else's release of this version is a reason to start again.
        taken(repo, version, None)
        merge(repo, number, sha)
    except BaseException:
        if number:
            gh_quiet(repo, "pr", "close", number, "--comment", "Closed by `ops/cut-release.py cut`: not released.")
        git("push", "--quiet", "origin", "--delete", rb, cwd=repo, check=False, timeout=NET_TIMEOUT)
        raise
    git("push", "--quiet", "origin", "--delete", rb, cwd=repo, check=False, timeout=NET_TIMEOUT)
    return sha


def gh_quiet(repo, *args):
    try:
        gh(*args, cwd=repo)
    except Refused as e:
        print(f"warning: {e}", file=sys.stderr)


def notes_ci(repo, sha, what, wait, branch=None):
    """Return once the CI workflow passed on the notes commit `sha`: its release pull request's run, or with
    `branch` (a notes commit already on it) also a push run there. Either checks the same change of notes
    on a tested `base`; a notes commit that was rebase-merged before merge commits were used has only the
    push run."""
    start, said = time.monotonic(), None
    open_pr = branch is None
    while True:
        runs = ci_runs(repo, sha, "pull_request")
        if branch is not None:
            runs += [r for r in [ci_run(repo, sha, branch)] if r]
        run = newest(runs)
        waited = time.monotonic() - start
        if run is not None and run.get("status") == "completed":
            if run.get("conclusion") == "success":
                print(f"CI passed on {what}: {run.get('html_url', run.get('id'))}", file=sys.stderr)
                return
            raise Refused(f"CI ({CI_WORKFLOW}) {run.get('conclusion')} on {what}: "
                          f"{run.get('html_url', run.get('id'))}. "
                          + ("It is closed; cut again once the cause is fixed (a flake: just cut again)" if open_pr
                             else rerun_hint(run)))
        state = "no run yet" if run is None else run.get("status") or "pending"
        if waited >= (min(wait, CI_APPEAR) if run is None else wait):
            raise Refused(f"CI ({CI_WORKFLOW}) on {what} is still {state} after {int(waited)}s; "
                          + ("it is closed, cut again" if open_pr else "cut again once it has passed"))
        if state != said:
            print(f"waiting for CI on {what} ({state})", file=sys.stderr)
            said = state
        time.sleep(CI_POLL)


def merge(repo, number, sha):
    """Merge the release pull request at `sha` with a merge commit, and return that commit.

    A merge commit, not a rebase: `sha` goes into the branch as it is, so the tag can name the very commit
    whose run passed, however far the branch moved meanwhile. Its three-way merge applies only what `sha`
    changed from `base` (the notes added, the fragments of `base` removed), so fragments that arrived on the
    branch since stay for the next release. `--match-head-commit` merges nothing but `sha`."""
    for attempt in range(1, MERGE_TRIES + 1):
        try:
            gh("pr", "merge", number, "--merge", "--match-head-commit", sha, cwd=repo)
            break
        except Refused as e:
            why = str(e)
            if re.search(r"base branch was modified", why, re.I) and attempt < MERGE_TRIES:
                # another pull request merged at the same moment: the merge itself is tried again
                print(f"{why}; merging again", file=sys.stderr)
                time.sleep(MERGE_RETRY)
                continue
            if re.search(r"not mergeable|cannot be cleanly created|conflict", why, re.I):
                raise Raced(f"release pull request #{number} does not merge cleanly: {why}")
            if re.search(r"head branch was modified", why, re.I):
                raise Raced(f"release pull request #{number}'s branch changed: {why}")
            if re.search(r"base branch was modified", why, re.I):
                raise Raced(why)
            if re.search(r"not up to date", why, re.I):
                raise Refused(f"merging release pull request #{number}: {why}. `cut` merges it with a merge commit "
                              f"while the branch moves on, so the branch rules must not require pull requests to "
                              f"be up to date before merging")
            raise Refused(f"merging release pull request #{number}: {why}")
    for _ in range(10):
        out = gh("pr", "view", number, "--json", "state,mergeCommit", cwd=repo)
        try:
            pr = json.loads(out)
        except ValueError:
            raise Refused(f"can't read release pull request #{number}: {out.strip()[:200]!r}")
        if pr.get("state") == "MERGED" and (pr.get("mergeCommit") or {}).get("oid"):
            return pr["mergeCommit"]["oid"]
        time.sleep(3)
    raise Refused(f"release pull request #{number} did not show as merged")


def tag(repo, wt, target, version, branch):
    """Tag `target` `vX.Y.Z` and push only the tag, once origin's `branch` contains `target`.

    The tag comes after the merge, never before it. A pushed tag starts the Release workflow, which publishes
    the version; tagged first, a merge that then failed (a conflict, a branch rule) would leave a published
    release that the branch does not contain, and its number taken. Merged first, a tag that then fails to
    land leaves only notes on the branch, which the next `cut` finds and tags (`left_behind`)."""
    name = f"v{version}"
    for attempt in range(3):  # a merge GitHub reported may take a moment to show on the branch
        if attempt:
            time.sleep(3)
        git("fetch", "--quiet", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
        if git("merge-base", "--is-ancestor", target, f"refs/remotes/origin/{branch}", cwd=repo,
               check=False).returncode == 0:
            break
    else:
        raise Refused(f"{target[:7]} is not on origin's {branch}: v{version} is not tagged (cut again once it is)")
    git("tag", "-a", name, "-m", f"Steve {name}", target, cwd=repo)
    try:
        push(wt, f"refs/tags/{name}")
    except BaseException:
        # the tag stays only if origin has it: a push that timed out may have landed
        if not landed(repo, version):
            git("tag", "-d", name, cwd=repo, check=False)
            raise


def landed(repo, version):
    """origin has our tag. Unknown counts as no: the tag is then removed here, and the next run finds the
    version taken if it did land."""
    try:
        theirs = remote_ref(repo, f"refs/tags/v{version}")
    except Refused:
        return False
    return bool(theirs) and theirs == git("rev-parse", f"refs/tags/v{version}", cwd=repo, check=False).stdout.strip()


def cut(repo, branch, version_arg, minor, dry, ci_wait=CI_WAIT):
    """`ci_wait` None: no CI check (--skip-ci)."""
    for attempt in range(1, ATTEMPTS + 1):
        try:
            return cut_once(repo, branch, version_arg, minor, dry, ci_wait)
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
    ci = c.add_mutually_exclusive_group()
    ci.add_argument("--ci-wait", type=int, default=CI_WAIT, metavar="S",
                    help=f"how long to wait when no tested base is available (default {CI_WAIT}s)")
    ci.add_argument("--skip-ci", action="store_true", help="release without checking CI (untested)")
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
            if a.skip_ci and not a.dry_run:
                print("warning: --skip-ci: releasing without checking CI", file=sys.stderr)
            v = cut(repo, a.branch or default_branch(repo), a.version, a.minor, a.dry_run,
                    None if a.skip_ci else max(a.ci_wait, 0))
            if not a.dry_run:
                print(f"released v{v}")
                print(f"The Release build of v{v} runs on GitHub now. If it fails on a flake, do not cut another "
                      f"version: gh run rerun <run id> --failed (gh run list --workflow release.yml --branch v{v}) "
                      f"runs it again on the same tag.", file=sys.stderr)
        else:
            lint(repo, a.base)
    except Refused as e:
        print(f"✗ {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
