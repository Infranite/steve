#!/usr/bin/env python3
"""Cut a release from Conventional Commits: version and release notes are derived at release time.

  cut-release.py next [--branch B] [--range A..B] [--offline]
  cut-release.py cut [--branch B] [--dry-run] [--range A..B] [--offline]
                     [--version X.Y.Z --reason WHY] [--ci-wait S | --skip-ci]
  cut-release.py lint [--base REF] [--head REF] [--range A..B]
                      [--pr-title TITLE] [--pr-body FILE] [--github-event PATH]

Versions are MAJOR.MINOR.PATCH (CONTRIBUTING.md, "Changes and releases"):
  - MAJOR: stability generation of public contracts; 0 until contracts are frozen at 1.0.0;
           after 1.0, incremented only on breaking changes (`!` or `BREAKING CHANGE:`).
  - MINOR: new user-visible capability (`feat`), operator action required (`Upgrade:` trailer),
           or breaking changes before 1.0. Resets PATCH to 0.
  - PATCH: fixes, performance, docs, and internal refactors only; safe to roll back in place.

Release notes are generated automatically from all commits since the last `vX.Y.Z` tag. Merged PRs
are listed from GitHub via GraphQL over the release range's time window and filtered to those whose
`mergeCommit.oid` lies in the release range (covering both direct merges and sub-PRs landed through
an integration PR). Multiple PRs sharing one `mergeCommit` are split into a single carrier PR (whose
`headRefOid` matches the merge commit's merged-branch parent) and independent sub-PR entries; a
`chore` carrier PR renders as one line under `## Other changes` listing its carried sub-PR numbers.
Live PR titles, descriptions, and optional `## Release note` sections come from GitHub; direct
commits without a merged PR use their commit subject. For PR entries, `Upgrade:` and
`BREAKING CHANGE:` trailers are read strictly from the PR description (never falling back to commit
trailers); direct commits without a PR read commit trailers. Identical notices are deduplicated under
`## Before upgrading`, followed by Features, Fixes, Performance, and Other changes.

`cut` takes the newest first-parent commit on `origin/<branch>` whose own full CI push run passed,
commits `docs/releases/X.Y.Z.md` on top of it (`chore(release): vX.Y.Z`, changing nothing else),
merges it through a release pull request (`--merge --match-head-commit`), and pushes the annotated
tag `vX.Y.Z` on the notes commit itself after the merge lands.

Talks to `origin`; "can't reach it" is an error, never "there are no tags".
"""
from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import textwrap
import time
import urllib.error
import urllib.request

NOTES = "docs/releases"
VERSION_RE = re.compile(r"^v(\d+)\.(\d+)\.(\d+)$")
NOTES_RE = re.compile(r"^docs/releases/(\d+)\.(\d+)\.(\d+)\.md$")
RELEASE_TITLE_RE = re.compile(r"^chore\(release\): v(\d+\.\d+\.\d+)$")
ISSUE_LINK_RE = re.compile(r"(?i)\b(?:closes|fixes|resolves)\s+#[1-9][0-9]*\b")
ALLOWED_TYPES = ("feat", "fix", "perf", "refactor", "docs", "test", "build", "ci", "chore", "revert")
HEADER_RE = re.compile(
    r"^(?P<type>feat|fix|perf|refactor|docs|test|build|ci|chore|revert)"
    r"(?:\((?P<scope>[A-Za-z0-9][A-Za-z0-9._/-]*)\))?"
    r"(?P<breaking>!)?"
    r": (?P<desc>\S.*)$"
)
PR_MERGE_RE = re.compile(r"^Merge pull request #(\d+) from \S+")
UPGRADE_RE = re.compile(r"^Upgrade:\s*(.*)$")
BREAKING_RE = re.compile(r"^BREAKING[ -]CHANGE:\s*(.*)$")
TRAILER_STOP_RE = re.compile(
    r"^(?:#{1,6}\s|---|===|```|~~~|Upgrade:\s*$|BREAKING[ -]CHANGE:\s*$"
    r"|[A-Za-z][A-Za-z0-9-]*:\s|BREAKING[ -]CHANGE:\s|(?i:Closes|Fixes|Resolves|Refs)\s+#\d+)"
)
RELEASE_NOTE_HEADING_RE = re.compile(r"^##\s+Release notes?\s*$", re.I)
NEXT_HEADING_RE = re.compile(r"^#{1,6}\s+")
RELEASE_NOTE_CHECKS_RE = re.compile(
    r"^(?:[*_]{1,2})?(?:local(?:\s+[a-z-]+)?\s+)?checks(?:[*_]{1,2})?[:：](?!:)",
    re.I,
)
RELEASE_NOTE_SIGNATURE_RE = re.compile(
    r"^(?:"
    r"🤖"
    r"|(?:[*_]{1,2})?Generated\s+(?:with|by)\s+\[[^\]]+\]\(https?://[^)]+\)(?:[*_]{1,2})?\s*$"
    r"|(?:Co-Authored-By|Signed-off-by):\s*\S"
    r")",
    re.I,
)
FENCE_RE = re.compile(r"^\s*(`{3,}|~{3,})")
HTML_COMMENT_RE = re.compile(r"<!--.*?-->", re.S)

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
# Cushion subtracted from the earliest commit timestamp in the release range when paginating
# GitHub's merged PRs (ordered by UPDATED_AT DESC), covering clock skew between git committer
# timestamps and GitHub's mergedAt/updatedAt timestamps.
MERGED_WINDOW_PAD_SECONDS = 24 * 3600
INTEGRATION_PR_SCOPE = "integration"
MERGED_PRS_GRAPHQL = """
query($owner: String!, $repo: String!, $endCursor: String) {
  repository(owner: $owner, name: $repo) {
    pullRequests(
      states: [MERGED]
      orderBy: {field: UPDATED_AT, direction: DESC}
      first: 100
      after: $endCursor
    ) {
      pageInfo {
        hasNextPage
        endCursor
      }
      nodes {
        number
        title
        body
        mergedAt
        updatedAt
        headRefOid
        mergeCommit {
          oid
        }
      }
    }
  }
}
""".strip()


class Refused(Exception):
    pass


class Raced(Exception):
    """The push lost to someone else's: start again from the new state."""


@dataclass(frozen=True)
class ParsedHeader:
    commit_type: str
    scope: str | None
    breaking: bool
    description: str


@dataclass(frozen=True)
class CommitRecord:
    sha: str
    parents: tuple[str, ...]
    committer_ts: int
    subject: str
    body: str


@dataclass(frozen=True)
class MergedPr:
    number: int
    title: str
    body: str
    merge_sha: str | None
    head_ref_oid: str | None
    merged_at: str | None
    updated_at: str | None


@dataclass(frozen=True)
class PrCommit:
    sha: str
    message: str


@dataclass(frozen=True)
class ReleaseEntry:
    sha: str
    pr_number: int | None
    raw_title: str
    commit_type: str | None
    scope: str | None
    breaking: bool
    description: str
    release_note: str | None
    upgrades: tuple[str, ...]
    breaking_notes: tuple[str, ...]
    carried_prs: tuple[int, ...] = ()


@dataclass(frozen=True)
class VersionPlan:
    version: str
    bump: str  # initial | patch | minor | major
    previous: str | None
    triggers: tuple[tuple[ReleaseEntry, tuple[str, ...]], ...]


def git(*args, cwd, check=True, timeout=None):
    try:
        r = subprocess.run(
            ["git", "-C", str(cwd), *args],
            capture_output=True,
            text=True,
            timeout=timeout,
            env=dict(os.environ, GIT_TERMINAL_PROMPT="0"),
        )
    except subprocess.TimeoutExpired:
        raise Refused(f"git {args[0]} timed out after {timeout}s")
    if check and r.returncode != 0:
        raise Refused(f"git {' '.join(args)}: {(r.stderr or r.stdout).strip()[:400]}")
    return r


def parse_version(text):
    m = re.fullmatch(r"(\d+)\.(\d+)\.(\d+)", text)
    if not m:
        return None
    return tuple(int(x) for x in m.groups())


def format_version(ver):
    return f"{ver[0]}.{ver[1]}.{ver[2]}"


def parse_header(title):
    line = title.strip()
    if "\n" in line or "\r" in line:
        return None
    m = HEADER_RE.match(line)
    if not m:
        return None
    return ParsedHeader(
        commit_type=m.group("type"),
        scope=m.group("scope"),
        breaking=bool(m.group("breaking")),
        description=m.group("desc").strip(),
    )


def format_header_error(title, label):
    allowed = "|".join(ALLOWED_TYPES)
    return (
        f"{label} {title!r} does not match Conventional Commits: "
        f"`type[(scope)][!]: imperative summary` (type in {allowed})"
    )


def validate_header(title, label):
    parsed = parse_header(title)
    if parsed is None:
        raise Refused(format_header_error(title, label))
    return parsed


def _fence_toggle(line, active_fence):
    m = FENCE_RE.match(line)
    if not m:
        return active_fence, False
    marker = m.group(1)
    if active_fence is None:
        return (marker[0], len(marker)), True
    if marker[0] == active_fence[0] and len(marker) >= active_fence[1] and line.strip() == marker:
        return None, True
    return active_fence, False


def _clean_trailer_value(first, continuation):
    head = first.rstrip()
    tail = textwrap.dedent("\n".join(continuation)).strip("\n") if continuation else ""
    if head and tail:
        return f"{head}\n{tail}".strip()
    return (head or tail).strip()


def parse_trailers(text, *, validate=False):
    """Extract `Upgrade:` and `BREAKING CHANGE:` trailers outside fenced code blocks."""
    upgrades, breaking = [], []
    if not text:
        return upgrades, breaking
    lines = text.splitlines()
    i, fence = 0, None
    while i < len(lines):
        line = lines[i]
        fence, toggled = _fence_toggle(line, fence)
        if toggled or fence is not None:
            i += 1
            continue
        m_up = UPGRADE_RE.match(line)
        m_br = None if m_up else BREAKING_RE.match(line)
        if not (m_up or m_br):
            i += 1
            continue
        kind = "Upgrade:" if m_up else "BREAKING CHANGE:"
        first = (m_up or m_br).group(1)
        i += 1
        cont = []
        while i < len(lines):
            nxt = lines[i]
            if not nxt.strip() or TRAILER_STOP_RE.match(nxt):
                break
            cont.append(nxt.rstrip())
            i += 1
        value = _clean_trailer_value(first, cont)
        if not value:
            if validate:
                raise Refused(f"empty {kind} trailer; explain what operators must do or remove it")
            continue
        (upgrades if m_up else breaking).append(value)
    return upgrades, breaking


def _strip_trailers_outside_fences(lines):
    out = []
    i, fence = 0, None
    while i < len(lines):
        line = lines[i]
        fence, toggled = _fence_toggle(line, fence)
        if toggled or fence is not None:
            out.append(line)
            i += 1
            continue
        if UPGRADE_RE.match(line) or BREAKING_RE.match(line):
            i += 1
            while i < len(lines) and lines[i].strip() and not TRAILER_STOP_RE.match(lines[i]):
                i += 1
            continue
        out.append(line)
        i += 1
    return out


def _extract_release_note_lines(pr_body):
    """Return cleaned lines inside `## Release note` (up to the next Markdown heading), or []."""
    if not pr_body:
        return []
    lines = pr_body.splitlines()
    collected, in_section, fence = [], False, None
    for line in lines:
        fence, toggled = _fence_toggle(line, fence)
        if toggled or fence is not None:
            if in_section:
                collected.append(line)
            continue
        if not in_section:
            if RELEASE_NOTE_HEADING_RE.match(line.strip()):
                in_section = True
            continue
        if NEXT_HEADING_RE.match(line.strip()):
            break
        collected.append(line)
    if not collected:
        return []
    cleaned = _strip_trailers_outside_fences(collected)
    # Strip HTML comments only outside code fences.
    out, fence = [], None
    buf = []

    def flush_buf():
        if buf:
            out.extend(HTML_COMMENT_RE.sub("", "\n".join(buf)).split("\n"))
            buf.clear()

    for line in cleaned:
        fence, toggled = _fence_toggle(line, fence)
        if toggled or fence is not None:
            flush_buf()
            out.append(line)
        else:
            buf.append(line)
    flush_buf()
    return out


def extract_release_note(pr_body):
    """Return the optional `## Release note` section from a PR description, or None."""
    lines = _extract_release_note_lines(pr_body)
    if not lines:
        return None
    result = "\n".join(lines).strip()
    return result or None


def validate_release_note(pr_body):
    """Refuse `Checks:` paragraphs or signature/attribution lines inside `## Release note`."""
    lines = _extract_release_note_lines(pr_body)
    if not lines:
        return
    errors = []
    fence = None
    for line in lines:
        fence, toggled = _fence_toggle(line, fence)
        if toggled or fence is not None:
            continue
        stripped = line.strip()
        if not stripped:
            continue
        if RELEASE_NOTE_CHECKS_RE.match(stripped):
            preview = stripped[:80]
            errors.append(
                f"`## Release note` contains a checks paragraph ({preview!r}); "
                "move check/test summaries into a separate section after `## Release note` (for example `## Checks`)."
            )
        elif RELEASE_NOTE_SIGNATURE_RE.match(stripped):
            preview = stripped[:80]
            errors.append(
                f"`## Release note` contains a signature/attribution line ({preview!r}); "
                "move it after a separate heading (for example `## Checks`) because `## Release note` runs until the next Markdown heading."
            )
    if errors:
        raise Refused("\n".join(errors))


def dedupe_texts(items):
    seen, out = set(), []
    for item in items:
        key = " ".join(item.split())
        if key and key not in seen:
            seen.add(key)
            out.append(item)
    return tuple(out)


def origin_web_url(repo):
    """Best-effort repository web URL derived from origin or $GH_REPO, else None."""
    if gh_repo := os.environ.get("GH_REPO", "").strip().strip("/"):
        return f"https://github.com/{gh_repo}"
    r = git("remote", "get-url", "origin", cwd=repo, check=False)
    if r.returncode != 0:
        return None
    url = r.stdout.strip()
    if m := re.match(r"^https?://(.+?)(?:\.git)?/?$", url):
        scheme = "https" if url.startswith("https://") else "http"
        return f"{scheme}://{m.group(1)}"
    if m := re.match(r"^ssh://(?:[^@]+@)?([^/:]+)(?::\d+)?/(.+?)(?:\.git)?/?$", url):
        return f"https://{m.group(1)}/{m.group(2)}"
    if m := re.match(r"^(?:[^@]+@)?([^/:]+):(.+?)(?:\.git)?/?$", url):
        return f"https://{m.group(1)}/{m.group(2)}"
    return None


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


def local_versions(repo, ref=None):
    args = ["tag", "--list", "v[0-9]*"]
    if ref:
        args += ["--merged", ref]
    r = git(*args, cwd=repo, check=False)
    if r.returncode != 0:
        return set()
    out = set()
    for line in r.stdout.splitlines():
        if m := VERSION_RE.match(line.strip()):
            out.add(tuple(int(x) for x in m.groups()))
    return out


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
        raise Refused(
            f"can't reach GitHub: no `gh` on PATH, in {', '.join(GH_PLACES)} or $STEVE_GH "
            f"(install it and log in)"
        )
    try:
        r = subprocess.run(
            [exe, *args],
            cwd=str(cwd),
            capture_output=True,
            text=True,
            timeout=NET_TIMEOUT,
            env=dict(os.environ, GH_PROMPT_DISABLED="1", NO_COLOR="1"),
        )
    except FileNotFoundError:
        raise Refused(f"can't reach GitHub: no `gh` at {exe} (install it and log in)")
    except subprocess.TimeoutExpired:
        raise Refused(f"gh {args[0]} timed out after {NET_TIMEOUT}s")
    if r.returncode != 0:
        what = "read from GitHub" if args[:1] == ("api",) else "ask GitHub"
        raise Refused(f"can't {what}: gh {' '.join(args[:2])}: {(r.stderr or r.stdout).strip()[:300]}")
    return r.stdout


def api(repo, path):
    out = gh("api", f"repos/{{owner}}/{{repo}}/{path}", cwd=repo)
    try:
        return json.loads(out)
    except ValueError:
        raise Refused(f"can't read from GitHub: unexpected answer {out.strip()[:200]!r}")


def _parse_iso_ts(value):
    if not isinstance(value, str) or not value:
        return None
    normalized = f"{value[:-1]}+00:00" if value.endswith("Z") else value
    try:
        dt = datetime.fromisoformat(normalized)
    except ValueError:
        return None
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.timestamp()


def graphql(repo, query, variables=None):
    args = ["api", "graphql", "-f", f"query={query}"]
    for key, val in (variables or {}).items():
        if val is None:
            continue
        if isinstance(val, str) and val in ("{owner}", "{repo}", "{branch}"):
            args += ["-F", f"{key}={val}"]
        elif isinstance(val, str):
            args += ["-f", f"{key}={val}"]
        else:
            args += ["-F", f"{key}={json.dumps(val)}"]
    out = gh(*args, cwd=repo)
    try:
        payload = json.loads(out)
    except ValueError:
        raise Refused(f"can't read from GitHub: unexpected GraphQL answer {out.strip()[:200]!r}")
    if not isinstance(payload, dict):
        raise Refused("can't read from GitHub: unexpected GraphQL payload")
    if errors := payload.get("errors"):
        items = errors if isinstance(errors, list) else [errors]
        msg = "; ".join(e.get("message", str(e)) if isinstance(e, dict) else str(e) for e in items)
        raise Refused(f"can't read from GitHub: GraphQL error: {msg[:300]}")
    data = payload.get("data")
    if not isinstance(data, dict):
        raise Refused("can't read from GitHub: unexpected GraphQL payload")
    return data


def _parse_merged_pr_node(node):
    if not isinstance(node, dict):
        return None
    number = node.get("number")
    title = node.get("title")
    if not isinstance(number, int) or isinstance(number, bool) or not isinstance(title, str):
        raise Refused(f"can't read merged PRs from GitHub: unexpected PR node {node!r}"[:300])
    body = node.get("body")
    if body is not None and not isinstance(body, str):
        raise Refused(f"can't read merged PRs from GitHub: unexpected body on PR #{number}")
    merge_commit = node.get("mergeCommit")
    merge_sha = (
        merge_commit.get("oid")
        if isinstance(merge_commit, dict) and isinstance(merge_commit.get("oid"), str)
        else None
    )
    head_ref_oid = node.get("headRefOid") if isinstance(node.get("headRefOid"), str) else None
    merged_at = node.get("mergedAt") if isinstance(node.get("mergedAt"), str) else None
    updated_at = node.get("updatedAt") if isinstance(node.get("updatedAt"), str) else None
    return MergedPr(
        number=number,
        title=title,
        body=body or "",
        merge_sha=merge_sha,
        head_ref_oid=head_ref_oid,
        merged_at=merged_at,
        updated_at=updated_at,
    )


def fetch_merged_prs_from_github(repo, since_ts):
    """Fetch merged PRs updated on or after `since_ts` (Unix seconds) via GitHub GraphQL.

    GitHub guarantees `updatedAt >= mergedAt`, so paginating `MERGED` PRs in `UPDATED_AT DESC`
    order and stopping once a page's last node has `updatedAt < since_ts` covers every PR merged
    since `since_ts` with no search indexing lag and no 1,000-result search cap.
    """
    prs: list[MergedPr] = []
    seen: set[int] = set()
    cursor: str | None = None
    while True:
        try:
            data = graphql(
                repo,
                MERGED_PRS_GRAPHQL,
                {"owner": "{owner}", "repo": "{repo}", "endCursor": cursor},
            )
        except Refused as e:
            raise Refused(f"can't read merged PRs from GitHub: {e}")
        repo_data = data.get("repository")
        pr_conn = repo_data.get("pullRequests") if isinstance(repo_data, dict) else None
        nodes = pr_conn.get("nodes") if isinstance(pr_conn, dict) else None
        page_info = pr_conn.get("pageInfo") if isinstance(pr_conn, dict) else None
        if not isinstance(nodes, list) or not isinstance(page_info, dict):
            raise Refused("can't read merged PRs from GitHub: unexpected GraphQL payload")
        last_updated_ts = None
        for node in nodes:
            pr = _parse_merged_pr_node(node)
            if pr is None:
                continue
            ts = _parse_iso_ts(pr.updated_at or pr.merged_at)
            if ts is not None:
                last_updated_ts = ts
            if pr.number not in seen:
                seen.add(pr.number)
                prs.append(pr)
        if not page_info.get("hasNextPage"):
            break
        if last_updated_ts is not None and last_updated_ts < since_ts:
            break
        next_cursor = page_info.get("endCursor")
        if not isinstance(next_cursor, str) or not next_cursor or next_cursor == cursor:
            break
        cursor = next_cursor
    return prs


def github_merged_prs_fetcher(repo, *, offline):
    """Return a `(since_ts) -> list[MergedPr] | None` callable for live GitHub merged PR metadata.

    Always reads live PR metadata from GitHub unless `--offline` is explicitly set,
    so PR titles and descriptions edited after merge take effect immediately.
    """
    warned_offline = False

    def fetch(since_ts):
        nonlocal warned_offline
        if offline:
            if not warned_offline:
                print(
                    "note: --offline: using commit messages only (GitHub PR titles and descriptions skipped)",
                    file=sys.stderr,
                )
                warned_offline = True
            return None
        return fetch_merged_prs_from_github(repo, since_ts)

    return fetch


def commit_meta(repo, sha):
    raw = git("show", "-s", "--format=%P%x00%s%x00%b", sha, cwd=repo).stdout
    parts = raw.split("\x00", 2)
    parents = parts[0].split() if parts[0] else []
    subject = parts[1].strip() if len(parts) > 1 else ""
    body = parts[2] if len(parts) > 2 else ""
    return parents, subject, body


def commit_bodies_in_pr(repo, parent1, parent2):
    raw = git("log", "--no-merges", "--reverse", "--format=%B%x00", f"{parent1}..{parent2}", cwd=repo).stdout
    return [chunk for chunk in raw.split("\x00") if chunk.strip()]


def _exclude_args(from_ref):
    return ["--not", from_ref] if from_ref else ["--not", "--glob=refs/tags/v[0-9]*"]


def _is_already_released_merge(repo, parents, exclude):
    """True when a merge commit's second parent brings zero commits outside `exclude`
    (for example, the merge commit of a prior `release/vX.Y.Z` pull request whose second parent is tagged)."""
    if len(parents) < 2:
        return False
    return not git("rev-list", "-n", "1", parents[1], *exclude, cwd=repo).stdout.strip()


def first_parent_commits(repo, to_ref, from_ref=None):
    """Chronological first-parent commits in the release range, skipping already-released merges."""
    exclude = _exclude_args(from_ref)
    shas = git("rev-list", "--first-parent", "--reverse", to_ref, *exclude, cwd=repo).stdout.split()
    kept = []
    for sha in shas:
        parents, _, _ = commit_meta(repo, sha)
        if _is_already_released_merge(repo, parents, exclude):
            continue
        kept.append(sha)
    return kept


def _range_commits(repo, to_ref, exclude):
    """Return CommitRecord items in topological reverse order for the release range."""
    raw = git(
        "log", "-z", "--topo-order", "--reverse", "--format=%H%n%P%n%ct%n%s%n%b", to_ref, *exclude, cwd=repo
    ).stdout
    records: list[CommitRecord] = []
    for chunk in raw.split("\x00"):
        if not chunk:
            continue
        parts = chunk.split("\n", 4)
        sha = parts[0].strip()
        if not sha:
            continue
        parents = tuple(parts[1].split()) if len(parts) > 1 and parts[1].strip() else ()
        committer_ts = int(parts[2].strip()) if len(parts) > 2 and parts[2].strip() else 0
        subject = parts[3].strip() if len(parts) > 3 else ""
        body = parts[4] if len(parts) > 4 else ""
        records.append(CommitRecord(sha=sha, parents=parents, committer_ts=committer_ts, subject=subject, body=body))
    return records


def _pick_carrier_pr(group, merge_record):
    """Identify the carrier PR among multiple PRs sharing the same merge commit.

    The carrier is the PR whose `headRefOid` matches the merge commit's merged-branch parent
    (`parents[1]`). If multiple PRs match (for example, a fast-forward integration branch) or
    none match (a single-parent squash commit), break ties with the PR number named in the
    merge commit subject (`Merge pull request #N from ...`), then the highest PR number.
    """
    merged_head = merge_record.parents[1] if len(merge_record.parents) >= 2 else None
    m_pr = PR_MERGE_RE.match(merge_record.subject)
    subject_num = int(m_pr.group(1)) if m_pr else None
    return max(
        group,
        key=lambda pr: (
            1 if (merged_head and pr.head_ref_oid == merged_head) else 0,
            1 if (subject_num is not None and pr.number == subject_num) else 0,
            pr.number,
        ),
    )


def _pr_release_entry(sha, pr_number, raw_title, pr_body, *, carried_prs=()):
    release_note = extract_release_note(pr_body)
    pr_upgrades, pr_breaking = parse_trailers(pr_body)
    upgrades_t = dedupe_texts(pr_upgrades)
    breaking_t = dedupe_texts(pr_breaking)
    parsed = parse_header(raw_title)
    return ReleaseEntry(
        sha=sha,
        pr_number=pr_number,
        raw_title=raw_title,
        commit_type=parsed.commit_type if parsed else None,
        scope=parsed.scope if parsed else None,
        breaking=bool((parsed and parsed.breaking) or breaking_t),
        description=parsed.description if parsed else raw_title,
        release_note=release_note,
        upgrades=upgrades_t,
        breaking_notes=breaking_t,
        carried_prs=tuple(carried_prs),
    )


def _direct_commit_entry(rec):
    raw_title = rec.subject
    upgrades, breaking_notes = parse_trailers(rec.body)
    upgrades_t = dedupe_texts(upgrades)
    breaking_t = dedupe_texts(breaking_notes)
    parsed = parse_header(raw_title)
    return ReleaseEntry(
        sha=rec.sha,
        pr_number=None,
        raw_title=raw_title,
        commit_type=parsed.commit_type if parsed else None,
        scope=parsed.scope if parsed else None,
        breaking=bool((parsed and parsed.breaking) or breaking_t),
        description=parsed.description if parsed else raw_title,
        release_note=None,
        upgrades=upgrades_t,
        breaking_notes=breaking_t,
    )


def _cover_merge_branch(repo, rec, exclude, covered):
    covered.add(rec.sha)
    if len(rec.parents) >= 2:
        branch_shas = git(
            "rev-list", rec.parents[1], f"^{rec.parents[0]}", *exclude, cwd=repo
        ).stdout.split()
        covered.update(branch_shas)


def collect_entries(repo, to_ref, from_ref=None, *, fetch_merged_prs=None):
    """Build ReleaseEntry items from the release range, resolving merged PRs deterministically.

    Online mode fetches merged PRs from GitHub GraphQL over the range's time window and filters
    them to PRs whose `mergeCommit.oid` is in the release range (ancestor of `to_ref`, outside
    `exclude`, and not an already-released merge such as a previous release PR). When multiple
    PRs share one `mergeCommit` (an integration batch PR plus its carried sub-PRs), the carrier
    is the PR whose `headRefOid` matches the merge commit's merged-branch parent, and each sub-PR
    becomes an independent entry. PR entries take `Upgrade:` and `BREAKING CHANGE:` strictly from
    their PR descriptions (never falling back to commit trailers); direct commits without a PR
    read their own commit trailers.
    """
    exclude = _exclude_args(from_ref)
    records = _range_commits(repo, to_ref, exclude)
    if not records:
        return []

    pos = {rec.sha: i for i, rec in enumerate(records)}
    record_by_sha = {rec.sha: rec for rec in records}
    skipped_merges: set[str] = set()
    git_pr_merges: list[tuple[CommitRecord, int]] = []

    for rec in records:
        if len(rec.parents) < 2:
            continue
        if _is_already_released_merge(repo, rec.parents, exclude):
            skipped_merges.add(rec.sha)
            continue
        if m_pr := PR_MERGE_RE.match(rec.subject):
            git_pr_merges.append((rec, int(m_pr.group(1))))

    # `valid_range_shas` is the exact set of commits reachable from `to_ref` and outside `exclude`
    # (equivalent to `git merge-base --is-ancestor <sha> <to_ref>` and not reachable from `exclude`),
    # minus already-released merge commits such as a prior `release/vX.Y.Z` merge.
    valid_range_shas = set(pos) - skipped_merges
    if not valid_range_shas:
        return []

    since_ts = min(record_by_sha[sha].committer_ts for sha in valid_range_shas) - MERGED_WINDOW_PAD_SECONDS
    github_prs = fetch_merged_prs(since_ts) if fetch_merged_prs is not None else None

    covered: set[str] = set()
    sortable: list[tuple[tuple[int, int, int], ReleaseEntry]] = []

    if github_prs is None:
        # --offline (or caller omitted fetcher): infer PRs solely from local git PR merge commits.
        for rec, pr_num in git_pr_merges:
            _cover_merge_branch(repo, rec, exclude, covered)
            fallback_title = next((l.strip() for l in rec.body.splitlines() if l.strip()), rec.subject)
            entry = _pr_release_entry(rec.sha, pr_num, fallback_title, "")
            sortable.append(((pos[rec.sha], 1, pr_num), entry))
    else:
        by_merge_sha: dict[str, list[MergedPr]] = {}
        found_pr_numbers: set[int] = set()
        for pr in github_prs:
            if not pr.merge_sha or pr.merge_sha not in valid_range_shas:
                continue
            by_merge_sha.setdefault(pr.merge_sha, []).append(pr)
            found_pr_numbers.add(pr.number)

        for rec, pr_num in git_pr_merges:
            if pr_num not in found_pr_numbers:
                raise Refused(
                    f"can't read PR #{pr_num} ({rec.sha[:7]}) from GitHub: merged PR not found in GitHub GraphQL response"
                )

        for merge_sha, group in by_merge_sha.items():
            rec = record_by_sha[merge_sha]
            _cover_merge_branch(repo, rec, exclude, covered)
            if len(group) == 1:
                pr = group[0]
                entry = _pr_release_entry(merge_sha, pr.number, pr.title.strip(), pr.body)
                sortable.append(((pos[merge_sha], 1, pr.number), entry))
            else:
                carrier = _pick_carrier_pr(group, rec)
                sub_prs = sorted((p for p in group if p.number != carrier.number), key=lambda p: p.number)
                for sub in sub_prs:
                    sub_entry = _pr_release_entry(merge_sha, sub.number, sub.title.strip(), sub.body)
                    sortable.append(((pos[merge_sha], 0, sub.number), sub_entry))
                carrier_entry = _pr_release_entry(
                    merge_sha,
                    carrier.number,
                    carrier.title.strip(),
                    carrier.body,
                    carried_prs=tuple(p.number for p in sub_prs),
                )
                sortable.append(((pos[merge_sha], 1, carrier.number), carrier_entry))

    for rec in records:
        if len(rec.parents) >= 2 or rec.sha in covered:
            continue
        sortable.append(((pos[rec.sha], 0, 0), _direct_commit_entry(rec)))

    sortable.sort(key=lambda item: item[0])
    return [entry for _, entry in sortable]


def validate_entries(entries):
    """Refuse to compute a version or generate notes when any entry fails Conventional Commits."""
    bad = [e for e in entries if e.commit_type is None]
    if not bad:
        return
    allowed = "|".join(ALLOWED_TYPES)
    lines = [
        f"{len(bad)} commit(s)/PR(s) do not follow Conventional Commits "
        f"(`type[(scope)][!]: imperative summary`, type in {allowed}):"
    ]
    has_pr = False
    for e in bad:
        if e.pr_number is not None:
            has_pr = True
            lines.append(f"  - PR #{e.pr_number} ({e.sha[:7]}): {e.raw_title!r}")
        else:
            lines.append(f"  - commit {e.sha[:7]}: {e.raw_title!r}")
    if has_pr:
        lines.append(
            "Update the PR title on GitHub (and add any `Upgrade:` trailer or `## Release note` section "
            "to the PR description), then re-run."
        )
    else:
        lines.append("Fix the commit message to follow Conventional Commits, then re-run.")
    raise Refused("\n".join(lines))


def compute_version(current, entries):
    """Compute the next version and explain which entries triggered any MINOR or MAJOR bump."""
    if current is None:
        return VersionPlan("0.1.0", "initial", None, ())
    major, minor, patch = current
    prev = f"v{major}.{minor}.{patch}"
    if any(e.breaking for e in entries):
        if major >= 1:
            triggers = tuple((e, ("breaking",)) for e in entries if e.breaking)
            return VersionPlan(f"{major + 1}.0.0", "major", prev, triggers)
        triggers = []
        for e in entries:
            reasons = []
            if e.breaking:
                reasons.append("breaking (pre-1.0 -> MINOR)")
            if e.upgrades:
                reasons.append("Upgrade:")
            if e.commit_type == "feat":
                reasons.append("feat")
            if reasons:
                triggers.append((e, tuple(reasons)))
        return VersionPlan(f"0.{minor + 1}.0", "minor", prev, tuple(triggers))
    triggers = []
    for e in entries:
        reasons = []
        if e.commit_type == "feat":
            reasons.append("feat")
        if e.upgrades:
            reasons.append("Upgrade:")
        if reasons:
            triggers.append((e, tuple(reasons)))
    if triggers:
        return VersionPlan(f"{major}.{minor + 1}.0", "minor", prev, tuple(triggers))
    return VersionPlan(f"{major}.{minor}.{patch + 1}", "patch", prev, ())


def _pr_ref(number, web_url):
    return f"[#{number}]({web_url}/pull/{number})" if web_url else f"#{number}"


def _entry_ref(entry, web_url):
    if entry.pr_number is not None:
        return _pr_ref(entry.pr_number, web_url)
    short = entry.sha[:7]
    return f"[`{short}`]({web_url}/commit/{entry.sha})" if web_url else f"`{short}`"


def _format_upgrade_bullet(text, ref):
    lines = text.splitlines()
    if len(lines) == 1:
        return f"- {lines[0]} ({ref})"
    if lines[0].lstrip().startswith(("- ", "* ")):
        indented = "\n".join(f"  {l}" if l.strip() else "" for l in lines)
        return f"- ({ref})\n{indented}"
    rest = "\n".join(f"  {l}" if l.strip() else "" for l in lines[1:])
    return f"- {lines[0]} ({ref})\n{rest}"


def _format_entry_bullet(entry, web_url, *, include_type):
    ref = _entry_ref(entry, web_url)
    if entry.carried_prs:
        carried = ", ".join(_pr_ref(n, web_url) for n in entry.carried_prs)
        ref = f"{ref}; includes {carried}"
    if include_type and entry.commit_type:
        label = f"{entry.commit_type}({entry.scope})" if entry.scope else entry.commit_type
        head = f"**{label}:** {entry.description} ({ref})"
    elif entry.scope:
        head = f"**{entry.scope}:** {entry.description} ({ref})"
    else:
        head = f"{entry.description} ({ref})"
    if not entry.release_note or (entry.carried_prs and entry.commit_type == "chore"):
        return f"- {head}"
    body_lines = "\n".join(f"  {l}" if l.strip() else "" for l in entry.release_note.splitlines())
    return f"- {head}\n\n{body_lines}"


def render_notes(version, entries, web_url, *, manual_reason=None):
    """Render complete Markdown release notes covering every entry in the range."""
    sections = [f"# Steve v{version}"]
    if manual_reason:
        sections.append(f"> Version set manually (`--version {version}`): {manual_reason}")

    before_upgrading = []
    seen_norm = set()
    for entry in entries:
        ref = _entry_ref(entry, web_url)
        entry_has_notice = False
        for up in entry.upgrades:
            entry_has_notice = True
            norm = " ".join(up.split())
            if norm not in seen_norm:
                seen_norm.add(norm)
                before_upgrading.append(_format_upgrade_bullet(up, ref))
        for br in entry.breaking_notes:
            entry_has_notice = True
            norm = " ".join(br.split())
            if norm not in seen_norm:
                seen_norm.add(norm)
                before_upgrading.append(_format_upgrade_bullet(f"**Breaking:** {br}", ref))
        if entry.breaking and not entry_has_notice:
            norm = " ".join(entry.description.split())
            if norm not in seen_norm:
                seen_norm.add(norm)
                before_upgrading.append(_format_upgrade_bullet(f"**Breaking:** {entry.description}", ref))

    if before_upgrading:
        sections.append("## Before upgrading\n\n" + "\n".join(before_upgrading))

    buckets = {"feat": [], "fix": [], "perf": [], "other": []}
    for entry in entries:
        if entry.commit_type in ("feat", "fix", "perf"):
            buckets[entry.commit_type].append(_format_entry_bullet(entry, web_url, include_type=False))
        else:
            buckets["other"].append(_format_entry_bullet(entry, web_url, include_type=True))

    for key, heading in (
        ("feat", "## Features"),
        ("fix", "## Fixes"),
        ("perf", "## Performance"),
        ("other", "## Other changes"),
    ):
        if buckets[key]:
            sections.append(f"{heading}\n\n" + "\n".join(buckets[key]))

    return "\n\n".join(sections) + "\n"


def format_next(plan, entry_count):
    if plan.previous is None:
        return f"{plan.version} (initial)\nNo previous version tag found.\n"
    lines = [f"{plan.version} ({plan.bump}, from {plan.previous})"]
    if entry_count == 0:
        lines.append(f"No new commits since {plan.previous}.")
    elif not plan.triggers:
        lines.append(f"No feat, Upgrade: trailer, or breaking change in {entry_count} commit(s) since {plan.previous}.")
    else:
        lines.append("Triggered by:")
        for entry, reasons in plan.triggers:
            tag = f"#{entry.pr_number}" if entry.pr_number is not None else entry.sha[:7]
            lines.append(f"- {tag} [{', '.join(reasons)}]: {entry.raw_title}")
    return "\n".join(lines) + "\n"


def parse_range(spec):
    if ".." not in spec or "..." in spec:
        raise Refused(f"--range {spec!r}: expected FROM..TO")
    from_ref, to_ref = spec.split("..", 1)
    if not from_ref.strip() or not to_ref.strip():
        raise Refused(f"--range {spec!r}: expected FROM..TO")
    return from_ref.strip(), to_ref.strip()


def base_version_for_range(repo, from_ref):
    if m := VERSION_RE.match(from_ref):
        return tuple(int(x) for x in m.groups())
    tags = local_versions(repo, from_ref)
    return max(tags) if tags else None


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
    return newest([
        r for r in ci_runs(repo, sha, "push", f"&branch={branch}")
        if r.get("head_branch") in (None, branch)
    ])


def rerun_hint(run):
    return (
        f"If it was a flake, run its failed jobs again: gh run rerun {run.get('id')} --failed "
        f"(then cut again once it passes); otherwise fix it on main."
    )


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
                raise Refused(
                    f"CI ({CI_WORKFLOW}) has no push run on {short}: only a tested commit is released "
                    f"(push it to the branch and wait for CI, or --skip-ci to release untested)"
                )
            state = "no run yet"
        elif run.get("status") == "completed":
            conclusion = run.get("conclusion")
            if conclusion == "success":
                print(f"CI passed on {short}: {run.get('html_url', run.get('id'))}", file=sys.stderr)
                return run
            if conclusion == "cancelled":
                raise Refused(
                    f"CI ({CI_WORKFLOW}) cancelled on {short}: {run.get('html_url', run.get('id'))}. "
                    "No successful push run verifies this commit; cut again once a tested commit is available."
                )
            raise Refused(
                f"CI ({CI_WORKFLOW}) {conclusion} on {short}: {run.get('html_url', run.get('id'))}. "
                + rerun_hint(run)
            )
        else:
            state = run.get("status") or "pending"
        if waited >= wait:
            where = f": {run.get('html_url', run.get('id'))}" if run else ""
            raise Refused(
                f"CI ({CI_WORKFLOW}) on {short} is still {state} after {int(waited)}s{where}; "
                f"cut again once it has passed"
            )
        if state != said:
            print(f"waiting for CI on {short} ({state})", file=sys.stderr)
            said = state
        time.sleep(CI_POLL)


def release_base(repo, head, branch, wait):
    """The newest first-parent commit since the last release that has new changes and whose own
    push run passed, else the head once its run passes. Cancelled and PR-only runs never count."""
    exclude = _exclude_args(None)
    # Chronological list of first-parent commits since the last tag, with a flag for non-release merges.
    chron = git("rev-list", "--first-parent", "--reverse", head, *exclude, cwd=repo).stdout.split()
    releasable = []
    seen_real = False
    for sha in chron:
        parents, _, _ = commit_meta(repo, sha)
        if not _is_already_released_merge(repo, parents, exclude):
            seen_real = True
        if seen_real:
            releasable.append(sha)
    # Walk newest-first in branch order (never completion order).
    for sha in reversed(releasable):
        run = ci_run(repo, sha, branch)
        if run and run.get("status") == "completed" and run.get("conclusion") == "success":
            print(f"CI passed on {sha[:7]}: {run.get('html_url', run.get('id'))}", file=sys.stderr)
            if sha != head:
                print(
                    f"releasing tested commit {sha[:7]}; newer commits on {branch} wait for the next release",
                    file=sys.stderr,
                )
            return sha
    ci_gate(repo, head, branch, wait)
    return head


def left_behind(wt, have_versions):
    """Return `(version, commit_sha)` for an untagged `chore(release): vX.Y.Z` notes commit already on
    the branch checked out in `wt`, or None."""
    notes_dir = Path(wt, NOTES)
    if not notes_dir.is_dir():
        return None
    candidates = []
    for path in notes_dir.iterdir():
        if not path.is_file():
            continue
        rel = f"{NOTES}/{path.name}"
        m = NOTES_RE.match(rel)
        if not m:
            continue
        ver_tuple = tuple(int(x) for x in m.groups())
        if ver_tuple not in have_versions:
            candidates.append((ver_tuple, format_version(ver_tuple), rel))
    for _, version, rel in sorted(candidates):
        c = git("log", "-1", "--format=%H", "--diff-filter=A", "--", rel, cwd=wt).stdout.strip()
        if not c or git("log", "-1", "--format=%s", c, cwd=wt).stdout.strip() != f"chore(release): v{version}":
            continue
        if len(git("rev-list", "--parents", "-n", "1", c, cwd=wt).stdout.split()) != 2:
            continue
        changes = git("diff", "--name-status", "--no-renames", f"{c}^", c, cwd=wt).stdout.splitlines()
        if changes == [f"A\t{rel}"]:
            return version, c
    return None


def taken(repo, version, version_arg):
    if parse_version(version) in remote_versions(repo):
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
        r = subprocess.run(
            ["git", "-C", str(wt), "push", "--porcelain", "origin", *refspecs],
            capture_output=True,
            text=True,
            timeout=NET_TIMEOUT,
            env=dict(os.environ, GIT_TERMINAL_PROMPT="0"),
        )
    except subprocess.TimeoutExpired:
        raise Refused(f"pushing {refspecs[-1]} timed out")
    if r.returncode != 0:
        if "[rejected]" in r.stdout or "[remote rejected]" in r.stdout:
            raise Raced((r.stdout + r.stderr).strip()[:300])
        raise Refused(f"push failed: {(r.stderr or r.stdout).strip()[:300]}")


def release_pr(repo, wt, branch, version, text, ci_wait):
    """Bring the release notes commit in `wt` into `branch` through a pull request merged with a merge
    commit, and return the notes commit itself. Anything short of the merge closes the PR and deletes its branch."""
    rb = f"release/v{version}"
    sha = git("rev-parse", "HEAD", cwd=wt).stdout.strip()
    if remote_ref(repo, f"refs/heads/{rb}"):
        raise Refused(
            f"{rb} is already on origin: another release of v{version} is in progress, or one was "
            f"left behind (delete it with `git push origin --delete {rb}` once nobody is releasing)"
        )
    push(wt, f"HEAD:refs/heads/{rb}")
    number = None
    try:
        url = (
            gh(
                "pr",
                "create",
                "--base",
                branch,
                "--head",
                rb,
                "--title",
                f"chore(release): v{version}",
                "--body",
                f"Release notes of v{version}, opened by `ops/cut-release.py cut`, which merges it and "
                f"tags this pull request's commit.\n\n{text}",
                cwd=repo,
            )
            .strip()
            .splitlines()[-1]
        )
        m = re.search(r"/pull/(\d+)$", url)
        if not m:
            raise Refused(f"can't tell the release pull request from {url!r}")
        number = m.group(1)
        print(f"release pull request #{number}: {url}", file=sys.stderr)
        if ci_wait is not None:
            notes_ci(repo, sha, f"release pull request #{number}", ci_wait)
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
    """Return once the CI workflow passed on the notes commit `sha`."""
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
            raise Refused(
                f"CI ({CI_WORKFLOW}) {run.get('conclusion')} on {what}: "
                f"{run.get('html_url', run.get('id'))}. "
                + (
                    "It is closed; cut again once the cause is fixed (a flake: just cut again)"
                    if open_pr
                    else rerun_hint(run)
                )
            )
        state = "no run yet" if run is None else run.get("status") or "pending"
        if waited >= (min(wait, CI_APPEAR) if run is None else wait):
            raise Refused(
                f"CI ({CI_WORKFLOW}) on {what} is still {state} after {int(waited)}s; "
                + ("it is closed, cut again" if open_pr else "cut again once it has passed")
            )
        if state != said:
            print(f"waiting for CI on {what} ({state})", file=sys.stderr)
            said = state
        time.sleep(CI_POLL)


def merge(repo, number, sha):
    """Merge the release pull request at `sha` with a merge commit, and return that commit."""
    for attempt in range(1, MERGE_TRIES + 1):
        try:
            gh("pr", "merge", number, "--merge", "--match-head-commit", sha, cwd=repo)
            break
        except Refused as e:
            why = str(e)
            if re.search(r"base branch was modified", why, re.I) and attempt < MERGE_TRIES:
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
                raise Refused(
                    f"merging release pull request #{number}: {why}. `cut` merges it with a merge commit "
                    f"while the branch moves on, so the branch rules must not require pull requests to "
                    f"be up to date before merging"
                )
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
    """Tag `target` `vX.Y.Z` and push only the tag, once origin's `branch` contains `target`."""
    name = f"v{version}"
    for attempt in range(3):  # a merge GitHub reported may take a moment to show on the branch
        if attempt:
            time.sleep(3)
        git("fetch", "--quiet", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
        if (
            git("merge-base", "--is-ancestor", target, f"refs/remotes/origin/{branch}", cwd=repo, check=False).returncode
            == 0
        ):
            break
    else:
        raise Refused(f"{target[:7]} is not on origin's {branch}: v{version} is not tagged (cut again once it is)")
    git("tag", "-a", name, "-m", f"Steve {name}", target, cwd=repo)
    try:
        push(wt, f"refs/tags/{name}")
    except BaseException:
        if not landed(repo, version):
            git("tag", "-d", name, cwd=repo, check=False)
            raise


def landed(repo, version):
    try:
        theirs = remote_ref(repo, f"refs/tags/v{version}")
    except Refused:
        return False
    return bool(theirs) and theirs == git("rev-parse", f"refs/tags/v{version}", cwd=repo, check=False).stdout.strip()


def fetched(repo, version):
    try:
        git("fetch", "--quiet", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT)
    except Refused:
        pass
    return version


def cut_range_preview(repo, range_spec, version_arg, reason, offline):
    from_ref, to_ref = parse_range(range_spec)
    git("rev-parse", "--verify", f"{from_ref}^{{commit}}", cwd=repo)
    to_sha = git("rev-parse", "--verify", f"{to_ref}^{{commit}}", cwd=repo).stdout.strip()
    fetch_merged_prs = github_merged_prs_fetcher(repo, offline=offline)
    entries = collect_entries(repo, to_sha, from_ref, fetch_merged_prs=fetch_merged_prs)
    if not entries:
        raise Refused(f"nothing to release in {from_ref}..{to_ref}")
    validate_entries(entries)
    plan = compute_version(base_version_for_range(repo, from_ref), entries)
    version = version_arg or plan.version
    text = render_notes(version, entries, origin_web_url(repo), manual_reason=reason if version_arg else None)
    print(text, end="")
    return version


def cut_once(repo, branch, version_arg, reason, dry, ci_wait=CI_WAIT, offline=False):
    if not dry and offline:
        raise Refused("--offline is only allowed with --dry-run")
    if offline:
        have = local_versions(repo)
    else:
        have = remote_versions(repo)
        if git("fetch", "--quiet", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT).returncode != 0:
            raise Refused("can't fetch origin")
    head = git("rev-parse", f"refs/remotes/origin/{branch}^{{commit}}", cwd=repo).stdout.strip()
    if version_arg and not offline:
        taken(repo, version_arg, version_arg)
    with tempfile.TemporaryDirectory(prefix="steve-cut-") as tmp:
        wt = Path(tmp, "wt")
        git("worktree", "add", "--quiet", "--detach", str(wt), head, cwd=repo)
        try:
            if recovered := left_behind(wt, have):
                rec_version, left = recovered
                notes_rel = f"{NOTES}/{rec_version}.md"
                print(
                    f"v{rec_version}'s notes commit {left[:7]} is on {branch} without its tag: tagging it",
                    file=sys.stderr,
                )
                if dry:
                    print(git("show", f"{left}:{notes_rel}", cwd=wt).stdout, end="")
                    return rec_version
                if ci_wait is not None:
                    ci_gate(repo, git("rev-parse", f"{left}^", cwd=wt).stdout.strip(), branch, ci_wait)
                    notes_ci(repo, left, f"notes commit {left[:7]}", ci_wait, branch)
                tag(repo, wt, left, rec_version, branch)
                return fetched(repo, rec_version)

            if not first_parent_commits(wt, head):
                prev = f"v{format_version(max(have))}" if have else "the start of history"
                raise Refused(f"nothing to release: no commits on {branch} since {prev}")

            base = head
            if not dry and ci_wait is not None:
                base = release_base(repo, head, branch, ci_wait)
                if base != head:
                    git("checkout", "--quiet", "--detach", base, cwd=wt)

            fetch_merged_prs = github_merged_prs_fetcher(repo, offline=offline)
            entries = collect_entries(wt, base, fetch_merged_prs=fetch_merged_prs)
            if not entries:
                raise Refused(
                    f"nothing to release at the newest tested commit {base[:7]}: newer commits on {branch} are still untested"
                )
            validate_entries(entries)

            current = max(have) if have else None
            plan = compute_version(current, entries)
            version = version_arg or plan.version
            if not offline:
                taken(repo, version, version_arg)
            text = render_notes(
                version, entries, origin_web_url(repo), manual_reason=reason if version_arg else None
            )
            if dry:
                print(text, end="")
                return version

            notes_rel = f"{NOTES}/{version}.md"
            Path(wt, notes_rel).parent.mkdir(parents=True, exist_ok=True)
            Path(wt, notes_rel).write_text(text)
            git("add", "--", notes_rel, cwd=wt)
            git("commit", "--quiet", "-m", f"chore(release): v{version}", cwd=wt)
            target = release_pr(repo, wt, branch, version, text, ci_wait)
            tag(repo, wt, target, version, branch)
        finally:
            git("worktree", "remove", "--force", str(wt), cwd=repo, check=False)
    return fetched(repo, version)


def cut(repo, branch, version_arg, reason, dry, ci_wait=CI_WAIT, offline=False):
    """`ci_wait` None: no CI check (--skip-ci)."""
    for attempt in range(1, ATTEMPTS + 1):
        try:
            return cut_once(repo, branch, version_arg, reason, dry, ci_wait, offline=offline)
        except Raced as e:
            if version_arg or attempt == ATTEMPTS:
                raise Refused(f"origin moved while releasing: {e}")
            print(f"origin moved while releasing ({e}); again from the new state", file=sys.stderr)


def cmd_next(repo, branch, range_spec, offline):
    if range_spec:
        from_ref, to_ref = parse_range(range_spec)
        git("rev-parse", "--verify", f"{from_ref}^{{commit}}", cwd=repo)
        to_sha = git("rev-parse", "--verify", f"{to_ref}^{{commit}}", cwd=repo).stdout.strip()
        current = base_version_for_range(repo, from_ref)
        fetch_merged_prs = github_merged_prs_fetcher(repo, offline=offline)
        entries = collect_entries(repo, to_sha, from_ref, fetch_merged_prs=fetch_merged_prs)
    else:
        if offline:
            have = local_versions(repo)
        else:
            have = remote_versions(repo)
            if git("fetch", "--quiet", "--tags", "origin", cwd=repo, check=False, timeout=NET_TIMEOUT).returncode != 0:
                raise Refused("can't fetch origin")
        current = max(have) if have else None
        branch = branch or default_branch(repo)
        head = git("rev-parse", f"refs/remotes/origin/{branch}^{{commit}}", cwd=repo).stdout.strip()
        fetch_merged_prs = github_merged_prs_fetcher(repo, offline=offline)
        entries = collect_entries(repo, head, fetch_merged_prs=fetch_merged_prs)
    validate_entries(entries)
    plan = compute_version(current, entries)
    print(format_next(plan, len(entries)), end="")
    return plan


def _github_get_json(url, token):
    req = urllib.request.Request(
        url,
        headers={
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "Authorization": f"Bearer {token}",
        },
    )
    with urllib.request.urlopen(req, timeout=15) as resp:
        link = resp.headers.get("Link") or ""
        return json.load(resp), link


def _refresh_pr_from_github(repo_slug, number, token, api_base):
    data, _ = _github_get_json(f"{api_base.rstrip('/')}/repos/{repo_slug}/pulls/{number}", token)
    if isinstance(data, dict) and isinstance(data.get("title"), str):
        return data["title"], data.get("body") or ""
    return None


def _fetch_pr_files_from_github(repo_slug, number, token, api_base):
    url = f"{api_base.rstrip('/')}/repos/{repo_slug}/pulls/{number}/files?per_page=100"
    files = []
    while url:
        page, link = _github_get_json(url, token)
        if not isinstance(page, list):
            raise Refused(f"unexpected GitHub PR files response for #{number}")
        files.extend(page)
        m = re.search(r'<([^>]+)>;\s*rel="next"', link)
        url = m.group(1) if m else None
    return files


def _fetch_pr_commits_from_github(repo_slug, number, token, api_base):
    url = f"{api_base.rstrip('/')}/repos/{repo_slug}/pulls/{number}/commits?per_page=100"
    commits: list[PrCommit] = []
    while url:
        page, link = _github_get_json(url, token)
        if not isinstance(page, list):
            raise Refused(f"unexpected GitHub PR commits response for #{number}")
        for item in page:
            if not isinstance(item, dict):
                continue
            sha = item.get("sha") if isinstance(item.get("sha"), str) else ""
            commit_obj = item.get("commit") if isinstance(item.get("commit"), dict) else {}
            msg = commit_obj.get("message") if isinstance(commit_obj.get("message"), str) else ""
            commits.append(PrCommit(sha=sha, message=msg))
        m = re.search(r'<([^>]+)>;\s*rel="next"', link)
        url = m.group(1) if m else None
    return commits


def _commit_body_from_message(message):
    if "\n" not in message:
        return ""
    return message.split("\n", 1)[1]


def _normalize_pr_commits(commits):
    out: list[PrCommit] = []
    for item in commits:
        if isinstance(item, PrCommit):
            out.append(item)
        elif isinstance(item, dict):
            sha = item.get("sha") if isinstance(item.get("sha"), str) else ""
            commit_obj = item.get("commit") if isinstance(item.get("commit"), dict) else {}
            raw_msg = commit_obj.get("message") if "commit" in item else item.get("message")
            msg = raw_msg if isinstance(raw_msg, str) else ""
            out.append(PrCommit(sha=sha, message=msg))
        elif isinstance(item, tuple) and len(item) == 2:
            out.append(PrCommit(sha=str(item[0]), message=str(item[1])))
        else:
            raise Refused(f"unexpected PR commit entry: {item!r}")
    return out


def validate_pr_upgrade_trailers(parsed_title, body, commits):
    """Refuse PRs whose commits carry `Upgrade:` trailers when the PR description has none.

    Release notes read `Upgrade:` trailers strictly from PR descriptions (never falling back to
    commit trailers so superseded draft wording cannot leak). `chore(integration)` carrier PRs
    are exempt because each carried sub-PR owns its own PR description.
    """
    if not commits:
        return
    if parsed_title.commit_type == "chore" and parsed_title.scope == INTEGRATION_PR_SCOPE:
        return
    pr_upgrades, _ = parse_trailers(body)
    if pr_upgrades:
        return
    offending = []
    for c in _normalize_pr_commits(commits):
        commit_upgrades, _ = parse_trailers(_commit_body_from_message(c.message))
        if commit_upgrades:
            offending.append(c.sha[:7] if c.sha else "<unknown>")
    if offending:
        shown = ", ".join(offending[:5])
        more = f" (+{len(offending) - 5} more)" if len(offending) > 5 else ""
        raise Refused(
            f"PR commit(s) {shown}{more} contain an `Upgrade:` trailer, but the PR description has none. "
            "Release notes read `Upgrade:` notices only from the PR description (commit trailers are ignored "
            "so superseded draft wording does not leak). Write the final `Upgrade:` notice into the PR "
            "description (or remove obsolete `Upgrade:` trailers from commits if no operator action is needed)."
        )


def _strip_fences_and_comments(text):
    out = []
    fence = None
    for line in (text or "").splitlines():
        fence, toggled = _fence_toggle(line, fence)
        if toggled or fence is not None:
            continue
        out.append(line)
    return HTML_COMMENT_RE.sub("", "\n".join(out))


def validate_pr_issue_link(parsed_title, body):
    """Ordinary PRs must link a GitHub issue (`Closes/Fixes/Resolves #N`); `chore(release):` PRs are exempt."""
    if parsed_title.commit_type == "chore" and parsed_title.scope == "release":
        return
    stripped = _strip_fences_and_comments(body)
    if not ISSUE_LINK_RE.search(stripped):
        raise Refused(
            "PR description must link a GitHub issue with `Closes #N`, `Fixes #N`, or `Resolves #N` "
            "(outside fenced code blocks). `chore(release):` PRs are exempt."
        )


def validate_release_dir_files(all_files, added_or_modified, added_only, release_title):
    """Files under `docs/releases/` may only be generated by `ops/cut-release.py cut`
    (a `chore(release): vX.Y.Z` release PR adding `docs/releases/X.Y.Z.md` alone).
    Adding, modifying, or deleting any other file under `docs/releases/` is refused."""
    touched = [f for f in all_files if f.startswith(f"{NOTES}/")]
    if not touched:
        return None
    m = RELEASE_TITLE_RE.match((release_title or "").strip())
    if m:
        expected = f"{NOTES}/{m.group(1)}.md"
        if added_only == [expected] and added_or_modified == [expected] and all_files == [expected]:
            return None
    return (
        f"{', '.join(touched)}: files under {NOTES}/ may only be generated by `ops/cut-release.py cut` "
        f"(a `chore(release): vX.Y.Z` release PR adding only `{NOTES}/X.Y.Z.md`; deleting or editing "
        f"historical release notes is forbidden)"
    )


def lint_pr(title, body="", files=None, commits=None, *, require_issue_link=True):
    parsed = validate_header(title, label="PR title")
    if require_issue_link:
        validate_pr_issue_link(parsed, body)
    parse_trailers(body, validate=True)
    validate_release_note(body)
    validate_pr_upgrade_trailers(parsed, body, commits)
    if files is not None:
        all_files, added_or_modified, added_only = [], [], []
        for item in files:
            name = item.get("filename") if isinstance(item, dict) else str(item)
            status = item.get("status", "added") if isinstance(item, dict) else "added"
            if not name:
                continue
            all_files.append(name)
            if status != "removed":
                added_or_modified.append(name)
            if status == "added":
                added_only.append(name)
        if err := validate_release_dir_files(all_files, added_or_modified, added_only, title):
            raise Refused(err)


def lint_github_event(
    event_path,
    env=os.environ,
    refresh=_refresh_pr_from_github,
    fetch_files=_fetch_pr_files_from_github,
    fetch_commits=_fetch_pr_commits_from_github,
):
    try:
        event = json.loads(Path(event_path).read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        raise Refused(f"can't read GitHub event {event_path}: {e}")
    pr = event.get("pull_request") if isinstance(event, dict) else None
    if not isinstance(pr, dict):
        return
    title = pr.get("title") or ""
    body = pr.get("body") or ""
    files = pr.get("files") if isinstance(pr.get("files"), list) else None
    commits = pr.get("commit_messages") if isinstance(pr.get("commit_messages"), list) else None
    token = env.get("GITHUB_TOKEN")
    repo_slug = env.get("GITHUB_REPOSITORY") or (event.get("repository") or {}).get("full_name")
    number = pr.get("number")
    api_base = env.get("GITHUB_API_URL", "https://api.github.com")
    if token and repo_slug and number:
        try:
            live = refresh(repo_slug, number, token, api_base)
            if live is not None:
                title, body = live
        except (OSError, ValueError, urllib.error.URLError):
            pass
        if files is None:
            try:
                files = fetch_files(repo_slug, number, token, api_base)
            except (OSError, ValueError, urllib.error.URLError) as e:
                raise Refused(f"can't read PR #{number} files from GitHub: {e}")
        if commits is None:
            try:
                commits = fetch_commits(repo_slug, number, token, api_base)
            except (OSError, ValueError, urllib.error.URLError) as e:
                raise Refused(f"can't read PR #{number} commits from GitHub: {e}")
    lint_pr(title, body, files=files, commits=commits)


def _commits_for_pr_lint(repo, base=None, head="HEAD", range_spec=None):
    if range_spec:
        from_ref, to_ref = parse_range(range_spec)
        git("rev-parse", "--verify", f"{from_ref}^{{commit}}", cwd=repo)
        head_sha = git("rev-parse", "--verify", f"{to_ref}^{{commit}}", cwd=repo).stdout.strip()
        base_ref = from_ref
    else:
        target_base = base or f"refs/remotes/origin/{default_branch(repo)}"
        base_ref = git("merge-base", head, target_base, cwd=repo).stdout.strip()
        head_sha = git("rev-parse", "--verify", f"{head}^{{commit}}", cwd=repo).stdout.strip()
    raw = git("log", "-z", "--no-merges", "--format=%H%n%B", f"{base_ref}..{head_sha}", cwd=repo).stdout
    commits: list[PrCommit] = []
    for chunk in raw.split("\x00"):
        if not chunk:
            continue
        sha, _, msg = chunk.partition("\n")
        if sha.strip():
            commits.append(PrCommit(sha=sha.strip(), message=msg))
    return commits


def lint_commits(repo, base=None, head="HEAD", range_spec=None):
    errors = []
    if range_spec:
        from_ref, to_ref = parse_range(range_spec)
        git("rev-parse", "--verify", f"{from_ref}^{{commit}}", cwd=repo)
        head_sha = git("rev-parse", "--verify", f"{to_ref}^{{commit}}", cwd=repo).stdout.strip()
        base_ref = from_ref
        check_files = False
    else:
        target_base = base or f"refs/remotes/origin/{default_branch(repo)}"
        base_ref = git("merge-base", head, target_base, cwd=repo).stdout.strip()
        head_sha = git("rev-parse", "--verify", f"{head}^{{commit}}", cwd=repo).stdout.strip()
        check_files = True

    exclude = ["--not", base_ref]
    shas = git("rev-list", "--first-parent", "--reverse", head_sha, *exclude, cwd=repo).stdout.split()

    if check_files:
        raw_changes = git("diff", "--name-status", "--no-renames", base_ref, head_sha, cwd=repo).stdout.splitlines()
        all_files, added_or_modified, added_only = [], [], []
        for line in raw_changes:
            if "\t" not in line:
                continue
            status, path = line.split("\t", 1)
            all_files.append(path)
            if not status.startswith("D"):
                added_or_modified.append(path)
            if status == "A":
                added_only.append(path)
        release_title = None
        if len(shas) == 1:
            p, subj, _ = commit_meta(repo, shas[0])
            if len(p) == 1:
                release_title = subj
        if err := validate_release_dir_files(all_files, added_or_modified, added_only, release_title):
            errors.append(err)

    for sha in shas:
        parents, subject, body = commit_meta(repo, sha)
        if len(parents) >= 2:
            if _is_already_released_merge(repo, parents, exclude):
                continue
            m_pr = PR_MERGE_RE.match(subject)
            if not m_pr:
                continue  # branch sync merge (e.g. Merge branch 'main' into ...)
            pr_num = m_pr.group(1)
            pr_title = next((l.strip() for l in body.splitlines() if l.strip()), "")
            if parse_header(pr_title) is None:
                errors.append(format_header_error(pr_title, f"PR #{pr_num} ({sha[:7]})"))
            for msg in [body, *commit_bodies_in_pr(repo, parents[0], parents[1])]:
                try:
                    parse_trailers(msg, validate=True)
                except Refused as e:
                    errors.append(f"PR #{pr_num} ({sha[:7]}): {e}")
        else:
            if parse_header(subject) is None:
                errors.append(format_header_error(subject, f"commit {sha[:7]}"))
            try:
                parse_trailers(body, validate=True)
            except Refused as e:
                errors.append(f"commit {sha[:7]}: {e}")

    if errors:
        raise Refused("\n".join(errors))


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--repo", default=".", help="a checkout of the repository (default: here)")
    sub = ap.add_subparsers(dest="cmd", required=True)

    n = sub.add_parser("next", help="compute the next version and show which commits triggered it")
    n.add_argument("--branch")
    n.add_argument("--range", dest="range_spec", metavar="FROM..TO", help="inspect a local commit range")
    n.add_argument("--offline", action="store_true", help="use local git state only; skip origin and GitHub")

    c = sub.add_parser("cut", help="generate release notes, open/merge the release PR, and tag")
    c.add_argument("--branch")
    c.add_argument("--version", help="maintainer override (requires --reason)")
    c.add_argument("--reason", help="why --version overrides the computed version (written into the notes)")
    c.add_argument("--dry-run", action="store_true")
    c.add_argument("--range", dest="range_spec", metavar="FROM..TO", help="preview a local range (--dry-run only)")
    c.add_argument("--offline", action="store_true", help="skip origin and GitHub (--dry-run only)")
    ci = c.add_mutually_exclusive_group()
    ci.add_argument(
        "--ci-wait",
        type=int,
        default=CI_WAIT,
        metavar="S",
        help=f"how long to wait when no tested base is available (default {CI_WAIT}s)",
    )
    ci.add_argument("--skip-ci", action="store_true", help="release without checking CI (untested)")

    l = sub.add_parser("lint", help="check Conventional Commit / PR title format and release rules")
    l.add_argument("--base", help="base ref for branch commit check")
    l.add_argument("--head", default="HEAD", help="head ref for branch commit check (default: HEAD)")
    l.add_argument("--range", dest="range_spec", metavar="FROM..TO", help="check commits/PRs in FROM..TO")
    l.add_argument("--pr-title", help="validate a single PR title")
    l.add_argument("--pr-body", help="optional file containing a PR description to validate trailers")
    l.add_argument("--github-event", help="validate the pull_request payload from a GitHub Actions event JSON")

    a = ap.parse_args(argv)
    repo = Path(a.repo).resolve()
    try:
        if a.cmd == "next":
            cmd_next(repo, a.branch, a.range_spec, a.offline)
        elif a.cmd == "cut":
            if a.version and parse_version(a.version) is None:
                raise Refused(f"--version {a.version}: expected X.Y.Z")
            if a.version and not (a.reason and a.reason.strip()):
                raise Refused("--version requires --reason explaining why the computed version is overridden")
            if a.reason and not a.version:
                raise Refused("--reason is only used with --version")
            if a.range_spec and not a.dry_run:
                raise Refused("--range is only allowed with --dry-run")
            if a.offline and not a.dry_run:
                raise Refused("--offline is only allowed with --dry-run")
            reason = a.reason.strip() if a.reason else None
            if a.range_spec:
                cut_range_preview(repo, a.range_spec, a.version, reason, a.offline)
            else:
                if a.skip_ci and not a.dry_run:
                    print("warning: --skip-ci: releasing without checking CI", file=sys.stderr)
                v = cut(
                    repo,
                    a.branch or default_branch(repo),
                    a.version,
                    reason,
                    a.dry_run,
                    None if a.skip_ci else max(a.ci_wait, 0),
                    offline=a.offline,
                )
                if not a.dry_run:
                    print(f"released v{v}")
                    print(
                        f"The Release build of v{v} runs on GitHub now. If it fails on a flake, do not cut another "
                        f"version: gh run rerun <run id> --failed (gh run list --workflow release.yml --branch v{v}) "
                        f"runs it again on the same tag.",
                        file=sys.stderr,
                    )
        else:
            if a.github_event:
                lint_github_event(a.github_event)
            elif a.pr_title is not None:
                body = Path(a.pr_body).read_text(encoding="utf-8") if a.pr_body else ""
                commits = (
                    _commits_for_pr_lint(repo, a.base, a.head, a.range_spec)
                    if (a.range_spec or a.base)
                    else None
                )
                lint_pr(a.pr_title, body, commits=commits, require_issue_link=(a.pr_body is not None))
            else:
                lint_commits(repo, a.base, a.head, a.range_spec)
    except Refused as e:
        print(f"✗ {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
