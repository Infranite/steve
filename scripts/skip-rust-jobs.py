#!/usr/bin/env python3
"""Decide whether a push may skip CI's Rust jobs because it only changed release notes.

    python3 scripts/skip-rust-jobs.py        (in the CI workflow, on a push)

Nothing the Rust jobs compile or test reads `docs/releases/`, so a push that changed only files there
(the release notes commit `ops/cut-release.py` pushes) would repeat the Rust jobs of the commit before
it. When this workflow's push run on that commit passed, they are skipped: the script waits for that run
and writes `skip=true` to `$GITHUB_OUTPUT`. A passed run on that commit either ran the Rust jobs or
skipped them for the same reason, so a passed run always stands on Rust jobs that ran on the same code.

Anything else writes `skip=false` and the Rust jobs run: another file changed, the push rewrote the
branch or is its first, that run failed, was cancelled, never appeared or is still going when `--wait`
runs out, or GitHub did not answer. The exit code is 0 either way; the decision is the output.
"""
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request

NOTES = "docs/releases/"
MAX_FILES = 300  # what the compare API lists at most: a longer change may be cut short
WAIT = 40 * 60   # the run on the commit before, from its start to its end, with room for queueing
APPEAR = 120     # how long that run may take to show up at all
POLL = 30
NET_TIMEOUT = 30


class Undecided(Exception):
    """Why the Rust jobs run."""


def changed(compare):
    """The paths a compare answer touches, old names of renames included; refuses anything but a plain
    step forward that fits in one answer."""
    if compare.get("status") != "ahead" or compare.get("behind_by") != 0:
        raise Undecided(f"the push is not a plain step forward ({compare.get('status')})")
    files = compare.get("files")
    if not isinstance(files, list) or not files:
        raise Undecided("GitHub lists no changed files")
    if len(files) >= MAX_FILES:
        raise Undecided(f"{len(files)} or more files changed")
    paths = []
    for f in files:
        paths += [p for p in (f.get("filename"), f.get("previous_filename")) if p]
    return paths


def newest(runs, sha):
    """The newest push run on `sha`, as ops/cut-release.py picks it."""
    mine = [r for r in runs if r.get("head_sha") == sha and r.get("event") == "push"]
    return max(mine, key=lambda r: (r.get("run_number") or 0, r.get("id") or 0), default=None)


def decide(get, before, sha, workflow, branch, *, wait=WAIT, appear=APPEAR, poll=POLL,
           clock=time.monotonic, sleep=time.sleep):
    """Return why the Rust jobs can be skipped, or raise Undecided. `get(path)` reads the repository's API."""
    if not before or set(before) == {"0"}:
        raise Undecided("there is no commit before this push")
    other = [p for p in changed(get(f"compare/{before}...{sha}")) if not p.startswith(NOTES)]
    if other:
        raise Undecided(f"it changes {other[0]}" + (f" and {len(other) - 1} more" if len(other) > 1 else ""))
    start = clock()
    while True:
        try:
            found = get(f"actions/workflows/{workflow}/runs?head_sha={before}&event=push&branch={branch}&per_page=20")
            run = newest(found.get("workflow_runs") or [], before)
        except (OSError, ValueError) as error:  # one bad answer is not a verdict: ask again
            run, state = None, f"GitHub did not answer ({error})"
        else:
            state = "no run yet" if run is None else run.get("status")
        if run is not None and run.get("status") == "completed":
            if run.get("conclusion") == "success":
                return f"only {NOTES} changed, and CI passed on {before[:7]}: {run.get('html_url', run.get('id'))}"
            raise Undecided(f"CI on {before[:7]} ended {run.get('conclusion')}")
        waited = clock() - start
        if waited >= (appear if run is None else wait):
            raise Undecided(f"CI on {before[:7]} has not passed after {int(waited)}s ({state})")
        print(f"waiting for CI on {before[:7]} ({state})", flush=True)
        sleep(poll)


def api(repository, token, base, *, tries=3, sleep=time.sleep, opener=urllib.request.urlopen):
    def get(path):
        request = urllib.request.Request(f"{base}/repos/{repository}/{path}", headers={
            "Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
            **({"Authorization": f"Bearer {token}"} if token else {})})
        for left in reversed(range(tries)):
            try:
                with opener(request, timeout=NET_TIMEOUT) as answer:
                    value = json.load(answer)
                break
            except urllib.error.HTTPError as error:  # GitHub answers 5xx now and then; a 4xx will not change
                if error.code < 500 or not left:
                    raise
            except OSError:
                if not left:
                    raise
            sleep(2)
        if not isinstance(value, dict):
            raise ValueError("unexpected answer")
        return value
    return get


def main(argv=None, env=os.environ, get=None, **timing):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--wait", type=int, default=WAIT, help="seconds to wait for the run on the commit before")
    args = ap.parse_args(argv)
    try:
        with open(env["GITHUB_EVENT_PATH"], encoding="utf-8") as f:
            before = json.load(f).get("before")
        # GITHUB_WORKFLOW_REF: owner/repo/.github/workflows/ci.yml@refs/heads/main
        workflow = env["GITHUB_WORKFLOW_REF"].split("@")[0].rsplit("/", 1)[-1]
        get = get or api(env["GITHUB_REPOSITORY"], env.get("GITHUB_TOKEN"),
                         env.get("GITHUB_API_URL", "https://api.github.com"))
        reason = decide(get, before, env["GITHUB_SHA"], workflow, env["GITHUB_REF_NAME"], wait=args.wait, **timing)
        skip = True
    except Undecided as why:
        skip, reason = False, str(why)
    except (OSError, ValueError, KeyError, AttributeError) as error:
        skip, reason = False, f"could not tell ({type(error).__name__}: {error})"
    print(f"Rust jobs {'skipped' if skip else 'run'}: {reason}")
    if out := env.get("GITHUB_OUTPUT"):
        with open(out, "a", encoding="utf-8") as f:
            f.write(f"skip={'true' if skip else 'false'}\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
