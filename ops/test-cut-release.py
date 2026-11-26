#!/usr/bin/env python3
"""cut-release.py unit and end-to-end checks on throwaway repositories: no network, fake `gh`."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

CUT = Path(__file__).resolve().parent / "cut-release.py"
ENV = dict(
    {k: v for k, v in os.environ.items() if k not in ("STEVE_GH", "GH_REPO")},
    GIT_AUTHOR_NAME="t",
    GIT_AUTHOR_EMAIL="t@example.com",
    GIT_COMMITTER_NAME="t",
    GIT_COMMITTER_EMAIL="t@example.com",
)

FAKE_GH = r"""#!/usr/bin/env python3
import json, os, re, subprocess, sys
args = " ".join(a.replace("\n", " ") for a in sys.argv[1:])
open(os.environ["FAKE_GH_LOG"], "a").write(args + "\n")
ci = json.load(open(os.environ["FAKE_GH_CI"]))
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
        msg = "Merge pull request #" + rest[0] + " from acme/" + pr["head"] + "\n\n" + pr["title"]
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
if sys.argv[1:3] == ["api", "graphql"]:
    if ci.get("down"):
        sys.exit("HTTP 502")
    if ci.get("graphql_errors"):
        print(json.dumps({"errors": ci["graphql_errors"]}))
        sys.exit(0)
    cursor = None
    for idx, tok in enumerate(sys.argv):
        if tok in ("-f", "-F") and idx + 1 < len(sys.argv) and sys.argv[idx + 1].startswith("endCursor="):
            cursor = sys.argv[idx + 1].split("=", 1)[1]
    pages = ci.get("merged_prs_pages")
    if pages is not None:
        key = cursor or "first"
        page = pages.get(key)
        if page is None:
            sys.exit("fake gh: unexpected graphql cursor " + str(cursor))
        print(json.dumps({"data": {"repository": {"pullRequests": page}}}))
        sys.exit(0)
    nodes = list(ci.get("merged_prs") or [])
    for num, p in prs.items():
        if p.get("state") == "MERGED":
            nodes.append({
                "number": int(num),
                "title": p["title"],
                "body": p.get("body", ""),
                "mergedAt": "2026-11-25T12:00:00Z",
                "updatedAt": "2026-11-25T12:00:00Z",
                "headRefOid": p["sha"],
                "mergeCommit": {"oid": p["merged"]} if p.get("merged") else None,
            })
    print(json.dumps({"data": {"repository": {"pullRequests": {
        "pageInfo": {"hasNextPage": False, "endCursor": None},
        "nodes": nodes,
    }}}}))
    sys.exit(0)
m = re.search(r"workflows/ci\.yml/runs\?head_sha=([0-9a-f]{40})&event=push&branch=main&", args)
tested = re.search(r"workflows/ci\.yml/runs\?head_sha=([0-9a-f]{40})&event=pull_request&", args)
if sys.argv[1:2] != ["api"] or not (m or tested):
    sys.exit("fake gh: unexpected " + args)
if ci.get("down"):
    sys.exit("HTTP 502")
if tested and any(p["sha"] == tested.group(1) for p in prs.values()):
    if ci.get("move_main") and not os.path.exists(prs_file + ".moved"):
        open(prs_file + ".moved", "w").close()
        main = og("rev-parse", "refs/heads/main")
        index = prs_file + ".index"
        og("read-tree", main, env={"GIT_INDEX_FILE": index})
        mv = ci["move_main"]
        files = mv.get("files", mv) if isinstance(mv, dict) else {}
        mv_msg = mv.get("msg", "fix(core): handle meanwhile update") if isinstance(mv, dict) and "msg" in mv else "fix(core): handle meanwhile update"
        for path, text in files.items():
            blob = subprocess.run(["git", "-C", origin, "hash-object", "-w", "--stdin"], input=text, check=True,
                                  capture_output=True, text=True).stdout.strip()
            og("update-index", "--add", "--cacheinfo", "100644," + blob + "," + path, env={"GIT_INDEX_FILE": index})
        tree = og("write-tree", env={"GIT_INDEX_FILE": index})
        og("update-ref", "refs/heads/main", og("commit-tree", tree, "-p", main, "-m", mv_msg), main)
    rel = ci.get("release_pr", {})
    runs = [{"id": 88, "run_number": 9, "head_sha": tested.group(1), "event": "pull_request",
             "head_branch": "release", "status": rel.get("status", "completed"),
             "conclusion": rel.get("conclusion", "success"), "html_url": "https://github.example/runs/88"}]
    print(json.dumps({"total_count": 1, "workflow_runs": runs}))
    sys.exit(0)
if tested:
    print(json.dumps({"total_count": 0, "workflow_runs": []}))
    sys.exit(0)
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


def load_module(name="cut_release"):
    spec = importlib.util.spec_from_file_location(name, CUT)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[name] = mod
    spec.loader.exec_module(mod)
    return mod


def fake_gh(tmp):
    bin_ = Path(tmp, "bin")
    bin_.mkdir()
    gh = bin_ / "gh"
    gh.write_text(FAKE_GH.replace("#!/usr/bin/env python3", f"#!{sys.executable}", 1))
    gh.chmod(0o755)
    ENV.update(
        PATH=f"{bin_}{os.pathsep}{ENV['PATH']}",
        FAKE_GH_CI=str(Path(tmp, "ci.json")),
        FAKE_GH_LOG=str(Path(tmp, "gh.log")),
        FAKE_GH_ORIGIN=str(Path(tmp, "origin.git")),
        FAKE_GH_PRS=str(Path(tmp, "prs.json")),
    )
    set_ci(conclusion="success")


def set_ci(**state):
    Path(ENV["FAKE_GH_CI"]).write_text(json.dumps(state))
    for once in (".moved", ".modified"):
        Path(ENV["FAKE_GH_PRS"] + once).unlink(missing_ok=True)


def prs():
    p = Path(ENV["FAKE_GH_PRS"])
    return json.loads(p.read_text()) if p.exists() else {}


def gh_calls():
    p = Path(ENV["FAKE_GH_LOG"])
    return p.read_text().splitlines() if p.exists() else []


def git(cwd, *args):
    return subprocess.run(["git", "-C", str(cwd), *args], check=True, capture_output=True, text=True, env=ENV).stdout.strip()


def cut(cwd, *args, extra_env=None):
    env = dict(ENV, **(extra_env or {}))
    return subprocess.run([sys.executable, str(CUT), "--repo", str(cwd), *args], capture_output=True, text=True, env=env)


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


def merge_pr(repo, number, branch_name, pr_title, files, commit_msg=None):
    """Create a feature branch commit and merge it with GitHub's merge-commit shape."""
    base = git(repo, "rev-parse", "HEAD")
    git(repo, "checkout", "-q", "-b", branch_name)
    commit(repo, files, commit_msg or pr_title)
    git(repo, "checkout", "-q", "main")
    git(
        repo,
        "merge",
        "-q",
        "--no-ff",
        branch_name,
        "-m",
        f"Merge pull request #{number} from acme/{branch_name}\n\n{pr_title}",
    )
    git(repo, "branch", "-q", "-D", branch_name)
    assert git(repo, "rev-parse", "HEAD^1") == base
    return git(repo, "rev-parse", "HEAD")


def setup(tmp):
    origin = Path(tmp, "origin.git")
    seed = Path(tmp, "seed")
    seed.mkdir()
    git(seed, "init", "-q", "-b", "main")
    commit(seed, {"README.md": "x\n", "docs/releases/0.2.24.md": "# Steve v0.2.24\n\nold\n"}, "chore: initial release")
    git(seed, "tag", "-a", "v0.2.24", "-m", "Steve v0.2.24")
    git(tmp, "clone", "-q", "--bare", str(seed), str(origin))
    a = Path(tmp, "a")
    git(tmp, "clone", "-q", str(origin), str(a))
    return origin, a


def ok(name):
    print(f"✓ {name}")


def unit_checks():
    m = load_module("cut_release_unit")

    # Header parsing: valid and invalid forms.
    for t in m.ALLOWED_TYPES:
        p = m.parse_header(f"{t}(hub/api-v2): do the thing")
        assert p and p.commit_type == t and p.scope == "hub/api-v2" and not p.breaking and p.description == "do the thing"
    p = m.parse_header("feat(api)!: replace item payload")
    assert p and p.breaking and p.scope == "api"
    p = m.parse_header("fix: trim whitespace")
    assert p and p.scope is None and not p.breaking
    for bad in (
        "Add feature",
        "feature(hub): wrong type",
        "Feat: capitalized",
        "feat(): empty scope",
        "feat(bad scope): spaces",
        "feat:no space",
        "feat:   ",
        "Release notes 0.10.1",
        "feat: first\nsecond",
    ):
        assert m.parse_header(bad) is None, bad
    ok("Conventional Commits header parser enforces exact allowed types, scope, and breaking marker")

    # Trailers: multi-line, bullets, multiple trailers, code fences, empty validation.
    body = """Summary paragraph.

```
Upgrade: ignored inside code block
BREAKING CHANGE: also ignored inside code block
```

Upgrade: upgrade Hub and Node together.
  Reinstalling the old release cannot undo migration 124.
Upgrade:
- Back up SQLite before upgrading.
- Re-grant macOS Full Disk Access once.
Closes #443
BREAKING CHANGE: drop `/api/v1/legacy` endpoint.
Co-Authored-By: Gemini <noreply@google.com>
"""
    ups, brs = m.parse_trailers(body, validate=True)
    assert ups == [
        "upgrade Hub and Node together.\nReinstalling the old release cannot undo migration 124.",
        "- Back up SQLite before upgrading.\n- Re-grant macOS Full Disk Access once.",
    ], ups
    assert brs == ["drop `/api/v1/legacy` endpoint."], brs
    try:
        m.parse_trailers("Upgrade:\n\nCo-Authored-By: x", validate=True)
        raise AssertionError("empty Upgrade: was accepted")
    except m.Refused as e:
        assert "empty Upgrade:" in str(e), e
    ok("trailer parser handles multi-line Upgrade:, BREAKING CHANGE:, code fences, and empty-trailer rejection")

    # ## Release note extraction.
    pr_body = """## Summary
Internal context.

## Release note
<!-- keep concise -->
Marking an item finished closes its cycle immediately.

- Handles paused and running items.

Upgrade: upgrade the web client alongside the Hub.

## Testing
Ran cargo nextest.
"""
    rn = m.extract_release_note(pr_body)
    assert rn == "Marking an item finished closes its cycle immediately.\n\n- Handles paused and running items.", rn
    assert m.extract_release_note("## Summary\nNothing else.") is None
    assert m.extract_release_note("## Release note\n<!-- only comment -->\n") is None
    # Stops at any next Markdown heading (including ### Checks), while preserving headings inside code fences.
    pr_subhead = (
        "## Release note\n\nUser summary.\n\n"
        "```md\n### Example heading inside fence\n```\n\n"
        "### Checks\n\nChecks: npm run check\n\n"
        "🤖 Generated with [Claude Code](https://claude.com/claude-code)\n"
    )
    assert m.extract_release_note(pr_subhead) == "User summary.\n\n```md\n### Example heading inside fence\n```", m.extract_release_note(pr_subhead)
    ok("## Release note extraction stops at any next heading and strips comments and trailing Upgrade: lines")

    # lint_pr rejects Checks: paragraphs and signature lines inside ## Release note, and accepts them after ## Checks.
    m.lint_pr(
        "feat(web): split Node page",
        "Closes #10\n\n## Release note\n\nReorganized the Node page.\n\n## Checks\n\nChecks: npm run check\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n",
    )
    m.lint_pr(
        "feat(web): split Node page",
        "Fixes #11\n\n## Release note\n\nReorganized the Node page.\n\n### Checks\n\nLocal checks: npm run check\n\nCo-Authored-By: Gemini <noreply@google.com>\n",
    )
    # Without ## Release note, Checks: and signature lines in PR body are allowed.
    m.lint_pr(
        "refactor(core): tidy helper",
        "Resolves #12\n\n## Summary\n\nInternal cleanup.\n\nChecks: cargo test\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n",
    )
    # Checks: / signature inside fenced code blocks or HTML comments within ## Release note are allowed.
    m.lint_pr(
        "docs(release): document example",
        "closes #13\n\n## Release note\n\n<!-- Checks: comment ignored -->\n```\nChecks: example\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n```\n",
    )
    # Checks: variants inside ## Release note (no separating heading) are refused with ## Checks guidance.
    for bad_checks in (
        "Checks: npm run check, test:web 361/361",
        "Local checks: steve-types, guards",
        "Local focused checks: cargo check",
        "**Checks:** npm run check",
    ):
        try:
            m.lint_pr("feat(web): split Node page", f"Closes #14\n\n## Release note\n\nUser summary.\n\n{bad_checks}\n")
            raise AssertionError(f"checks paragraph passed lint: {bad_checks}")
        except m.Refused as e:
            assert "## Checks" in str(e) and "checks paragraph" in str(e), e
    # Signature / attribution lines inside ## Release note are refused with ## Checks guidance.
    for bad_sig in (
        "🤖 Generated with [Claude Code](https://claude.com/claude-code)",
        "Generated with [Claude Code](https://claude.com/claude-code)",
        "Co-Authored-By: Gemini <noreply@google.com>",
        "Signed-off-by: Maintainer <m@example.com>",
    ):
        try:
            m.lint_pr("feat(web): split Node page", f"Closes #15\n\n## Release note\n\nUser summary.\n\n{bad_sig}\n")
            raise AssertionError(f"signature line passed lint: {bad_sig}")
        except m.Refused as e:
            assert "## Checks" in str(e) and "signature/attribution line" in str(e), e
    # Both Checks: and signature in the same ## Release note report both violations together.
    try:
        m.lint_pr(
            "feat(web): split Node page",
            "Closes #16\n\n## Release note\n\nUser summary.\n\nChecks: npm run check\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n",
        )
        raise AssertionError("combined Checks: + signature passed lint")
    except m.Refused as e:
        assert "checks paragraph" in str(e) and "signature/attribution line" in str(e) and "## Checks" in str(e), e
    # Ordinary PRs without Closes/Fixes/Resolves #N outside code blocks are refused; chore(release): is exempt.
    for missing_issue_body in (
        "",
        "## Release note\n\nNo issue link here.\n",
        "```\nCloses #99\n```\n",
        "<!-- Fixes #99 -->\n",
        "Related to #99\n",
        "Closes #0\n",
    ):
        try:
            m.lint_pr("fix(hub): patch bug", missing_issue_body)
            raise AssertionError(f"PR without issue link passed lint: {missing_issue_body!r}")
        except m.Refused as e:
            assert "Closes #N" in str(e), e
    m.lint_pr("chore(release): v0.12.0", "")
    ok("lint rejects Checks:/signatures inside ## Release note and enforces PR issue links (exempting chore(release))")

    # Version computation: pre-1.0 vs post-1.0, PATCH/MINOR/MAJOR, triggers.
    def mk(ct, *, breaking=False, upgrades=(), pr=None, title=None):
        return m.ReleaseEntry(
            sha="a" * 40,
            pr_number=pr,
            raw_title=title or f"{ct or 'misc'}: item",
            commit_type=ct,
            scope="hub",
            breaking=breaking,
            description="item",
            release_note=None,
            upgrades=tuple(upgrades),
            breaking_notes=(),
        )

    assert m.compute_version(None, [mk("fix")]).version == "0.1.0"
    p_patch = m.compute_version((0, 10, 1), [mk("fix"), mk("perf"), mk("refactor"), mk("docs"), mk("chore"), mk("revert")])
    assert (p_patch.version, p_patch.bump, p_patch.triggers) == ("0.10.2", "patch", ())
    p_feat = m.compute_version((0, 10, 1), [mk("fix", pr=1), mk("feat", pr=2)])
    assert (p_feat.version, p_feat.bump) == ("0.11.0", "minor") and len(p_feat.triggers) == 1 and p_feat.triggers[0][0].pr_number == 2
    p_up = m.compute_version((0, 10, 1), [mk("fix", upgrades=("restart node",), pr=3)])
    assert (p_up.version, p_up.bump) == ("0.11.0", "minor") and p_up.triggers[0][1] == ("Upgrade:",)
    p_pre1_br = m.compute_version((0, 10, 1), [mk("refactor", breaking=True, pr=4)])
    assert (p_pre1_br.version, p_pre1_br.bump) == ("0.11.0", "minor")
    p_post1_br = m.compute_version((1, 2, 3), [mk("feat", pr=5), mk("fix", breaking=True, pr=6)])
    assert (p_post1_br.version, p_post1_br.bump) == ("2.0.0", "major") and len(p_post1_br.triggers) == 1 and p_post1_br.triggers[0][0].pr_number == 6
    p_post1_feat = m.compute_version((1, 2, 3), [mk("feat", pr=7)])
    assert (p_post1_feat.version, p_post1_feat.bump) == ("1.3.0", "minor")
    ok("version calculation follows pre-1.0 / post-1.0 MAJOR, MINOR, and PATCH rules")

    # Rendering: section order, manual override note, links, chore carrier summary, and complete coverage.
    entries = [
        m.ReleaseEntry("1" * 40, 50, "feat(items): add lifecycle", "feat", "items", False, "add lifecycle",
                       "Details line.", ("upgrade web client with Hub",), ()),
        m.ReleaseEntry("2" * 40, 51, "fix(web): anchor scroll", "fix", "web", False, "anchor scroll", None, (), ()),
        m.ReleaseEntry("3" * 40, None, "perf(chat): batch frames", "perf", "chat", False, "batch frames", None, (), ()),
        m.ReleaseEntry("4" * 40, 52, "refactor(store): unify snapshots", "refactor", "store", False, "unify snapshots", None, (), ()),
        m.ReleaseEntry("5" * 40, 53, "docs: clarify versioning", "docs", None, False, "clarify versioning", None, (), ()),
        m.ReleaseEntry("6" * 40, 69, "chore(integration): land batch", "chore", "integration", False, "land batch",
                       "Verbose carrier body that must be suppressed.", (), (), carried_prs=(50, 51)),
    ]
    rendered = m.render_notes("1.0.0", entries, "https://github.com/acme/app", manual_reason="Freeze public contracts")
    order = [rendered.index(h) for h in ("# Steve v1.0.0", "> Version set manually", "## Before upgrading", "## Features", "## Fixes", "## Performance", "## Other changes")]
    assert order == sorted(order), rendered
    assert "upgrade web client with Hub ([#50](https://github.com/acme/app/pull/50))" in rendered
    assert "  Details line." in rendered
    assert "[`3333333`](https://github.com/acme/app/commit/" + "3" * 40 + ")" in rendered
    assert "**refactor(store):** unify snapshots ([#52](https://github.com/acme/app/pull/52))" in rendered
    assert "**docs:** clarify versioning ([#53](https://github.com/acme/app/pull/53))" in rendered
    assert (
        "- **chore(integration):** land batch ([#69](https://github.com/acme/app/pull/69); "
        "includes [#50](https://github.com/acme/app/pull/50), [#51](https://github.com/acme/app/pull/51))"
    ) in rendered, rendered
    assert "Verbose carrier body" not in rendered, rendered
    ok("release notes render in fixed order with upgrade notices, PR/commit links, single-line chore carrier, and full coverage")

    # lint_pr rejects PRs whose commits have Upgrade: trailers when the PR description has none.
    upgrade_commit = m.PrCommit(sha="a1b2c3d4" + "0" * 32, message="feat(store): migrate schema\n\nUpgrade: draft commit upgrade notice.\n")
    fenced_commit = m.PrCommit(sha="b2c3d4e5" + "0" * 32, message="docs: document trailer\n\n```\nUpgrade: example inside fence\n```\n")
    try:
        m.lint_pr("feat(store): migrate schema", "Closes #20\n\n## Release note\n\nSchema migrated.\n", commits=[upgrade_commit])
        raise AssertionError("missing PR-description Upgrade: passed lint")
    except m.Refused as e:
        assert "a1b2c3d" in str(e) and "PR description" in str(e), e
    # Writing the final Upgrade: into the PR description satisfies lint.
    m.lint_pr(
        "feat(store): migrate schema",
        "Closes #20\n\n## Release note\n\nSchema migrated.\n\nUpgrade: final operator upgrade notice.\n",
        commits=[upgrade_commit],
    )
    # Upgrade: only inside a fenced code block in a commit does not trigger the rule.
    m.lint_pr("docs: document trailer", "Closes #21\n\n## Summary\n\nDocs.\n", commits=[fenced_commit])
    # chore(integration) carrier PR is exempt because carried sub-PRs own their PR descriptions.
    m.lint_pr("chore(integration): land batch 0.12", "Closes #22\n\nBatch PR.", commits=[upgrade_commit])
    ok("lint rejects PRs with commit Upgrade: trailers missing from the PR description (exempting chore(integration))")

    # Cross-entry deduplication in ## Before upgrading (identical text across entries, breaking fallback guard).
    dup_entries = [
        m.ReleaseEntry("1" * 40, 60, "feat(node)!: bump protocol", "feat", "node", True, "bump protocol",
                       None, ("upgrade Hub and Node together.",), ()),
        m.ReleaseEntry("2" * 40, 61, "fix(node)!: align protocol", "fix", "node", True, "align protocol",
                       None, ("upgrade Hub and\n  Node together.",), ()),
        m.ReleaseEntry("3" * 40, 62, "refactor(api)!: drop v1", "refactor", "api", True, "drop v1",
                       None, (), ("drop legacy `/api/v1`.",)),
        m.ReleaseEntry("4" * 40, 63, "chore(api)!: remove v1 routes", "chore", "api", True, "remove v1 routes",
                       None, (), ("drop legacy   `/api/v1`.",)),
    ]
    dup_rendered = m.render_notes("0.12.0", dup_entries, "https://github.com/acme/app")
    before_sec = dup_rendered.split("## Features")[0]
    assert before_sec.count("upgrade Hub and") == 1, before_sec
    assert "align protocol" not in before_sec, before_sec
    assert before_sec.count("drop legacy") == 1, before_sec
    assert "remove v1 routes" not in before_sec, before_sec
    ok("render_notes deduplicates identical Upgrade:/BREAKING CHANGE: notices across entries")

    # lint --github-event with live refresh, invalid title rejection, and docs/releases/ file guard.
    with tempfile.TemporaryDirectory(prefix="steve-lint-event-") as tmp:
        ev = Path(tmp, "event.json")
        ev.write_text(json.dumps({"pull_request": {"number": 9, "title": "Bad title", "body": ""}}))
        try:
            m.lint_github_event(str(ev), env={})
            raise AssertionError("bad PR title passed")
        except m.Refused as e:
            assert "Bad title" in str(e), e
        # Live refresh sees an edited valid PR title even if the event snapshot still has the old one.
        no_commits = lambda repo, num, tok, base: []
        m.lint_github_event(
            str(ev),
            env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
            refresh=lambda repo, num, tok, base: ("fix(ci): edited title", "Closes #30\n\nUpgrade: restart Hub"),
            fetch_files=lambda repo, num, tok, base: [{"filename": "ops/cut-release.py", "status": "modified"}],
            fetch_commits=no_commits,
        )
        # Missing issue link in a real PR event is refused.
        try:
            m.lint_github_event(
                str(ev),
                env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
                refresh=lambda repo, num, tok, base: ("fix(ci): missing issue link", "No issue link."),
                fetch_files=lambda repo, num, tok, base: [{"filename": "ops/cut-release.py", "status": "modified"}],
                fetch_commits=no_commits,
            )
            raise AssertionError("PR event without issue link passed lint_github_event")
        except m.Refused as e:
            assert "Closes #N" in str(e), e
        # Commit carries Upgrade: while PR description omits it -> refused in lint --github-event.
        try:
            m.lint_github_event(
                str(ev),
                env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
                refresh=lambda repo, num, tok, base: ("feat(store): add migration", "Closes #31\n\n## Release note\n\nMigrated.\n"),
                fetch_files=lambda repo, num, tok, base: [{"filename": "hub-rs/src/lib.rs", "status": "modified"}],
                fetch_commits=lambda repo, num, tok, base: [
                    m.PrCommit(sha="deadbeef" + "0" * 32, message="feat(store): add migration\n\nUpgrade: draft commit note.\n")
                ],
            )
            raise AssertionError("commit Upgrade: without PR-body Upgrade: passed lint_github_event")
        except m.Refused as e:
            assert "deadbee" in str(e) and "PR description" in str(e), e
        # fetch_commits network error fails closed.
        try:
            m.lint_github_event(
                str(ev),
                env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
                refresh=lambda repo, num, tok, base: ("fix(ci): ok title", "Closes #32"),
                fetch_files=lambda repo, num, tok, base: [{"filename": "ops/cut-release.py", "status": "modified"}],
                fetch_commits=lambda repo, num, tok, base: (_ for _ in ()).throw(OSError("timeout")),
            )
            raise AssertionError("fetch_commits failure passed lint_github_event")
        except m.Refused as e:
            assert "can't read PR #9 commits from GitHub" in str(e), e
        # Live PR body with Checks: or signature inside ## Release note is refused in lint --github-event.
        try:
            m.lint_github_event(
                str(ev),
                env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
                refresh=lambda repo, num, tok, base: (
                    "feat(web): regroup sidebar",
                    "Closes #33\n\n## Release note\n\nRegrouped sidebar.\n\nChecks: npm run check\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n",
                ),
                fetch_files=lambda repo, num, tok, base: [{"filename": "web/src/nav.tsx", "status": "modified"}],
                fetch_commits=no_commits,
            )
            raise AssertionError("dirty ## Release note passed lint_github_event")
        except m.Refused as e:
            assert "## Checks" in str(e) and "checks paragraph" in str(e) and "signature/attribution line" in str(e), e
        # Ordinary PR adding, modifying, or deleting any file under docs/releases/ is refused.
        for bad_files in (
            [{"filename": "docs/releases/0.11.0.md", "status": "added"}],
            [{"filename": "docs/releases/draft.md", "status": "added"}],
            [{"filename": "docs/releases/0.10.1.md", "status": "modified"}],
            [{"filename": "docs/releases/0.10.1.md", "status": "removed"}],
        ):
            try:
                m.lint_github_event(
                    str(ev),
                    env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
                    refresh=lambda repo, num, tok, base: ("docs: edit release notes", "Closes #34"),
                    fetch_files=lambda repo, num, tok, base, bf=bad_files: bf,
                    fetch_commits=no_commits,
                )
                raise AssertionError(f"docs/releases change passed: {bad_files}")
            except m.Refused as e:
                assert "docs/releases/" in str(e), e
        # chore(release) PR adding extra files, modifying, or deleting existing notes is refused.
        for bad_rel in (
            [{"filename": "docs/releases/0.11.0.md", "status": "added"}, {"filename": "README.md", "status": "modified"}],
            [{"filename": "docs/releases/0.11.0.md", "status": "modified"}],
            [{"filename": "docs/releases/0.12.0.md", "status": "added"}],
            [{"filename": "docs/releases/0.10.1.md", "status": "removed"}],
        ):
            try:
                m.lint_github_event(
                    str(ev),
                    env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
                    refresh=lambda repo, num, tok, base: ("chore(release): v0.11.0", ""),
                    fetch_files=lambda repo, num, tok, base, br=bad_rel: br,
                    fetch_commits=no_commits,
                )
                raise AssertionError(f"bad release PR passed: {bad_rel}")
            except m.Refused as e:
                assert "docs/releases/" in str(e), e
        # Genuine chore(release): v0.11.0 PR adding only docs/releases/0.11.0.md passes without an issue link.
        m.lint_github_event(
            str(ev),
            env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
            refresh=lambda repo, num, tok, base: ("chore(release): v0.11.0", ""),
            fetch_files=lambda repo, num, tok, base: [{"filename": "docs/releases/0.11.0.md", "status": "added"}],
            fetch_commits=no_commits,
        )
        # Deleting files under docs/releases/ is always refused (even under chore(release):).
        try:
            m.lint_github_event(
                str(ev),
                env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
                refresh=lambda repo, num, tok, base: ("chore(release): remove old notes", ""),
                fetch_files=lambda repo, num, tok, base: [
                    {"filename": "docs/releases/old/note.md", "status": "removed"},
                    {"filename": "ops/cut-release.py", "status": "modified"},
                ],
                fetch_commits=no_commits,
            )
            raise AssertionError("deleting docs/releases/ passed lint_github_event")
        except m.Refused as e:
            assert "docs/releases/" in str(e), e
        ev_non_pr = Path(tmp, "push.json")
        ev_non_pr.write_text(json.dumps({"before": "a" * 40}))
        m.lint_github_event(str(ev_non_pr), env={})
    ok("lint --github-event validates PR titles/trailers, refreshes edited titles, and guards docs/releases/")

    # Node PROTOCOL bump and ordinary_compat baseline fixture guards.
    bump_patch = "@@ -56,3 +56,3 @@\n /// contract\n-pub const PROTOCOL: u32 = 17;\n+pub const PROTOCOL: u32 = 18;\n"
    comment_only_patch = "@@ -55,4 +55,4 @@\n-/// old doc\n+/// new doc\n pub const PROTOCOL: u32 = 17;\n"
    jump_patch = "@@ -56,3 +56,3 @@\n-pub const PROTOCOL: u32 = 17;\n+pub const PROTOCOL: u32 = 19;\n"
    decrement_patch = "@@ -56,3 +56,3 @@\n-pub const PROTOCOL: u32 = 17;\n+pub const PROTOCOL: u32 = 16;\n"
    lib_file = "hub-rs/crates/steve-node-api/src/lib.rs"
    fixture_vitals = "hub-rs/crates/steve-node-api/tests/fixtures/ordinary_compat/node_vitals.json"
    fixture_proto = "hub-rs/crates/steve-node-api/tests/fixtures/ordinary_compat/protocol.txt"
    fixture_new = "hub-rs/crates/steve-node-api/tests/fixtures/ordinary_compat/new_payload.json"

    # 1. No PROTOCOL change (doc comment only + adding a brand-new baseline fixture) passes.
    m.lint_pr(
        "chore(node-api): clarify protocol rule and add fixture",
        "Closes #610\n\n## Release note\n\nDocumented protocol bump rules.\n",
        files=[
            {"filename": lib_file, "status": "modified", "patch": comment_only_patch},
            {"filename": fixture_new, "status": "added"},
        ],
    )

    # 2. Compliant +1 bump with allowlisted Protocol-Bump: category, concrete reason, Upgrade:, and updated baselines passes.
    good_bump_body = (
        "Closes #611\n\n## Release note\n\n"
        "Narrowed effort field to canonical enum.\n\n"
        "Protocol-Bump: type-narrowing — Narrow HarnessChoice.effort from String to ReasoningEffort; older Nodes reject new variants\n"
        "Upgrade: update the Hub and all Nodes together (Node protocol 18).\n"
    )
    m.lint_pr(
        "feat(node-api)!: narrow HarnessChoice.effort",
        good_bump_body,
        files=[
            {"filename": lib_file, "status": "modified", "patch": bump_patch},
            {"filename": fixture_vitals, "status": "modified"},
            {"filename": fixture_proto, "status": "modified"},
        ],
    )
    # Protocol-Bump: inside ## Release note is stripped from generated release notes.
    assert m.extract_release_note(good_bump_body) == "Narrowed effort field to canonical enum.", m.extract_release_note(good_bump_body)
    # Every allowlisted category and ASCII separator variant is accepted.
    for cat in m.PROTOCOL_BUMP_CATEGORIES:
        m.lint_pr(
            "feat(node-api)!: breaking wire update",
            f"Closes #612\n\nProtocol-Bump: {cat} -- Remove deprecated wire field from OpenSession payload\nUpgrade: update Hub and Nodes together.\n",
            files=[{"filename": lib_file, "status": "modified", "patch": bump_patch}],
        )

    # 3. Missing Protocol-Bump:, fenced/commented Protocol-Bump:, unknown category, short reason, or missing Upgrade: are refused.
    for label, bad_body, needle in (
        (
            "missing Protocol-Bump:",
            "Closes #613\n\nUpgrade: update Hub and Nodes together.\n",
            "Protocol-Bump:",
        ),
        (
            "Protocol-Bump: inside code fence",
            "Closes #613\n\n```\nProtocol-Bump: type-narrowing — Narrow HarnessChoice.effort from String to ReasoningEffort\n```\n\nUpgrade: update Hub and Nodes together.\n",
            "Protocol-Bump:",
        ),
        (
            "unknown category",
            "Closes #613\n\nProtocol-Bump: additive-field — Added optional field to NodeVitals struct\nUpgrade: update Hub and Nodes together.\n",
            "unknown `Protocol-Bump:` category",
        ),
        (
            "too-short justification",
            "Closes #613\n\nProtocol-Bump: type-narrowing — changed field\nUpgrade: update Hub and Nodes together.\n",
            "too short",
        ),
        (
            "missing Upgrade:",
            "Closes #613\n\nProtocol-Bump: required-field — Add required auth_mode field to InstallHarness strict struct\n",
            "without an `Upgrade:` trailer",
        ),
    ):
        try:
            m.lint_pr(
                "feat(node-api)!: breaking wire update",
                bad_body,
                files=[{"filename": lib_file, "status": "modified", "patch": bump_patch}],
            )
            raise AssertionError(f"invalid protocol bump ({label}) passed lint_pr")
        except m.Refused as e:
            assert needle in str(e), (label, e)

    # 4. Jumping (+2) or decrementing (-1) PROTOCOL is refused even with Protocol-Bump: and Upgrade:.
    for label, bad_patch in (("jump +2", jump_patch), ("decrement -1", decrement_patch)):
        try:
            m.lint_pr(
                "feat(node-api)!: breaking wire update",
                good_bump_body,
                files=[{"filename": lib_file, "status": "modified", "patch": bad_patch}],
            )
            raise AssertionError(f"non-single-step PROTOCOL ({label}) passed lint_pr")
        except m.Refused as e:
            assert "only increment by +1" in str(e), (label, e)

    # 5. Modifying, deleting, or renaming existing ordinary_compat baselines without bumping PROTOCOL is refused.
    for label, bad_files in (
        ("modified json", [{"filename": fixture_vitals, "status": "modified"}]),
        ("deleted json", [{"filename": fixture_vitals, "status": "removed"}]),
        ("renamed json", [{"filename": fixture_new, "previous_filename": fixture_vitals, "status": "renamed"}]),
        ("modified protocol.txt", [{"filename": fixture_proto, "status": "modified"}]),
    ):
        try:
            m.lint_pr(
                "fix(node-api): tweak fixture",
                "Closes #614\n\n## Release note\n\nTweaked fixture.\n",
                files=bad_files,
            )
            raise AssertionError(f"baseline mutation without bump ({label}) passed lint_pr")
        except m.Refused as e:
            assert "without bumping PROTOCOL" in str(e), (label, e)

    ok("lint enforces single-step PROTOCOL bumps, Protocol-Bump:/Upgrade: trailers, and ordinary_compat baseline lock")


def integration_checks():
    with tempfile.TemporaryDirectory(prefix="steve-cut-test-") as tmp:
        fake_gh(tmp)
        origin, a = setup(tmp)

        r = cut(a, "next")
        assert r.returncode == 0 and r.stdout.startswith("0.2.25 (patch, from v0.2.24)\n") and "No new commits" in r.stdout, r
        r = cut(a, "cut")
        assert r.returncode == 1 and "nothing to release" in r.stderr, r
        ok("no new commits since last tag: next reports patch and cut refuses")

        # Merge a PR whose git merge commit has an old non-conventional title, to prove:
        # (1) non-conventional titles are refused (never silently bucketed into Other changes as PATCH);
        # (2) live GitHub PR title + description edited after merge override the merge commit message!
        m50 = merge_pr(
            a,
            50,
            "feat/items",
            "Items and delivery: legacy title before convention (#443)",
            {"src/items.txt": "1\n"},
            "feat(items): close finished cycles immediately",
        )
        h50 = git(a, "rev-parse", f"{m50}^2")
        commit(a, {"src/web.txt": "2\n"}, "fix(web): keep scroll anchored")
        commit(a, {"src/chat.txt": "3\n"}, "perf(chat): batch streaming frames")
        commit(a, {"src/doc.txt": "4\n"}, "docs: document version policy")
        git(a, "push", "-q", "origin", "main")

        def gh_pr(number, title, body, merge_sha, head_sha, ts="2026-11-25T12:00:00Z"):
            return {
                "number": number,
                "title": title,
                "body": body,
                "mergedAt": ts,
                "updatedAt": ts,
                "headRefOid": head_sha,
                "mergeCommit": {"oid": merge_sha} if merge_sha else None,
            }

        # Without updating PR #50 on GitHub (or in --offline reading merge commit title), next and cut --dry-run MUST refuse!
        set_ci(
            conclusion="success",
            merged_prs=[gh_pr(50, "Items and delivery: legacy title before convention (#443)", "", m50, h50)],
        )
        for cmd in (("next",), ("cut", "--dry-run"), ("next", "--range", "v0.2.24..origin/main", "--offline"), ("cut", "--dry-run", "--range", "v0.2.24..origin/main", "--offline")):
            r = cut(a, *cmd)
            assert r.returncode == 1 and "PR #50" in r.stderr and "legacy title before convention" in r.stderr and "Update the PR title on GitHub" in r.stderr, (cmd, r)
        # Online next/cut fails closed if GitHub API is unreachable, returns GraphQL errors, or omits a merged PR.
        set_ci(conclusion="success", down=True)
        r = cut(a, "next")
        assert r.returncode == 1 and "can't read" in r.stderr and "from GitHub" in r.stderr, r
        set_ci(conclusion="success", graphql_errors=[{"message": "API rate limit exceeded"}])
        r = cut(a, "next")
        assert r.returncode == 1 and "GraphQL error" in r.stderr and "rate limit" in r.stderr, r
        set_ci(conclusion="success", merged_prs=[])
        r = cut(a, "next")
        assert r.returncode == 1 and "can't read PR #50" in r.stderr, r
        ok("next and cut refuse non-Conventional PR titles with actionable hints and fail closed if GitHub is down or incomplete")

        # Now maintainer edits PR #50 on GitHub (after merge!) to Conventional Commits + ## Release note + Upgrade:.
        pr50 = [
            gh_pr(
                50,
                "feat(items): close finished cycles immediately",
                "Summary.\n\n## Release note\n\nFinishing an item closes its active cycle immediately.\n\nUpgrade: upgrade web client alongside the Hub.\n",
                m50,
                h50,
            )
        ]
        set_ci(conclusion="success", merged_prs=pr50)

        r = cut(a, "next")
        assert r.returncode == 0 and r.stdout.startswith("0.3.0 (minor, from v0.2.24)\n"), r
        assert "#50 [feat, Upgrade:]" in r.stdout, r
        ok("next reads live PR title/body edited after merge, computes MINOR, and names triggering PR")

        calls_before = len(gh_calls())
        r = cut(a, "cut", "--dry-run", extra_env={"GH_REPO": "acme/app"})
        assert r.returncode == 0, r
        assert "# Steve v0.3.0" in r.stdout
        assert "## Before upgrading\n\n- upgrade web client alongside the Hub. ([#50](https://github.com/acme/app/pull/50))" in r.stdout, r.stdout
        assert "Finishing an item closes its active cycle immediately." in r.stdout, r.stdout
        assert "## Features" in r.stdout and "## Fixes" in r.stdout and "## Performance" in r.stdout and "## Other changes" in r.stdout, r.stdout
        assert "**docs:** document version policy" in r.stdout, r.stdout
        assert git(origin, "tag", "-l", "v0.3.0") == ""
        assert not [c for c in gh_calls()[calls_before:] if "workflows" in c], gh_calls()[calls_before:]
        ok("--dry-run renders complete notes from live PR title/body with links, without asking CI or mutating origin")

        # Offline --range preview with valid commits makes zero gh calls.
        calls_before = len(gh_calls())
        r = cut(a, "cut", "--dry-run", "--range", "HEAD~3..HEAD", "--offline")
        assert r.returncode == 0 and "keep scroll anchored" in r.stdout, r
        assert gh_calls()[calls_before:] == [], gh_calls()[calls_before:]
        ok("--range with --offline previews valid local commits without calling origin or gh")

        # Manual --version requires --reason and writes it into the notes.
        assert cut(a, "cut", "--dry-run", "--version", "1.0.0").returncode == 1
        assert cut(a, "cut", "--dry-run", "--reason", "why").returncode == 1
        r = cut(a, "cut", "--dry-run", "--version", "1.0.0", "--reason", "Freeze public contracts")
        assert r.returncode == 0 and "> Version set manually (`--version 1.0.0`): Freeze public contracts" in r.stdout, r
        ok("--version requires --reason and embeds the override rationale in release notes")

        # CI failure modes on head.
        head = git(origin, "rev-parse", "main")
        set_ci(conclusion="failure", merged_prs=pr50)
        r = cut(a, "cut", "--ci-wait", "0")
        assert r.returncode == 1 and f"failure on {head[:7]}" in r.stderr and "gh run rerun 42 --failed" in r.stderr, r
        assert git(origin, "tag", "-l", "v0.3.0") == ""
        set_ci(conclusion="success", status="in_progress", merged_prs=pr50)
        assert "still in_progress" in cut(a, "cut", "--ci-wait", "0").stderr
        for conclusion in ("cancelled", "skipped", "neutral"):
            set_ci(conclusion=conclusion, merged_prs=pr50)
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 1 and f"{conclusion} on" in r.stderr, r
        set_ci(none=True, merged_prs=pr50)
        assert "has no push run on" in cut(a, "cut", "--ci-wait", "0").stderr
        set_ci(down=True, merged_prs=pr50)
        assert "can't read" in cut(a, "cut", "--ci-wait", "0").stderr
        ok("red, in-progress, cancelled, PR-only, or unreachable CI refuses the release")

        # $STEVE_GH fallback when PATH has no gh.
        only_git = Path(tmp, "only-git")
        only_git.mkdir()
        (only_git / "git").symlink_to(shutil.which("git"))
        no_gh = dict(ENV, PATH=f"{only_git}{os.pathsep}/usr/bin{os.pathsep}/bin")
        fake = str(Path(tmp, "bin", "gh"))
        set_ci(conclusion="failure", merged_prs=pr50)
        r = subprocess.run([sys.executable, str(CUT), "--repo", str(a), "cut", "--ci-wait", "0"], capture_output=True, text=True, env=dict(no_gh, STEVE_GH=fake))
        assert r.returncode == 1 and f"failure on {head[:7]}" in r.stderr, r
        ok("no gh on PATH: $STEVE_GH is used")

        # Full release cut leaves caller checkout untouched.
        set_ci(conclusion="success", merged_prs=pr50)
        git(a, "checkout", "-q", "-b", "elsewhere")
        Path(a, "scratch.txt").write_text("mine\n")
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.3.0" in r.stdout, r
        assert git(a, "rev-parse", "--abbrev-ref", "HEAD") == "elsewhere" and Path(a, "scratch.txt").exists()
        pr = prs()["100"]
        assert pr["title"] == "chore(release): v0.3.0" and pr["head"] == "release/v0.3.0" and pr["state"] == "MERGED", pr
        assert git(origin, "rev-parse", "v0.3.0^{commit}") == pr["sha"] == git(origin, "rev-parse", "main^2")
        assert git(origin, "rev-parse", "v0.3.0^{commit}^") == head
        assert git(origin, "diff", "--name-status", head, "v0.3.0^{commit}") == "A\tdocs/releases/0.3.0.md"
        assert git(origin, "branch", "--list", "release/*") == ""
        notes = git(origin, "show", "main:docs/releases/0.3.0.md")
        assert "# Steve v0.3.0" in notes and "Finishing an item closes its active cycle immediately." in notes
        ok("cut opens chore(release): vX.Y.Z PR, waits for CI, merges, deletes branch, and tags notes commit")

        # Immediately cutting again refuses (the release PR merge commit is skipped).
        r = cut(a, "cut")
        assert r.returncode == 1 and "nothing to release" in r.stderr, r
        ok("the release PR merge commit is not counted as a new change")

        # Stale checkout gets PATCH 0.3.1 for a fix commit; taken version is refused.
        b = Path(tmp, "b")
        git(tmp, "clone", "-q", str(origin), str(b))
        commit(b, {"src/f.txt": "f\n"}, "fix(core): avoid nil deref")
        git(b, "push", "-q", "origin", "main")
        git(a, "checkout", "-q", "main")
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.3.1" in r.stdout, r
        r = cut(a, "cut", "--version", "0.3.1", "--reason", "duplicate")
        assert r.returncode == 1 and "already on origin" in r.stderr, r
        ok("stale checkout computes PATCH 0.3.1 from origin and refuses already-taken versions")

        # --skip-ci releases without asking CI.
        commit(b, {"src/s.txt": "s\n"}, "refactor(hub): simplify router")
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        git(b, "push", "-q", "origin", "main")
        set_ci(conclusion="failure")
        calls = len(gh_calls())
        r = cut(a, "cut", "--skip-ci")
        assert r.returncode == 0 and "released v0.3.2" in r.stdout and "--skip-ci" in r.stderr, r
        assert not [c for c in gh_calls()[calls:] if "actions/workflows" in c]
        set_ci(conclusion="success")
        ok("--skip-ci releases without CI checks")

        # Rejected push retries cleanly.
        hook = Path(origin, "hooks", "pre-receive")
        marker = Path(tmp, "rejected-once")
        hook.write_text(f"#!/bin/sh\nif [ ! -f '{marker}' ]; then touch '{marker}'; echo busy >&2; exit 1; fi\nexit 0\n")
        hook.chmod(0o755)
        subprocess.run(["git", "-C", str(b), "pull", "-q", "--rebase", "origin", "main"], check=True, env=ENV)
        commit(b, {"src/r.txt": "r\n"}, "fix(node): flush stderr")
        marker.touch()
        git(b, "push", "-q", "origin", "main")
        marker.unlink()
        r = cut(a, "cut")
        assert r.returncode == 0 and "released v0.3.3" in r.stdout and "again from the new state" in r.stderr, r
        hook.unlink()
        ok("a rejected push starts again and lands once")

        # lint on branch: valid commits, docs/releases/ guard (general rule), chore(release) exception, invalid subject.
        git(a, "fetch", "-q", "origin")
        git(a, "checkout", "-q", "-b", "feat/lint-test", "origin/main")
        commit(a, {"src/l1.txt": "1\n"}, "feat(web): add filter\n\nUpgrade: reload open tabs.")
        assert cut(a, "lint").returncode == 0
        # Local PR check with --base refuses when branch commits carry Upgrade: but PR body omits it.
        no_up_body = Path(tmp, "no-up-body.md")
        no_up_body.write_text("Closes #40\n\n## Release note\n\nAdded filter.\n", encoding="utf-8")
        r = cut(a, "lint", "--pr-title", "feat(web): add filter", "--pr-body", str(no_up_body), "--base", "origin/main")
        assert r.returncode == 1 and "PR description" in r.stderr, r
        with_up_body = Path(tmp, "with-up-body.md")
        with_up_body.write_text("Closes #40\n\n## Release note\n\nAdded filter.\n\nUpgrade: reload open tabs after upgrading.\n", encoding="utf-8")
        assert cut(a, "lint", "--pr-title", "feat(web): add filter", "--pr-body", str(with_up_body), "--base", "origin/main").returncode == 0
        # Deleting an existing release note on a branch -> refused.
        git(a, "rm", "-q", "docs/releases/0.3.0.md")
        git(a, "commit", "-q", "-m", "chore: delete old release notes")
        r = cut(a, "lint")
        assert r.returncode == 1 and "docs/releases/0.3.0.md" in r.stderr, r
        git(a, "reset", "-q", "--hard", "HEAD~1")
        # Adding any file under docs/releases/ on a regular branch -> refused.
        commit(a, {"docs/releases/draft.md": "- Draft.\n"}, "docs: add draft note")
        r = cut(a, "lint")
        assert r.returncode == 1 and "files under docs/releases/ may only be generated by" in r.stderr, r
        git(a, "reset", "-q", "--hard", "HEAD~1")
        # Adding a hand-written versioned release note -> refused.
        commit(a, {"docs/releases/0.4.0.md": "# Steve v0.4.0\n"}, "docs: write version ahead")
        r = cut(a, "lint")
        assert r.returncode == 1 and "docs/releases/0.4.0.md" in r.stderr, r
        git(a, "reset", "-q", "--hard", "HEAD~1")
        # Non-conventional commit subject -> refused.
        commit(a, {"src/l2.txt": "2\n"}, "wip quick fix")
        r = cut(a, "lint")
        assert r.returncode == 1 and "wip quick fix" in r.stderr, r
        # A genuine chore(release): v0.4.0 branch adding only docs/releases/0.4.0.md passes lint.
        git(a, "checkout", "-q", "-b", "release/v0.4.0", "origin/main")
        commit(a, {"docs/releases/0.4.0.md": "# Steve v0.4.0\n"}, "chore(release): v0.4.0")
        assert cut(a, "lint").returncode == 0
        # --pr-title and --pr-body check.
        assert cut(a, "lint", "--pr-title", "fix(hub): handle EOF").returncode == 0
        assert cut(a, "lint", "--pr-title", "Handle EOF").returncode == 1
        bad_body = Path(tmp, "bad-pr-body.md")
        bad_body.write_text("Closes #41\n\n## Release note\n\nFixed EOF.\n\nChecks: cargo test\n", encoding="utf-8")
        r = cut(a, "lint", "--pr-title", "fix(hub): handle EOF", "--pr-body", str(bad_body))
        assert r.returncode == 1 and "## Checks" in r.stderr, r
        good_body = Path(tmp, "good-pr-body.md")
        good_body.write_text("Closes #41\n\n## Release note\n\nFixed EOF.\n\n## Checks\n\nChecks: cargo test\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n", encoding="utf-8")
        assert cut(a, "lint", "--pr-title", "fix(hub): handle EOF", "--pr-body", str(good_body)).returncode == 0
        # --range check on main skips release PR merges and validates commits.
        assert cut(a, "lint", "--range", "v0.3.0..origin/main").returncode == 0
        ok("lint validates branch commits, PR titles, ranges, and enforces docs/releases/ cut-only rule")

        # In-process checks: interrupted tag push recovery even when a higher-bump commit lands later.
        m = load_module("cut_release_proc")
        saved_env = dict(os.environ)
        os.environ.update({k: v for k, v in ENV.items() if k.startswith(("GIT_", "FAKE_GH_")) or k == "PATH"})
        try:
            # GH_PLACES discovery when PATH has no gh.
            saved_path, saved_places = os.environ.get("PATH"), m.GH_PLACES
            os.environ["PATH"] = str(only_git)
            m.GH_PLACES = (str(Path(tmp, "nowhere", "gh")), str(Path(tmp, "bin", "gh")))
            try:
                assert m.gh_path() == str(Path(tmp, "bin", "gh"))
                m.GH_PLACES = (str(Path(tmp, "nowhere", "gh")),)
                assert m.gh_path() is None
            finally:
                os.environ["PATH"], m.GH_PLACES = saved_path, saved_places
            ok("no gh on PATH nor $STEVE_GH: usual install places are checked")

            # Push tag fails -> left_behind recovers 0.3.4 even after a later `feat:` lands on main!
            git(b, "fetch", "-q", "origin")
            git(b, "checkout", "-q", "-B", "main", "origin/main")
            commit(b, {"src/p.txt": "p\n"}, "fix(store): close cursor")
            git(b, "push", "-q", "origin", "main")
            real_push = m.push

            def drop_tag_push(wt, *refspecs):
                if refspecs[-1].startswith("refs/tags/"):
                    raise m.Refused("pushing timed out")
                return real_push(wt, *refspecs)

            m.push = drop_tag_push
            try:
                m.cut(b, "main", None, None, False)
                raise AssertionError("released without tag push")
            except m.Refused:
                pass
            m.push = real_push
            assert git(origin, "tag", "-l", "v0.3.4") == ""
            notes_commit = git(origin, "rev-parse", "main^2")
            assert git(origin, "log", "-1", "--format=%s", notes_commit) == "chore(release): v0.3.4"

            # Now a `feat:` commit lands on main before the next cut!
            git(b, "fetch", "-q", "origin")
            git(b, "checkout", "-q", "-B", "main", "origin/main")
            commit(b, {"src/feat_later.txt": "fl\n"}, "feat(ui): add timeline view")
            git(b, "push", "-q", "origin", "main")

            r = cut(a, "cut", "--dry-run")
            assert r.returncode == 0 and "without its tag" in r.stderr and "close cursor" in r.stdout and "timeline" not in r.stdout, r
            opened = len(prs())
            assert m.cut(b, "main", None, None, False) == "0.3.4"
            assert len(prs()) == opened
            assert git(origin, "rev-parse", "v0.3.4^{commit}") == notes_commit
            # Next cut now picks up `feat(ui): add timeline view` as 0.4.0!
            assert m.cut(b, "main", None, None, False) == "0.4.0"
            assert "add timeline view" in git(origin, "show", "v0.4.0:docs/releases/0.4.0.md")
            ok("interrupted tagging recovers untagged v0.3.4 even after a later feat lands, then releases 0.4.0")

            # main moves between CI check and merge: tested commit is tagged, meanwhile commit goes into next release.
            git(b, "fetch", "-q", "origin")
            git(b, "checkout", "-q", "-B", "main", "origin/main")
            commit(b, {"src/t1.txt": "1\n"}, "fix(api): return 404 on missing id")
            git(b, "push", "-q", "origin", "main")
            tested_head = git(origin, "rev-parse", "main")
            before_prs = len(prs())
            set_ci(conclusion="success", move_main={"files": {"src/meanwhile.txt": "m\n"}, "msg": "feat(hub): stream metrics"})
            r = cut(a, "cut")
            assert r.returncode == 0 and "released v0.4.1" in r.stdout and "again from the new state" not in r.stderr, r
            tagged = git(origin, "rev-parse", "v0.4.1^{commit}")
            assert git(origin, "rev-parse", "v0.4.1^{commit}^") == tested_head
            assert "stream metrics" not in git(origin, "show", "v0.4.1:docs/releases/0.4.1.md")
            set_ci(conclusion="success")
            # Next release automatically picks up the meanwhile `feat` commit and bumps MINOR to 0.5.0!
            r = cut(a, "cut")
            assert r.returncode == 0 and "released v0.5.0" in r.stdout, r
            assert "stream metrics" in git(origin, "show", "v0.5.0:docs/releases/0.5.0.md")
            ok("commits landing while a release PR runs stay out of that tag and enter the next release automatically")

            # Release PR failure closes PR and deletes release branch; existing release branch refuses; base_modified retries.
            git(b, "fetch", "-q", "origin")
            git(b, "checkout", "-q", "-B", "main", "origin/main")
            commit(b, {"src/t2.txt": "2\n"}, "fix(mail): decode utf-8 header")
            git(b, "push", "-q", "origin", "main")
            before_prs = len(prs())
            set_ci(conclusion="success", release_pr={"conclusion": "failure"})
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 1 and "failure on release pull request #" in r.stderr
            assert all(p["state"] == "CLOSED" for p in list(prs().values())[before_prs:])
            assert git(origin, "branch", "--list", "release/*") == ""

            git(b, "push", "-q", "origin", "HEAD:refs/heads/release/v0.5.1")
            set_ci(conclusion="success")
            assert "release/v0.5.1 is already on origin" in cut(a, "cut").stderr
            git(b, "push", "-q", "origin", "--delete", "release/v0.5.1")

            set_ci(conclusion="success", base_modified=True)
            r = cut(a, "cut")
            assert r.returncode == 0 and "released v0.5.1" in r.stdout and "merging again" in r.stderr, r
            set_ci(conclusion="success")
            ok("failed release PR cleans up; existing release branch refuses; base_modified retries merge")

            # Release PR that never merges cleanly closes every attempt and tags nothing.
            git(b, "fetch", "-q", "origin")
            git(b, "checkout", "-q", "-B", "main", "origin/main")
            commit(b, {"src/t3.txt": "3\n"}, "fix(sync): retry cleanly")
            git(b, "push", "-q", "origin", "main")
            real_gh = m.gh

            def conflicting(*args, cwd):
                if args[:2] == ("pr", "merge"):
                    raise m.Refused("can't ask GitHub: gh pr merge: X Pull request #1 is not mergeable: the merge commit cannot be cleanly created.")
                return real_gh(*args, cwd=cwd)

            m.gh = conflicting
            before_prs = len(prs())
            try:
                try:
                    m.cut(b, "main", None, None, False)
                    raise AssertionError("released conflicting PR")
                except m.Refused as e:
                    assert "origin moved while releasing" in str(e) and "does not merge cleanly" in str(e), e
            finally:
                m.gh = real_gh
            assert [p["state"] for p in list(prs().values())[before_prs:]] == ["CLOSED"] * m.ATTEMPTS
            assert git(origin, "tag", "-l", "v0.5.2") == "" and git(origin, "branch", "--list", "release/*") == ""
            ok("a release pull request that never merges cleanly closes each attempt and tags nothing")

            # Unmerged commit is never tagged.
            commit(b, {"src/u.txt": "u\n"}, "chore: unmerged")
            git(b, "push", "-q", "origin", "HEAD:refs/heads/elsewhere")
            sleep, m.time.sleep = m.time.sleep, lambda s: None
            try:
                try:
                    m.tag(b, b, git(b, "rev-parse", "HEAD"), "0.5.2", "main")
                    raise AssertionError("tagged unmerged commit")
                except m.Refused as e:
                    assert "is not on origin's main" in str(e), e
            finally:
                m.time.sleep = sleep
            ok("a commit main does not contain is never tagged")
        finally:
            os.environ.clear()
            os.environ.update(saved_env)

        # Unreachable origin is an error, never 0.1.0.
        git(a, "remote", "set-url", "origin", str(Path(tmp, "gone.git")))
        r = cut(a, "next")
        assert r.returncode == 1 and "can't read the tags" in r.stderr, r
        ok("an unreachable origin is an error, never version 0.1.0")


def latest_tested_checks():
    with tempfile.TemporaryDirectory(prefix="steve-cut-tested-") as tmp:
        fake_gh(tmp)
        origin, a = setup(tmp)
        commit(a, {"src/tested.txt": "tested\n"}, "fix(core): tested patch fix")
        tested = git(a, "rev-parse", "HEAD")
        commit(a, {"src/pending.txt": "pending\n"}, "feat(ui): untested feature")
        pending = git(a, "rev-parse", "HEAD")
        commit(a, {"src/head.txt": "head\n"}, "chore: untested chore")
        head = git(a, "rev-parse", "HEAD")
        git(a, "push", "-q", "origin", "main")

        states = {
            tested: {"conclusion": "success"},
            pending: {"conclusion": "cancelled", "older_conclusion": "success"},
            head: {"status": "queued"},
        }
        set_ci(commits=states)
        m = load_module("cut_tested")
        saved_env = dict(os.environ)
        os.environ.update(ENV)
        try:
            assert m.release_base(a, head, "main", 0) == tested
            states[head] = {"conclusion": "success"}
            set_ci(commits=states)
            assert m.release_base(a, head, "main", 0) == head
            states[head] = {"status": "queued"}
            states[pending] = {"conclusion": "success"}
            set_ci(commits=states)
            assert m.release_base(a, head, "main", 0) == pending

            for bad_state in (
                {"conclusion": "failure"},
                {"none": True},
                {"conclusion": "neutral"},
                {"status": "in_progress", "conclusion": "success"},
            ):
                states[pending] = bad_state
                set_ci(commits=states)
                assert m.release_base(a, head, "main", 0) == tested

            # Cutting now releases `tested` (fix only -> PATCH 0.2.25!), NOT `pending` (untested feat!).
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 0 and "released v0.2.25" in r.stdout, r
            assert git(origin, "rev-parse", "v0.2.25^{commit}^") == tested
            notes = git(origin, "show", "v0.2.25:docs/releases/0.2.25.md")
            assert "tested patch fix" in notes and "untested feature" not in notes, notes

            # Now `main` has the release merge commit (green), `pending` (cancelled), `head` (queued):
            # release_base must NOT pick the release merge commit (no new changes at it) and must gate on head.
            new_main = git(origin, "rev-parse", "main")
            states[new_main] = {"status": "queued"}
            states[tested] = {"conclusion": "success"}
            set_ci(commits=states)
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 1 and "still queued" in r.stderr, r

            # Once `new_main` passes, the next release cuts 0.3.0 (MINOR) containing `pending` + `head`.
            states[new_main] = {"conclusion": "success"}
            set_ci(commits=states)
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 0 and "released v0.3.0" in r.stdout, r
            notes2 = git(origin, "show", "v0.3.0:docs/releases/0.3.0.md")
            assert "untested feature" in notes2 and "untested chore" in notes2 and "tested patch fix" not in notes2, notes2
        finally:
            os.environ.clear()
            os.environ.update(saved_env)
        ok("latest tested commit determines both tree and version bump; untested feat waits for next release")


def upgrade_dedupe_and_integration_checks():
    """Fixture checks for PR-description-only Upgrade:/BREAKING CHANGE:, GraphQL discovery, and 9-PR integration batch."""

    def gh_pr(number, title, body, merge_sha, head_sha, ts="2026-11-25T12:00:00Z", updated_ts=None):
        return {
            "number": number,
            "title": title,
            "body": body,
            "mergedAt": ts,
            "updatedAt": updated_ts or ts,
            "headRefOid": head_sha,
            "mergeCommit": {"oid": merge_sha} if merge_sha else None,
        }

    # 1. PR Upgrade:/BREAKING CHANGE: come strictly from PR descriptions (stale/conflicting commit trailers ignored);
    #    direct commits without a PR still use commit trailers; identical PR notices deduplicate across PRs.
    with tempfile.TemporaryDirectory(prefix="steve-cut-pr-only-up-") as tmp:
        fake_gh(tmp)
        _, a = setup(tmp)
        git(a, "tag", "-a", "v0.10.1", "-m", "Steve v0.10.1")
        git(a, "push", "-q", "origin", "v0.10.1")

        # PR #544: early commit contradicts later commit & PR description; commit also has obsolete BREAKING CHANGE:.
        git(a, "checkout", "-q", "-b", "fix/yolo-cleanup")
        commit(
            a,
            {"src/m128.txt": "v1\n"},
            "refactor(store): initial cleanup\n\n"
            "Upgrade: Migration 128 irreversibly expires historical YOLO-decided decisions.\n"
            "BREAKING CHANGE: old commit breaking wording that was superseded.\n",
        )
        commit(
            a,
            {"src/m128.txt": "v2\n"},
            "fix(store): preserve settled YOLO decisions\n\n"
            "Upgrade: draft second commit wording for migration 128.\n",
        )
        h544 = git(a, "rev-parse", "HEAD")
        git(a, "checkout", "-q", "main")
        git(
            a,
            "merge",
            "-q",
            "--no-ff",
            "fix/yolo-cleanup",
            "-m",
            "Merge pull request #544 from acme/fix/yolo-cleanup\n\n"
            "refactor(store)!: remove retired YOLO columns\n\n"
            "Upgrade: stale merge commit body wording.\n",
        )
        m544 = git(a, "rev-parse", "HEAD")
        git(a, "branch", "-q", "-D", "fix/yolo-cleanup")

        # PR #545: commits carry Upgrade: trailers for a feature that was later reverted/dropped; PR description has NONE.
        m545 = merge_pr(
            a,
            545,
            "fix/context-cleanup",
            "fix(node): align harness session options",
            {"src/ctx.txt": "clean\n"},
            "feat(node): draft context budget\n\n"
            "Upgrade: wire protocol advances from 13 to 14 for turn context budgets.\n"
            "Upgrade: Removes STEVE_TURN_CONTEXT_BUDGET_*.\n",
        )
        h545 = git(a, "rev-parse", f"{m545}^2")

        # PR #559 & #560: both PR descriptions carry the exact same Upgrade: text -> cross-PR dedupe keeps one bullet.
        m559 = merge_pr(a, 559, "fix/sec-1", "fix(security): close boundary holes", {"src/s1.txt": "1\n"})
        h559 = git(a, "rev-parse", f"{m559}^2")
        m560 = merge_pr(a, 560, "fix/sec-2", "fix(security): require online node", {"src/s2.txt": "2\n"})
        h560 = git(a, "rev-parse", f"{m560}^2")

        # Direct commit (no PR) carrying Upgrade: and BREAKING CHANGE: -> still reads commit trailers.
        commit(
            a,
            {"src/direct.txt": "d\n"},
            "fix(cli): tighten env parser\n\n"
            "Upgrade: direct commit upgrade action.\n"
            "BREAKING CHANGE: direct commit breaking detail.\n",
        )
        git(a, "push", "-q", "origin", "main")

        set_ci(
            conclusion="success",
            merged_prs=[
                gh_pr(
                    544,
                    "refactor(store)!: remove retired YOLO columns",
                    "## Release note\n\nRemoved retired YOLO columns while preserving settled decisions.\n\n"
                    "Upgrade: Migration 128 preserves settled YOLO decisions as resolved.\n",
                    m544,
                    h544,
                ),
                gh_pr(
                    545,
                    "fix(node): align harness session options",
                    "## Release note\n\nAligned harness session options.\n",
                    m545,
                    h545,
                ),
                gh_pr(
                    559,
                    "fix(security): close boundary holes",
                    "Upgrade: secret use needs an online Node.\n",
                    m559,
                    h559,
                ),
                gh_pr(
                    560,
                    "fix(security): require online node",
                    "Upgrade: secret use needs\n  an online Node.\n",
                    m560,
                    h560,
                ),
            ],
        )

        r_next = cut(a, "next", "--range", "v0.10.1..origin/main")
        assert r_next.returncode == 0 and r_next.stdout.startswith("0.11.0 (minor, from v0.10.1)\n"), r_next
        assert "#544" in r_next.stdout and "#545" not in r_next.stdout, r_next
        r_dry = cut(a, "cut", "--dry-run", "--range", "v0.10.1..origin/main", extra_env={"GH_REPO": "acme/app"})
        assert r_dry.returncode == 0, r_dry
        before = r_dry.stdout.split("## Before upgrading\n\n", 1)[1].split("\n\n## ", 1)[0]
        bullets = [line for line in before.splitlines() if line.startswith("- ")]
        assert len(bullets) == 4, before
        assert "Migration 128 preserves settled YOLO decisions as resolved." in before, before
        for stale in (
            "irreversibly expires",
            "old commit breaking wording",
            "draft second commit wording",
            "stale merge commit body",
            "turn context budgets",
            "STEVE_TURN_CONTEXT_BUDGET_",
        ):
            assert stale not in r_dry.stdout, (stale, r_dry.stdout)
        assert "#545" not in before and "#545" in r_dry.stdout, r_dry.stdout
        assert before.count("secret use needs an online Node.") == 1, before
        assert "direct commit upgrade action." in before and "direct commit breaking detail." in before, before
        ok("PR entries read Upgrade:/BREAKING CHANGE: strictly from PR descriptions; direct commits keep commit trailers")

    # 2. Integration batch (#569) with 9 sub-PRs sharing the exact same mergeCommit, alongside direct PRs,
    #    prior release PR merge, already-released PR, off-branch PR, and paginated GraphQL cutoff.
    with tempfile.TemporaryDirectory(prefix="steve-cut-integ-9-") as tmp:
        fake_gh(tmp)
        _, a = setup(tmp)

        # Already-released PR #400 before v0.11.0.
        m400 = merge_pr(a, 400, "feat/old", "feat(old): already released", {"src/old.txt": "old\n"})
        h400 = git(a, "rev-parse", f"{m400}^2")

        # Release v0.11.0 via a release PR merge (#567) whose second parent is tagged v0.11.0.
        git(a, "checkout", "-q", "-b", "release/v0.11.0")
        commit(a, {"docs/releases/0.11.0.md": "# Steve v0.11.0\n\nnotes\n"}, "chore(release): v0.11.0")
        h567 = git(a, "rev-parse", "HEAD")
        git(a, "tag", "-a", "v0.11.0", "-m", "Steve v0.11.0", h567)
        git(a, "checkout", "-q", "main")
        git(
            a,
            "merge",
            "-q",
            "--no-ff",
            "release/v0.11.0",
            "-m",
            "Merge pull request #567 from acme/release/v0.11.0\n\nchore(release): v0.11.0",
        )
        m567 = git(a, "rev-parse", "HEAD")
        git(a, "branch", "-q", "-D", "release/v0.11.0")
        git(a, "push", "-q", "origin", "v0.11.0")

        # An off-branch PR #599 merged on a side branch that never lands on main.
        git(a, "checkout", "-q", "-b", "side-base")
        git(a, "checkout", "-q", "-b", "feat/side")
        commit(a, {"src/side.txt": "side\n"}, "feat(side): never merged to main")
        h599 = git(a, "rev-parse", "HEAD")
        git(a, "checkout", "-q", "side-base")
        git(a, "merge", "-q", "--no-ff", "feat/side", "-m", "Merge pull request #599 from acme/feat/side\n\nfeat(side): never merged to main")
        m599 = git(a, "rev-parse", "HEAD")
        git(a, "checkout", "-q", "main")
        git(a, "branch", "-q", "-D", "feat/side")

        # Direct PR #568 before the integration batch.
        m568 = merge_pr(a, 568, "fix/flaky", "fix(research): stabilize item archive", {"src/r.txt": "1\n"})
        h568 = git(a, "rev-parse", f"{m568}^2")

        # Build the 9 independent sub-PR branches (mirroring real #569's 9 PRs).
        sub_specs = [
            (
                544,
                "refactor(store)!: remove retired YOLO columns and adopt PermissionMode",
                "## Release note\n\nRemoved retired YOLO columns and moved bots onto canonical PermissionMode.\n\n"
                "Upgrade: Migration 128 preserves settled YOLO decisions as resolved.\n",
                "refactor(store)!: initial cleanup\n\nUpgrade: Migration 128 irreversibly expires historical YOLO-decided decisions.\n",
            ),
            (
                545,
                "feat(knowledge)!: redefine personal knowledge boundary and proposals",
                "## Release note\n\nScoped knowledge to personal memory and routed engineering facts to project proposals.\n\n"
                "Upgrade: Migration 125 withdraws item and chat rows from the knowledge index.\n",
                "feat(knowledge)!: draft\n\nUpgrade: draft old migration 121 wording.\n",
            ),
            (
                546,
                "feat(coordinator): route orchestration through unified coordinator",
                "## Release note\n\nUnified coordinator handles scheduling and handoffs.\n",
                "feat(coordinator): coordinator\n\nUpgrade: wire protocol advances from 13 to 14 for turn context budgets.\n",
            ),
            (
                550,
                "perf(store): batch event archive compaction",
                None,
                "perf(store): batch compaction\n\nUpgrade: Removes STEVE_TURN_CONTEXT_BUDGET_*.\n",
            ),
            (
                553,
                "fix(handoff): return typed HandoffError codes",
                None,
                "fix(handoff): return typed HandoffError codes",
            ),
            (
                560,
                "feat(waits): unify wait contract and timeout handling",
                "## Release note\n\nUnified item dependency wait deadlines.\n",
                "feat(waits): enforce unified WaitContract",
            ),
            (
                507,
                "feat(backup)!: single encrypted backup repository",
                "## Release note\n\nConsolidated backups into one encrypted restic repository.\n\n"
                "Upgrade: initialize the encrypted backup repository after upgrading.\n",
                "Bundle verified restic engine\n\nUpgrade: draft commit upgrade note for backup v2.\n",
            ),
            (
                563,
                "refactor(channels)!: route disclosure audit through gateway",
                "Upgrade: configure disclosure model in workspace model settings instead of STEVE_MODELS_CHECK.\n",
                "refactor(channels)!: route disclosure audit",
            ),
            (
                564,
                "fix(avatar): serve person avatars from canonical store",
                None,
                "fix(avatar): serve person avatars",
            ),
        ]

        sub_heads = {}
        for num, _, _, commit_msg in sub_specs:
            br = f"pr/{num}"
            git(a, "checkout", "-q", "-b", br, "main")
            commit(a, {f"src/pr_{num}.txt": f"{num}\n"}, commit_msg)
            sub_heads[num] = git(a, "rev-parse", "HEAD")

        # Assemble integ/batch-0.12 with local non-PR merges (step-1..step-9) plus a final integration fixup commit.
        git(a, "checkout", "-q", "-b", "integ/batch-0.12", "main")
        for step, (num, _, _, _) in enumerate(sub_specs, 1):
            br = f"pr/{num}"
            git(a, "merge", "-q", "--no-ff", br, "-m", f"Merge branch 'integ/batch-0.12' into step-{step}")
            git(a, "branch", "-q", "-D", br)
        commit(a, {"src/integ_fix.txt": "ok\n"}, "fix(integ): resolve integration test wiring")
        h569 = git(a, "rev-parse", "HEAD")

        # Land integ/batch-0.12 onto main through integration PR #569.
        git(a, "checkout", "-q", "main")
        git(
            a,
            "merge",
            "-q",
            "--no-ff",
            "integ/batch-0.12",
            "-m",
            "Merge pull request #569 from acme/integ/batch-0.12\n\nchore(integration): land nine migration-bearing PRs together (0.12 batch)",
        )
        m569 = git(a, "rev-parse", "HEAD")
        git(a, "branch", "-q", "-D", "integ/batch-0.12")

        # Direct PR #571 after #569.
        m571 = merge_pr(a, 571, "fix/node-link", "fix(nodes): recover stale upgrade-only nodes", {"src/n.txt": "1\n"})
        h571 = git(a, "rev-parse", f"{m571}^2")
        git(a, "push", "-q", "origin", "main")

        # Use real commit timestamps so GraphQL time-window pagination cutoff is genuinely exercised.
        recent_ts = git(a, "show", "-s", "--format=%cI", m571)
        old_ts = "2020-01-01T00:00:00Z"

        # Put all 9 sub-PRs + #569 with the exact same mergeCommit m569, split across two GraphQL pages;
        # page 2 ends with an old PR before the time window while claiming hasNextPage=True (cursor "page3"
        # is intentionally absent so requesting a 3rd page fails immediately).
        pr569_node = gh_pr(
            569,
            "chore(integration): land nine migration-bearing PRs together (0.12 batch)",
            "## Release note\n\nCarrier summary that should stay suppressed on a single Other changes line.\n",
            m569,
            h569,
            ts=recent_ts,
        )
        sub_nodes = [
            gh_pr(num, title, body or "", m569, sub_heads[num], ts=recent_ts)
            for num, title, body, _ in sub_specs
        ]
        page1_nodes = [
            gh_pr(400, "feat(old): already released", "Upgrade: old upgrade already shipped.\n", m400, h400, ts=old_ts, updated_ts=recent_ts),
            gh_pr(571, "fix(nodes): recover stale upgrade-only nodes", "", m571, h571, ts=recent_ts),
            gh_pr(599, "feat(side): never merged to main", "Upgrade: side branch upgrade.\n", m599, h599, ts=recent_ts),
            pr569_node,
            *sub_nodes[:5],
        ]
        page2_nodes = [
            *sub_nodes[5:],
            gh_pr(568, "fix(research): stabilize item archive", "", m568, h568, ts=recent_ts),
            gh_pr(567, "chore(release): v0.11.0", "", m567, h567, ts=recent_ts),
            gh_pr(10, "chore: ancient released PR", "", m400, h400, ts=old_ts, updated_ts=old_ts),
        ]
        pages = {
            "first": {"pageInfo": {"hasNextPage": True, "endCursor": "page2"}, "nodes": page1_nodes},
            "page2": {"pageInfo": {"hasNextPage": True, "endCursor": "page3"}, "nodes": page2_nodes},
        }
        set_ci(conclusion="success", merged_prs_pages=pages)

        calls_before = len(gh_calls())
        r_next = cut(a, "next")
        assert r_next.returncode == 0 and r_next.stdout.startswith("0.12.0 (minor, from v0.11.0)\n"), r_next
        for trig in ("#507", "#544", "#545", "#546", "#560", "#563"):
            assert trig in r_next.stdout, (trig, r_next.stdout)
        for non_trig in ("#569", "#567", "#400", "#599", "#550", "#553", "#564", "#568", "#571"):
            assert non_trig not in r_next.stdout, (non_trig, r_next.stdout)

        r_dry = cut(a, "cut", "--dry-run", extra_env={"GH_REPO": "acme/app"})
        assert r_dry.returncode == 0, r_dry
        out = r_dry.stdout
        assert "# Steve v0.12.0" in out, out
        before = out.split("## Before upgrading\n\n", 1)[1].split("\n\n## ", 1)[0]
        assert "Migration 128 preserves settled YOLO decisions as resolved." in before and "#544" in before, before
        assert "Migration 125 withdraws item and chat rows" in before and "#545" in before, before
        assert "initialize the encrypted backup repository after upgrading." in before and "#507" in before, before
        assert "configure disclosure model in workspace model settings" in before and "#563" in before, before
        for excluded_in_before in ("#569", "#546", "#550", "#400", "#599", "irreversibly expires", "turn context budgets", "STEVE_TURN_CONTEXT_BUDGET_", "draft commit upgrade note", "migration 121"):
            assert excluded_in_before not in before, (excluded_in_before, before)

        features = out.split("## Features\n\n", 1)[1].split("\n\n## ", 1)[0]
        for num in (507, 545, 546, 560):
            assert f"#{num}" in features, (num, features)
        fixes = out.split("## Fixes\n\n", 1)[1].split("\n\n## ", 1)[0]
        for num in (553, 564, 568, 571):
            assert f"#{num}" in fixes, (num, fixes)
        perf = out.split("## Performance\n\n", 1)[1].split("\n\n## ", 1)[0]
        assert "#550" in perf, perf
        other = out.split("## Other changes\n\n", 1)[1]
        assert "#544" in other and "#563" in other, other
        expected_carrier = (
            "- **chore(integration):** land nine migration-bearing PRs together (0.12 batch) "
            "([#569](https://github.com/acme/app/pull/569); includes "
            + ", ".join(f"[#{n}](https://github.com/acme/app/pull/{n})" for n in (507, 544, 545, 546, 550, 553, 560, 563, 564))
            + ")"
        )
        assert expected_carrier in other, other
        assert "Carrier summary that should stay suppressed" not in out, out
        for absent in ("#567", "#400", "#599", "#10"):
            assert absent not in out, (absent, out)

        # Verify GraphQL-only discovery (zero commits/{sha}/pulls or pulls/{n} calls) and 2-page window cutoff.
        new_calls = gh_calls()[calls_before:]
        assert all(c.startswith("api graphql ") for c in new_calls), new_calls
        assert len(new_calls) == 4, new_calls  # 2 pages for `next` + 2 pages for `cut --dry-run`
        ok("GraphQL discovery resolves all 9 sub-PRs sharing #569's mergeCommit, stops at time window, and renders chore carrier as one line")

        # Non-conventional title on a sub-PR inside an integration PR is refused with an actionable hint.
        bad_page1 = [dict(n, title="Backup v2 overhaul") if n["number"] == 507 else n for n in page1_nodes]
        bad_page2 = [dict(n, title="Backup v2 overhaul") if n["number"] == 507 else n for n in page2_nodes]
        set_ci(
            conclusion="success",
            merged_prs_pages={
                "first": {"pageInfo": {"hasNextPage": True, "endCursor": "page2"}, "nodes": bad_page1},
                "page2": {"pageInfo": {"hasNextPage": False, "endCursor": None}, "nodes": bad_page2},
            },
        )
        r_bad = cut(a, "next")
        assert r_bad.returncode == 1 and "PR #507" in r_bad.stderr and "Backup v2 overhaul" in r_bad.stderr, r_bad
        ok("non-conventional sub-PR title inside an integration PR is refused")

        # 3. Post-1.0 MAJOR bump and single-PR fast-forward carrier tie-break (carrier #570 has lower number
        #    and identical headRefOid to sub-PR #571, broken deterministically by merge commit subject).
        git(a, "tag", "-a", "v1.0.0", "-m", "Steve v1.0.0")
        git(a, "push", "-q", "origin", "v1.0.0")
        git(a, "checkout", "-q", "-b", "pr/572", "main")
        commit(
            a,
            {"src/api.txt": "v2\n"},
            "refactor(api)!: drop v1 endpoints\n\nUpgrade: stale commit wording.\n",
        )
        h572 = git(a, "rev-parse", "HEAD")
        git(a, "checkout", "-q", "main")
        git(
            a,
            "merge",
            "-q",
            "--no-ff",
            "pr/572",
            "-m",
            "Merge pull request #570 from acme/integ/single\n\nchore(integration): land single breaking PR",
        )
        m570 = git(a, "rev-parse", "HEAD")
        git(a, "branch", "-q", "-D", "pr/572")
        git(a, "push", "-q", "origin", "main")
        set_ci(
            conclusion="success",
            merged_prs=[
                gh_pr(
                    572,
                    "refactor(api)!: drop v1 endpoints",
                    "Upgrade: point clients at /api/v2.\n",
                    m570,
                    h572,
                ),
                gh_pr(
                    570,
                    "chore(integration): land single breaking PR",
                    "",
                    m570,
                    h572,
                ),
            ],
        )
        r_major = cut(a, "next")
        assert r_major.returncode == 0 and r_major.stdout.startswith("2.0.0 (major, from v1.0.0)\n"), r_major
        assert "#572 [breaking]" in r_major.stdout and "#570" not in r_major.stdout, r_major
        r_major_dry = cut(a, "cut", "--dry-run", extra_env={"GH_REPO": "acme/app"})
        assert r_major_dry.returncode == 0, r_major_dry
        before_major = r_major_dry.stdout.split("## Before upgrading\n\n", 1)[1].split("\n\n## ", 1)[0]
        assert "point clients at /api/v2." in before_major and "#572" in before_major and "#570" not in before_major and "stale commit wording" not in r_major_dry.stdout, r_major_dry.stdout
        assert (
            "- **chore(integration):** land single breaking PR "
            "([#570](https://github.com/acme/app/pull/570); includes [#572](https://github.com/acme/app/pull/572))"
        ) in r_major_dry.stdout, r_major_dry.stdout
        ok("post-1.0 breaking sub-PR triggers MAJOR and fast-forward carrier tie-break picks merge-subject carrier")


def protocol_bump_lint_checks():
    """Git-backed local branch and --pr-body lint checks for PROTOCOL bumps and ordinary_compat baseline lock."""
    lib_rel = "hub-rs/crates/steve-node-api/src/lib.rs"
    fix_rel = "hub-rs/crates/steve-node-api/tests/fixtures/ordinary_compat/node_vitals.json"
    fix_new = "hub-rs/crates/steve-node-api/tests/fixtures/ordinary_compat/extra_payload.json"
    proto_rel = "hub-rs/crates/steve-node-api/tests/fixtures/ordinary_compat/protocol.txt"

    with tempfile.TemporaryDirectory(prefix="steve-cut-proto-") as tmp:
        _, a = setup(tmp)
        commit(
            a,
            {
                lib_rel: "/// wire\npub const PROTOCOL: u32 = 17;\n",
                fix_rel: "{\"at\": 1}\n",
                proto_rel: "17\n",
            },
            "chore(node-api): seed protocol baseline",
        )
        git(a, "push", "-q", "origin", "main")

        pr_body_plain = Path(tmp, "proto-plain-body.md")
        pr_body_plain.write_text("Closes #700\n\n## Release note\n\nUpdated fixture.\n", encoding="utf-8")
        pr_body_bump = Path(tmp, "proto-bump-body.md")
        pr_body_bump.write_text(
            "Closes #701\n\n## Release note\n\nNarrowed wire type.\n\n"
            "Protocol-Bump: type-narrowing — Narrow NodeVitals.at field type; older Nodes cannot decode new wire encoding\n"
            "Upgrade: update the Hub and all Nodes together (Node protocol 18).\n",
            encoding="utf-8",
        )

        # Adding a brand-new baseline fixture and editing only PROTOCOL docs passes without a bump.
        git(a, "checkout", "-q", "-b", "test/proto-additive", "origin/main")
        commit(
            a,
            {
                lib_rel: "/// wire rules updated\npub const PROTOCOL: u32 = 17;\n",
                fix_new: "{\"ok\": true}\n",
            },
            "chore(node-api): document protocol rules and add baseline fixture",
        )
        assert cut(a, "lint").returncode == 0
        assert cut(a, "lint", "--pr-title", "chore(node-api): document protocol rules", "--pr-body", str(pr_body_plain), "--base", "origin/main").returncode == 0

        # Modifying or deleting an existing baseline without bumping PROTOCOL fails both branch lint and PR lint.
        git(a, "checkout", "-q", "-b", "test/proto-baseline-edit", "origin/main")
        commit(a, {fix_rel: "{\"at\": 2}\n"}, "fix(node-api): rewrite baseline without bump")
        r = cut(a, "lint")
        assert r.returncode == 1 and "without bumping PROTOCOL" in r.stderr, r
        r = cut(a, "lint", "--pr-title", "fix(node-api): rewrite baseline without bump", "--pr-body", str(pr_body_plain), "--base", "origin/main")
        assert r.returncode == 1 and "without bumping PROTOCOL" in r.stderr, r

        git(a, "checkout", "-q", "-b", "test/proto-baseline-delete", "origin/main")
        commit(a, {fix_rel: None}, "fix(node-api): delete baseline without bump")
        r = cut(a, "lint")
        assert r.returncode == 1 and "without bumping PROTOCOL" in r.stderr, r

        # Jumping PROTOCOL 17 -> 19 fails both branch lint and PR lint.
        git(a, "checkout", "-q", "-b", "test/proto-jump", "origin/main")
        commit(
            a,
            {lib_rel: "/// wire\npub const PROTOCOL: u32 = 19;\n", proto_rel: "19\n"},
            "feat(node-api)!: skip protocol 18",
        )
        r = cut(a, "lint")
        assert r.returncode == 1 and "only increment by +1" in r.stderr, r
        r = cut(a, "lint", "--pr-title", "feat(node-api)!: skip protocol 18", "--pr-body", str(pr_body_bump), "--base", "origin/main")
        assert r.returncode == 1 and "only increment by +1" in r.stderr, r

        # Compliant +1 bump (17 -> 18) with updated baseline passes branch lint, fails PR lint without reason, and passes with Protocol-Bump: + Upgrade:.
        git(a, "checkout", "-q", "-b", "test/proto-bump-ok", "origin/main")
        commit(
            a,
            {
                lib_rel: "/// wire\npub const PROTOCOL: u32 = 18;\n",
                fix_rel: "{\"at\": 2}\n",
                proto_rel: "18\n",
            },
            "feat(node-api)!: narrow field type",
        )
        assert cut(a, "lint").returncode == 0
        r = cut(a, "lint", "--pr-title", "feat(node-api)!: narrow field type", "--pr-body", str(pr_body_plain), "--base", "origin/main")
        assert r.returncode == 1 and "Protocol-Bump:" in r.stderr and "Upgrade:" in r.stderr, r
        r = cut(a, "lint", "--pr-title", "feat(node-api)!: narrow field type", "--pr-body", str(pr_body_bump), "--base", "origin/main")
        assert r.returncode == 0, r
        ok("git-backed lint catches unbumped baseline edits, PROTOCOL jumps, missing reasons, and accepts compliant +1 bumps")


if __name__ == "__main__":
    unit_checks()
    integration_checks()
    latest_tested_checks()
    upgrade_dedupe_and_integration_checks()
    protocol_bump_lint_checks()
    print("All cut-release checks passed")
