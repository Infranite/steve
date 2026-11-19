#!/usr/bin/env python3
"""Rerun a failed CI push run on main when its only failures are jobs GitHub never gave a runner.

    python3 scripts/rerun-unacquired.py RUN_ID ATTEMPT     (in .github/workflows/ci-rerun.yml)

GitHub's hosted macOS arm64 runners are sometimes all taken. A job that waits too long is cancelled by
GitHub itself, with no step run and the annotation "The job was not acquired by Runner of type hosted
even after multiple attempts". Such a run says nothing about the code, so this script asks GitHub to
rerun its failed jobs (`rerun-failed-jobs`), at most MAX_RERUNS times per run.

It reruns only when every failed or cancelled job of that attempt is either such an unacquired job (no
step and that annotation) or one that only reports on others: the `CI gate`, and a platform's `rust`
job when one of that platform's `rust-test` shards went unacquired. A job that ran any step and failed
(a test, a build, a check) is never rerun: that is a result to investigate, not a missing runner.
The exit code is 0 either way; the decision is printed.
"""
import argparse
import json
import os
import re
import sys
import time
import urllib.error
import urllib.request

MAX_RERUNS = 3
UNACQUIRED = "not acquired by Runner"
GATE = "CI gate"
NET_TIMEOUT = 30
FAILED = {"failure", "cancelled", "timed_out", "startup_failure"}


def platform(name, job):
    """The runner in a `rust-test (RUNNER, k/N)` or `rust (RUNNER)` job name, or None."""
    m = re.fullmatch(rf"{job} \(([^,)]+)(?:, [^)]*)?\)", name)
    return m and m.group(1)


def decide(jobs, annotations, attempt):
    """Return why the run may be rerun, or None with the reason it must not. `annotations(job)` lists the
    messages of one job's annotations."""
    if attempt > MAX_RERUNS:
        return None, f"attempt {attempt}: already rerun {MAX_RERUNS} times"
    failed = [j for j in jobs if j.get("conclusion") in FAILED]
    if not failed:
        return None, "no failed job"
    unacquired = [j for j in failed
                  if not j.get("steps") and any(UNACQUIRED in m for m in annotations(j))]
    if not unacquired:
        return None, "no job failed for want of a runner"
    starved = {platform(j["name"], "rust-test") for j in unacquired} - {None}
    for j in failed:
        if j in unacquired or j["name"] == GATE or platform(j["name"], "rust") in starved:
            continue
        return None, f"{j['name']} ran and ended {j.get('conclusion')}"
    names = ", ".join(j["name"] for j in unacquired)
    return f"only unacquired jobs failed ({names})", None


def api(repository, token, base, *, tries=3, sleep=time.sleep, opener=urllib.request.urlopen):
    def call(path, method="GET"):
        request = urllib.request.Request(f"{base}/repos/{repository}/{path}", method=method, headers={
            "Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28",
            "Authorization": f"Bearer {token}"})
        for left in reversed(range(tries)):
            try:
                with opener(request, timeout=NET_TIMEOUT) as answer:
                    body = answer.read()
                return json.loads(body) if body else None
            except urllib.error.HTTPError as error:  # GitHub answers 5xx now and then; a 4xx will not change
                if error.code < 500 or not left:
                    raise
            except OSError:
                if not left:
                    raise
            sleep(2)
    return call


def jobs_of(call, run, attempt):
    jobs, page = [], 1
    while True:
        answer = call(f"actions/runs/{run}/attempts/{attempt}/jobs?per_page=100&page={page}")
        jobs += answer.get("jobs") or []
        if len(jobs) >= answer.get("total_count", 0) or not answer.get("jobs"):
            return jobs
        page += 1


def main(argv=None, env=os.environ, call=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("run", type=int)
    ap.add_argument("attempt", type=int)
    args = ap.parse_args(argv)
    call = call or api(env["GITHUB_REPOSITORY"], env["GITHUB_TOKEN"],
                       env.get("GITHUB_API_URL", "https://api.github.com"))
    try:
        jobs = jobs_of(call, args.run, args.attempt)
        why, refusal = decide(jobs, lambda j: [a.get("message", "") for a in
                                               call(f"check-runs/{j['id']}/annotations") or []], args.attempt)
    except (OSError, ValueError, KeyError) as error:
        why, refusal = None, f"could not tell ({type(error).__name__}: {error})"
    if why is None:
        print(f"Not rerun: {refusal}")
        return 0
    call(f"actions/runs/{args.run}/rerun-failed-jobs", method="POST")
    print(f"Rerun of the failed jobs requested: {why}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
