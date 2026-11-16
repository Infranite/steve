#!/usr/bin/env python3
"""Cut a release: the version is chosen here, at release time, never ahead on a branch.

  cut-release.py next [--minor]                    the version the next release gets
  cut-release.py cut [--minor | --version X.Y.Z] [--branch B] [--dry-run] [--ci-wait S | --skip-ci]
  cut-release.py lint [--base REF]                 a branch carries release-note fragments only

Release notes are fragments: each change adds `docs/releases/unreleased/<name>.md` (its item, e.g.
`ST-38.md`, or its branch), a few sentences or bullets, no version and no `# ` title. `cut` takes the
target branch as it is on `origin` (in a worktree of its own, so the caller's checkout does not matter),
gives it the next free version (one more than the highest `vX.Y.Z` tag on `origin`), joins the fragments
in the order they arrived on the branch into `docs/releases/X.Y.Z.md` and removes them.

The branch takes changes only through pull requests (it may be protected), so the notes go in as one: the
commit is pushed to `release/vX.Y.Z`, a pull request into the branch carries it, `cut` waits for that pull
request's CI run, rebase-merges it, and tags the commit it became on the branch with the annotated tag
`vX.Y.Z`. Only the tag is pushed directly; branch rules do not cover tags. A release branch already on
`origin` is someone else's release in progress (or one left behind: delete it once nobody is releasing).
When the branch moved before the merge, or someone else released first, the pull request is closed, its
branch deleted, and `cut` starts again from the new state, a few times at most. A release whose pull request
merged but whose tag never landed is finished by the next `cut`: the branch then carries the notes of an
untagged version and no other fragment, and that commit is tagged as it is.

Only a tested commit is tagged: `cut` first asks GitHub (`gh`: $STEVE_GH, else on PATH, else in /opt/homebrew/bin,
/usr/local/bin or /usr/bin, as the Hub finds it) for the CI workflow's run (`.github/workflows/
ci.yml`) pushed to the branch at the head it is about to release. It waits while that run is queued or running
(up to `--ci-wait` seconds, default 45 minutes), and refuses when it failed or there is none, naming the run and
how to run its failed jobs again (`gh run rerun <id> --failed`) when it was only a flake. `--skip-ci` releases
without that check and without waiting for the release pull request's run, on purpose (it says so; a
protected branch that requires the run then refuses the merge). `--dry-run` asks nothing and pushes nothing.

Only the head's own push run counts. Pull requests run the Linux jobs only (macOS runs on main), so a pull
request's green run is never taken for the head it was merged as.

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
    head = git("rev-parse", f"refs/remotes/origin/{branch}^{{commit}}", cwd=repo).stdout.strip()
    version = version_arg or next_version(repo, minor)
    taken(repo, version, version_arg)
    with tempfile.TemporaryDirectory(prefix="steve-cut-") as tmp:
        wt = Path(tmp, "wt")
        git("worktree", "add", "--quiet", "--detach", str(wt), head, cwd=repo)
        try:
            parts = fragments(wt, version)
            if not parts:
                raise Refused(f"no release notes: add {FRAGMENTS}/<item>.md on {branch} first")
            notes_rel = f"{NOTES}/{version}.md"
            # the notes of this untagged version are on the branch and nothing else waits: a release whose
            # pull request merged but whose tag never landed (or notes written ahead the old way, alone)
            ready = [p for p, _ in parts] == [notes_rel]
            text = parts[0][1] if ready else assemble(version, parts)
            if dry:
                print(text, end="")
                return version
            if ci_wait is not None:
                ci_gate(repo, head, branch, ci_wait)
            if ready:
                target = head
            else:
                for p, _ in parts:
                    if p != notes_rel:
                        git("rm", "--quiet", "--", p, cwd=wt)
                Path(wt, notes_rel).write_text(text)
                git("add", "--", notes_rel, cwd=wt)
                git("commit", "--quiet", "-m", f"Release notes {version}", cwd=wt)
                target = release_pr(repo, wt, branch, version, head, text, ci_wait)
            tag(repo, wt, target, version)
        finally:
            git("worktree", "remove", "--force", str(wt), cwd=repo, check=False)
    try:
        git("fetch", "--quiet", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
    except Refused:
        pass  # released; the next fetch brings it here
    return version


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


def release_pr(repo, wt, branch, version, base, text, ci_wait):
    """Bring the release notes commit in `wt` (on `base`) into `branch` through a pull request: the commit
    it became on the branch. Anything short of the merge closes the pull request and deletes its branch."""
    rb = f"release/v{version}"
    sha = git("rev-parse", "HEAD", cwd=wt).stdout.strip()
    if remote_ref(repo, f"refs/heads/{rb}"):
        raise Refused(f"{rb} is already on origin: another release of v{version} is in progress, or one was "
                      f"left behind (delete it with `git push origin --delete {rb}` once nobody is releasing)")
    push(wt, f"HEAD:refs/heads/{rb}")
    number = None
    try:
        url = gh("pr", "create", "--base", branch, "--head", rb, "--title", f"Release notes {version}",
                 "--body", f"Release notes of v{version}, opened by `ops/cut-release.py cut`, which merges it and "
                           f"tags the result.\n\n{text}", cwd=repo).strip().splitlines()[-1]
        m = re.search(r"/pull/(\d+)$", url)
        if not m:
            raise Refused(f"can't tell the release pull request from {url!r}")
        number = m.group(1)
        print(f"release pull request #{number}: {url}", file=sys.stderr)
        if ci_wait is not None:
            pr_ci(repo, sha, number, ci_wait)
        # the branch moved since its CI was checked: what would be tagged was not
        if remote_ref(repo, f"refs/heads/{branch}") != base:
            raise Raced(f"{branch} moved while the release pull request waited")
        taken(repo, version, None)
        merged = merge(repo, number, sha)
    except BaseException:
        if number:
            gh_quiet(repo, "pr", "close", number, "--comment", "Closed by `ops/cut-release.py cut`: not released.")
        git("push", "--quiet", "origin", "--delete", rb, cwd=repo, check=False, timeout=NET_TIMEOUT)
        raise
    git("push", "--quiet", "origin", "--delete", rb, cwd=repo, check=False, timeout=NET_TIMEOUT)
    git("fetch", "--quiet", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
    parent = git("rev-parse", f"{merged}^", cwd=repo, check=False).stdout.strip()
    if parent != base and ci_wait is not None:
        # merged onto a branch that had moved after all: its own run is what tested the tagged tree
        ci_gate(repo, merged, branch, ci_wait)
    return merged


def gh_quiet(repo, *args):
    try:
        gh(*args, cwd=repo)
    except Refused as e:
        print(f"warning: {e}", file=sys.stderr)


def pr_ci(repo, sha, number, wait):
    """Return once the CI workflow passed on the release pull request's head `sha`."""
    start, said = time.monotonic(), None
    while True:
        run = newest(ci_runs(repo, sha, "pull_request"))
        waited = time.monotonic() - start
        if run is not None and run.get("status") == "completed":
            if run.get("conclusion") == "success":
                print(f"CI passed on release pull request #{number}: {run.get('html_url', run.get('id'))}",
                      file=sys.stderr)
                return
            raise Refused(f"CI ({CI_WORKFLOW}) {run.get('conclusion')} on release pull request #{number}: "
                          f"{run.get('html_url', run.get('id'))}. It is closed; cut again once the cause is fixed "
                          f"(a flake: just cut again)")
        state = "no run yet" if run is None else run.get("status") or "pending"
        if waited >= (min(wait, CI_APPEAR) if run is None else wait):
            raise Refused(f"CI ({CI_WORKFLOW}) on release pull request #{number} is still {state} after "
                          f"{int(waited)}s; it is closed, cut again")
        if state != said:
            print(f"waiting for CI on release pull request #{number} ({state})", file=sys.stderr)
            said = state
        time.sleep(CI_POLL)


def merge(repo, number, sha):
    """Rebase-merge the release pull request at `sha`: the commit it became on the branch."""
    try:
        gh("pr", "merge", number, "--rebase", "--match-head-commit", sha, cwd=repo)
    except Refused as e:
        why = str(e)
        if re.search(r"not up to date|base branch was modified|head branch was modified", why, re.I):
            raise Raced(why)
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


def tag(repo, wt, target, version):
    """Tag `target` (on origin's branch) `vX.Y.Z` and push only the tag."""
    name = f"v{version}"
    if git("cat-file", "-e", f"{target}^{{commit}}", cwd=repo, check=False).returncode != 0:
        git("fetch", "--quiet", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
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
                    help=f"how long to wait for CI on the branch head (default {CI_WAIT}s)")
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
