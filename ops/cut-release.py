#!/usr/bin/env python3
"""Cut a release: the version is chosen here, at release time, never ahead on a branch.

  cut-release.py next [--minor]                    the version the next release gets
  cut-release.py cut [--minor | --version X.Y.Z] [--branch B] [--dry-run] [--ci-wait S | --skip-ci]
  cut-release.py lint [--base REF]                 a branch carries release-note fragments only

Release notes are fragments: each change adds `docs/releases/unreleased/<name>.md` (its item, e.g.
`ST-38.md`, or its branch), a few sentences or bullets, no version and no `# ` title. `cut` takes the
target branch as it is on `origin` (in a worktree of its own, so the caller's checkout does not matter),
gives it the next free version (one more than the highest `vX.Y.Z` tag on `origin`), joins the fragments
in the order they arrived on the branch into `docs/releases/X.Y.Z.md`, removes them, commits, tags
`vX.Y.Z` and pushes the commit and the tag together (`--atomic`): both land or neither does. When
someone else pushed or released first, it starts again from the new state, a few times at most.

Only a tested commit is tagged: `cut` first asks GitHub (`gh`: $STEVE_GH, else on PATH, else in /opt/homebrew/bin,
/usr/local/bin or /usr/bin, as the Hub finds it) for the CI workflow's run (`.github/workflows/
ci.yml`) pushed to the branch at the head it is about to release. It waits while that run is queued or running
(up to `--ci-wait` seconds, default 45 minutes), and refuses when it failed or there is none, naming the run and
how to run its failed jobs again (`gh run rerun <id> --failed`) when it was only a flake. `--skip-ci` releases
without that check, on purpose (it says so). `--dry-run` does not ask.

Until the branch's own run has finished, a pull request's run counts in one case: the head is that pull
request as GitHub merged it, and its commits are the pull request's commits with the same trees, one for one,
on the commit they were already on (a rebase merge of an up-to-date pull request). What that run tested is then
the very tree being released, workflow included, and a second run of it would only repeat the first. Any other
pull request run does not count: it tested something else.

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
# where gh is when PATH is launchd's bare one (the Hub's merge lane runs `cut` with its environment); the same
# places the Hub looks (steve-core config.rs gh_cli())
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
        raise Refused(f"can't check CI: no `gh` on PATH, in {', '.join(GH_PLACES)} or $STEVE_GH "
                      f"(install and log in, or --skip-ci to release untested)")
    try:
        r = subprocess.run([exe, *args], cwd=str(cwd), capture_output=True, text=True, timeout=NET_TIMEOUT,
                           env=dict(os.environ, GH_PROMPT_DISABLED="1", NO_COLOR="1"))
    except FileNotFoundError:
        raise Refused(f"can't check CI: no `gh` at {exe} (install and log in, or --skip-ci to release untested)")
    except subprocess.TimeoutExpired:
        raise Refused(f"gh {args[0]} timed out after {NET_TIMEOUT}s")
    if r.returncode != 0:
        raise Refused(f"can't read CI from GitHub: gh {' '.join(args[:2])}: {(r.stderr or r.stdout).strip()[:300]}")
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


def merged_pull(repo, sha, branch):
    """The pull request GitHub merged into `branch` as `sha`: (its number, its head commit), or None."""
    pulls = api(repo, f"commits/{sha}/pulls?per_page=20")
    for p in pulls if isinstance(pulls, list) else []:
        head = (p.get("head") or {}).get("sha")
        if (head and p.get("merged_at") and p.get("merge_commit_sha") == sha
                and (p.get("base") or {}).get("ref") == branch):
            return p.get("number"), head
    return None


def replayed(repo, sha, number, pull):
    """`sha` is the pull request's head `pull`, replayed onto the commit that head was already on: the same
    commits with the same trees, one for one (a rebase merge of an up-to-date pull request). The branch then
    held nothing the pull request lacked, so the merge its run tested had the tree of `pull`, which is the
    tree of `sha`."""
    try:
        if git("cat-file", "-e", f"{pull}^{{commit}}", cwd=repo, check=False).returncode != 0:
            git("fetch", "--quiet", "origin", f"refs/pull/{number}/head", cwd=repo, check=False, timeout=NET_TIMEOUT)
        base = git("merge-base", pull, sha, cwd=repo, check=False).stdout.strip()
        if not base:
            return False
        trees = []
        for tip in (pull, sha):
            rows = [l.split() for l in git("log", "--topo-order", "--format=%T %P", f"{base}..{tip}",
                                           cwd=repo).stdout.splitlines()]
            if any(len(r) != 2 for r in rows):  # a merge in between: not a replay
                return False
            trees.append([r[0] for r in rows])
        return trees[0] == trees[1]
    except Refused:
        return False


def rerun_hint(run):
    return (f"If it was a flake, run its failed jobs again: gh run rerun {run.get('id')} --failed "
            f"(then cut again once it passes); otherwise fix it on main.")


def ci_gate(repo, sha, branch, wait):
    """Return once the CI workflow passed on `sha`; refuse when it failed, is missing, or still runs after
    `wait` seconds. This is the workflow conclusion, never a successful job."""
    short = sha[:7]
    start = time.monotonic()
    said = None
    pull = ...  # the pull request merged as `sha` whose run tested this tree (None: there is none), once asked
    while True:
        run = ci_run(repo, sha, branch)
        waited = time.monotonic() - start
        if run is None or run.get("status") != "completed":
            if pull is ...:
                pull = merged_pull(repo, sha, branch)
                if pull and not replayed(repo, sha, *pull):
                    pull = None
            tested = newest(ci_runs(repo, pull[1], "pull_request")) if pull else None
            if tested and (tested.get("status"), tested.get("conclusion")) == ("completed", "success"):
                print(f"CI passed on pull request #{pull[0]} ({pull[1][:7]}), merged as {short} with the same "
                      f"commits and trees: {tested.get('html_url', tested.get('id'))}", file=sys.stderr)
                return tested
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
    if tuple(int(x) for x in version.split(".")) in remote_versions(repo):
        if not version_arg:
            raise Raced(f"v{version} was released meanwhile")
        raise Refused(f"v{version} is already on origin")
    with tempfile.TemporaryDirectory(prefix="steve-cut-") as tmp:
        wt = Path(tmp, "wt")
        git("worktree", "add", "--quiet", "--detach", str(wt), head, cwd=repo)
        try:
            parts = fragments(wt, version)
            if not parts:
                raise Refused(f"no release notes: add {FRAGMENTS}/<item>.md on {branch} first")
            notes = Path(wt, NOTES, f"{version}.md")
            text = assemble(version, parts)
            if dry:
                print(text, end="")
                return version
            if ci_wait is not None:
                ci_gate(repo, head, branch, ci_wait)
            for p, _ in parts:
                if p != f"{NOTES}/{version}.md":
                    git("rm", "--quiet", "--", p, cwd=wt)
            notes.write_text(text)
            git("add", "--", str(notes.relative_to(wt)), cwd=wt)
            git("commit", "--quiet", "-m", f"Release notes {version}", cwd=wt)
            git("tag", "-a", f"v{version}", "-m", f"Steve v{version}", cwd=wt)
            try:
                push(wt, branch, version)
            except BaseException:
                # the tag stays only if origin has it: a push that timed out may have landed
                if not landed(repo, branch, version, wt):
                    git("tag", "-d", f"v{version}", cwd=repo, check=False)
                    raise
        finally:
            git("worktree", "remove", "--force", str(wt), cwd=repo, check=False)
    try:
        git("fetch", "--quiet", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
    except Refused:
        pass  # released; the next fetch brings it here
    return version


def push(wt, branch, version):
    try:
        r = subprocess.run(["git", "-C", str(wt), "push", "--atomic", "--porcelain", "origin",
                            f"HEAD:refs/heads/{branch}", f"refs/tags/v{version}"], capture_output=True,
                           text=True, timeout=NET_TIMEOUT, env=dict(os.environ, GIT_TERMINAL_PROMPT="0"))
    except subprocess.TimeoutExpired:
        raise Refused(f"pushing v{version} timed out")
    if r.returncode != 0:
        if "[rejected]" in r.stdout or "[remote rejected]" in r.stdout:
            raise Raced((r.stdout + r.stderr).strip()[:300])
        raise Refused(f"push failed: {(r.stderr or r.stdout).strip()[:300]}")


def landed(repo, branch, version, wt):
    """origin has this release: its tag is our tag and its branch our commit. Unknown counts as no:
    the tag is then removed here, and the next run finds the version taken if it did land."""
    try:
        r = git("ls-remote", "origin", f"refs/tags/v{version}", f"refs/heads/{branch}", cwd=repo,
                check=False, timeout=NET_TIMEOUT)
    except Refused:
        return False
    if r.returncode != 0:
        return False
    refs = dict(reversed(l.split("\t", 1)) for l in r.stdout.splitlines() if "\t" in l)
    tag = git("rev-parse", f"refs/tags/v{version}", cwd=wt, check=False).stdout.strip()
    head = git("rev-parse", "HEAD", cwd=wt, check=False).stdout.strip()
    return refs.get(f"refs/tags/v{version}") == tag and refs.get(f"refs/heads/{branch}") == head


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
