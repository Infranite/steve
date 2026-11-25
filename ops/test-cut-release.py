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
m = re.search(r"workflows/ci\.yml/runs\?head_sha=([0-9a-f]{40})&event=push&branch=main&", args)
merged = re.search(r"/commits/([0-9a-f]{40})/pulls\?", args)
tested = re.search(r"workflows/ci\.yml/runs\?head_sha=([0-9a-f]{40})&event=pull_request&", args)
pr_api = re.search(r"/pulls/(\d+)$", args)
if sys.argv[1:2] != ["api"] or not (m or merged or tested or pr_api):
    sys.exit("fake gh: unexpected " + args)
if ci.get("down"):
    sys.exit("HTTP 502")
if pr_api:
    num = pr_api.group(1)
    pr_meta = (ci.get("pr_meta") or {}).get(num)
    if pr_meta is None:
        sys.exit("HTTP 404: Not Found")
    print(json.dumps({"number": int(num), "title": pr_meta["title"], "body": pr_meta.get("body", "")}))
    sys.exit(0)
if merged:
    sha = merged.group(1)
    commit_pulls = ci.get("commit_pulls") or {}
    if sha in commit_pulls:
        print(json.dumps(commit_pulls[sha]))
    elif pull:
        print(json.dumps([{"number": pull["number"], "merged_at": "2026-01-01T00:00:00Z",
                           "merge_commit_sha": pull.get("merged_as", sha),
                           "head": {"sha": pull["head"]}, "base": {"ref": pull.get("base", "main")}}]))
    else:
        print(json.dumps([]))
    sys.exit(0)
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
    runs = [{"id": 77, "run_number": 5, "head_sha": pull["head"], "event": "pull_request", "head_branch": "pr",
             "status": pull.get("status", "completed"), "conclusion": pull.get("conclusion", "success"),
             "html_url": "https://github.example/runs/77"}] if pull and pull["head"] == tested.group(1) else []
    print(json.dumps({"total_count": len(runs), "workflow_runs": runs}))
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
    ok("## Release note extraction strips comments and trailing Upgrade: lines cleanly")

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

    # Rendering: section order, manual override note, links, and complete coverage.
    entries = [
        m.ReleaseEntry("1" * 40, 50, "feat(items): add lifecycle", "feat", "items", False, "add lifecycle",
                       "Details line.", ("upgrade web client with Hub",), ()),
        m.ReleaseEntry("2" * 40, 51, "fix(web): anchor scroll", "fix", "web", False, "anchor scroll", None, (), ()),
        m.ReleaseEntry("3" * 40, None, "perf(chat): batch frames", "perf", "chat", False, "batch frames", None, (), ()),
        m.ReleaseEntry("4" * 40, 52, "refactor(store): unify snapshots", "refactor", "store", False, "unify snapshots", None, (), ()),
        m.ReleaseEntry("5" * 40, 53, "docs: clarify versioning", "docs", None, False, "clarify versioning", None, (), ()),
    ]
    rendered = m.render_notes("1.0.0", entries, "https://github.com/acme/app", manual_reason="Freeze public contracts")
    order = [rendered.index(h) for h in ("# Steve v1.0.0", "> Version set manually", "## Before upgrading", "## Features", "## Fixes", "## Performance", "## Other changes")]
    assert order == sorted(order), rendered
    assert "upgrade web client with Hub ([#50](https://github.com/acme/app/pull/50))" in rendered
    assert "  Details line." in rendered
    assert "[`3333333`](https://github.com/acme/app/commit/" + "3" * 40 + ")" in rendered
    assert "**refactor(store):** unify snapshots ([#52](https://github.com/acme/app/pull/52))" in rendered
    assert "**docs:** clarify versioning ([#53](https://github.com/acme/app/pull/53))" in rendered
    ok("release notes render in fixed order with upgrade notices, PR/commit links, and full coverage")

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
        m.lint_github_event(
            str(ev),
            env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
            refresh=lambda repo, num, tok, base: ("fix(ci): edited title", "Upgrade: restart Hub"),
            fetch_files=lambda repo, num, tok, base: [{"filename": "ops/cut-release.py", "status": "modified"}],
        )
        # Ordinary PR adding or modifying any file under docs/releases/ is refused.
        for bad_files in (
            [{"filename": "docs/releases/0.11.0.md", "status": "added"}],
            [{"filename": "docs/releases/draft.md", "status": "added"}],
            [{"filename": "docs/releases/0.10.1.md", "status": "modified"}],
        ):
            try:
                m.lint_github_event(
                    str(ev),
                    env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
                    refresh=lambda repo, num, tok, base: ("docs: edit release notes", ""),
                    fetch_files=lambda repo, num, tok, base, bf=bad_files: bf,
                )
                raise AssertionError(f"docs/releases change passed: {bad_files}")
            except m.Refused as e:
                assert "docs/releases/" in str(e), e
        # chore(release) PR adding extra files or modifying existing notes is refused.
        for bad_rel in (
            [{"filename": "docs/releases/0.11.0.md", "status": "added"}, {"filename": "README.md", "status": "modified"}],
            [{"filename": "docs/releases/0.11.0.md", "status": "modified"}],
            [{"filename": "docs/releases/0.12.0.md", "status": "added"}],
        ):
            try:
                m.lint_github_event(
                    str(ev),
                    env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
                    refresh=lambda repo, num, tok, base: ("chore(release): v0.11.0", ""),
                    fetch_files=lambda repo, num, tok, base, br=bad_rel: br,
                )
                raise AssertionError(f"bad release PR passed: {bad_rel}")
            except m.Refused as e:
                assert "docs/releases/" in str(e), e
        # Genuine chore(release): v0.11.0 PR adding only docs/releases/0.11.0.md passes.
        m.lint_github_event(
            str(ev),
            env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
            refresh=lambda repo, num, tok, base: ("chore(release): v0.11.0", ""),
            fetch_files=lambda repo, num, tok, base: [{"filename": "docs/releases/0.11.0.md", "status": "added"}],
        )
        # Deleting files under docs/releases/ (cleanup PR) passes.
        m.lint_github_event(
            str(ev),
            env={"GITHUB_TOKEN": "tok", "GITHUB_REPOSITORY": "acme/app"},
            refresh=lambda repo, num, tok, base: ("chore(release): remove old notes", ""),
            fetch_files=lambda repo, num, tok, base: [
                {"filename": "docs/releases/old/note.md", "status": "removed"},
                {"filename": "ops/cut-release.py", "status": "modified"},
            ],
        )
        ev_non_pr = Path(tmp, "push.json")
        ev_non_pr.write_text(json.dumps({"before": "a" * 40}))
        m.lint_github_event(str(ev_non_pr), env={})
    ok("lint --github-event validates PR titles/trailers, refreshes edited titles, and guards docs/releases/")


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
        merge_pr(
            a,
            50,
            "feat/items",
            "Items and delivery: legacy title before convention (#443)",
            {"src/items.txt": "1\n"},
            "feat(items): close finished cycles immediately",
        )
        commit(a, {"src/web.txt": "2\n"}, "fix(web): keep scroll anchored")
        commit(a, {"src/chat.txt": "3\n"}, "perf(chat): batch streaming frames")
        commit(a, {"src/doc.txt": "4\n"}, "docs: document version policy")
        git(a, "push", "-q", "origin", "main")

        # Without updating PR #50 on GitHub (or in --offline reading merge commit title), next and cut --dry-run MUST refuse!
        set_ci(conclusion="success", pr_meta={"50": {"title": "Items and delivery: legacy title before convention (#443)", "body": ""}})
        for cmd in (("next",), ("cut", "--dry-run"), ("next", "--range", "v0.2.24..origin/main", "--offline"), ("cut", "--dry-run", "--range", "v0.2.24..origin/main", "--offline")):
            r = cut(a, *cmd)
            assert r.returncode == 1 and "PR #50" in r.stderr and "legacy title before convention" in r.stderr and "Update the PR title on GitHub" in r.stderr, (cmd, r)
        # Online next/cut fails closed if GitHub API is unreachable (never silently falls back to commit messages).
        set_ci(conclusion="success", down=True)
        r = cut(a, "next")
        assert r.returncode == 1 and "can't read" in r.stderr and "from GitHub" in r.stderr, r
        # Also fails closed if commits/{sha}/pulls returns [] and pulls/{number} is missing/unreachable.
        set_ci(conclusion="success", pr_meta={})
        r = cut(a, "next")
        assert r.returncode == 1 and "can't read PR #50 from GitHub" in r.stderr, r
        ok("next and cut refuse non-Conventional PR titles with actionable hints and fail closed if GitHub is down")

        # Now maintainer edits PR #50 on GitHub (after merge!) to Conventional Commits + ## Release note + Upgrade:.
        pr50 = {
            "50": {
                "title": "feat(items): close finished cycles immediately",
                "body": "Summary.\n\n## Release note\n\nFinishing an item closes its active cycle immediately.\n\nUpgrade: upgrade web client alongside the Hub.\n",
            }
        }
        set_ci(conclusion="success", pr_meta=pr50)

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
        set_ci(conclusion="failure", pr_meta=pr50)
        r = cut(a, "cut", "--ci-wait", "0")
        assert r.returncode == 1 and f"failure on {head[:7]}" in r.stderr and "gh run rerun 42 --failed" in r.stderr, r
        assert git(origin, "tag", "-l", "v0.3.0") == ""
        set_ci(conclusion="success", status="in_progress", pr_meta=pr50)
        assert "still in_progress" in cut(a, "cut", "--ci-wait", "0").stderr
        for conclusion in ("cancelled", "skipped", "neutral"):
            set_ci(conclusion=conclusion, pr_meta=pr50)
            r = cut(a, "cut", "--ci-wait", "0")
            assert r.returncode == 1 and f"{conclusion} on" in r.stderr, r
        set_ci(none=True, pr_meta=pr50)
        assert "has no push run on" in cut(a, "cut", "--ci-wait", "0").stderr
        set_ci(down=True, pr_meta=pr50)
        assert "can't read" in cut(a, "cut", "--ci-wait", "0").stderr
        ok("red, in-progress, cancelled, PR-only, or unreachable CI refuses the release")

        # $STEVE_GH fallback when PATH has no gh.
        only_git = Path(tmp, "only-git")
        only_git.mkdir()
        (only_git / "git").symlink_to(shutil.which("git"))
        no_gh = dict(ENV, PATH=f"{only_git}{os.pathsep}/usr/bin{os.pathsep}/bin")
        fake = str(Path(tmp, "bin", "gh"))
        set_ci(conclusion="failure", pr_meta=pr50)
        r = subprocess.run([sys.executable, str(CUT), "--repo", str(a), "cut", "--ci-wait", "0"], capture_output=True, text=True, env=dict(no_gh, STEVE_GH=fake))
        assert r.returncode == 1 and f"failure on {head[:7]}" in r.stderr, r
        ok("no gh on PATH: $STEVE_GH is used")

        # Full release cut leaves caller checkout untouched.
        set_ci(conclusion="success", pr_meta=pr50)
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
        # --pr-title check.
        assert cut(a, "lint", "--pr-title", "fix(hub): handle EOF").returncode == 0
        assert cut(a, "lint", "--pr-title", "Handle EOF").returncode == 1
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
    """Offline fixture checks for per-PR Upgrade: precedence/dedupe and integration PR discovery."""
    # 1. Per-PR Upgrade: / BREAKING CHANGE: precedence & deduplication (mirrors v0.11.0 #547/#558).
    with tempfile.TemporaryDirectory(prefix="steve-cut-dedupe-") as tmp:
        fake_gh(tmp)
        _, a = setup(tmp)
        git(a, "tag", "-a", "v0.10.1", "-m", "Steve v0.10.1")
        git(a, "push", "-q", "origin", "v0.10.1")

        # PR #547: PR description has polished Upgrade:, commit has older wording -> ONLY PR description wins.
        merge_pr(
            a,
            547,
            "reform/wf0",
            "refactor(workflow)!: remove legacy workflow engine",
            {"src/wf.txt": "removed\n"},
            "refactor(workflow)!: remove legacy workflow engine\n\n"
            "Upgrade: drop legacy workflow tables. Remove STEVE_WORKFLOW from steve.env.\n"
            "BREAKING CHANGE: old commit breaking wording.\n",
        )
        c547 = git(a, "rev-parse", "HEAD^2")

        # PR #558: PR description has polished Upgrade:, commit AND merge commit body also have Upgrade: -> ONLY PR description wins.
        git(a, "checkout", "-q", "-b", "reform/harnesses")
        commit(
            a,
            {"src/h.txt": "h\n"},
            "feat(devices)!: harness auto-upgrade\n\n"
            "Upgrade: commit draft wording for STEVE_ADAPTER_UPDATES.\n",
        )
        c558 = git(a, "rev-parse", "HEAD")
        git(a, "checkout", "-q", "main")
        git(
            a,
            "merge",
            "-q",
            "--no-ff",
            "reform/harnesses",
            "-m",
            "Merge pull request #558 from acme/reform/harnesses\n\n"
            "feat(devices)!: harness auto-upgrade\n\n"
            "Upgrade: merge commit body wording for STEVE_ADAPTER_UPDATES.\n",
        )
        git(a, "branch", "-q", "-D", "reform/harnesses")

        # PR #559: PR description has NO Upgrade:, commits carry Upgrade: (with one duplicate across two commits) -> falls back & dedupes.
        git(a, "checkout", "-q", "-b", "fix/sec")
        commit(
            a,
            {"src/s1.txt": "1\n"},
            "fix(security): reject custom harness env\n\n"
            "Upgrade: move custom env into harness config on the Node.\n"
            "Upgrade: secret use needs an online Node.\n",
        )
        c559a = git(a, "rev-parse", "HEAD")
        commit(
            a,
            {"src/s2.txt": "2\n"},
            "fix(security): harden vault boundary\n\n"
            "Upgrade: secret use needs\n  an online Node.\n",
        )
        c559b = git(a, "rev-parse", "HEAD")
        git(a, "checkout", "-q", "main")
        git(
            a,
            "merge",
            "-q",
            "--no-ff",
            "fix/sec",
            "-m",
            "Merge pull request #559 from acme/fix/sec\n\nfix(security): close boundary holes",
        )
        git(a, "branch", "-q", "-D", "fix/sec")

        # PR #560: PR description repeats identical Upgrade: text from #559 -> cross-PR dedupe keeps one bullet.
        merge_pr(
            a,
            560,
            "fix/sec-followup",
            "fix(security): require online node",
            {"src/s3.txt": "3\n"},
            "fix(security): require online node",
        )
        c560 = git(a, "rev-parse", "HEAD^2")
        git(a, "push", "-q", "origin", "main")

        pr547_body = (
            "## Release note\n\nRemoved the legacy workflow engine.\n\n"
            "Upgrade: drop legacy workflow tables. Remove `STEVE_WORKFLOW` from `steve.env` if set.\n"
            "BREAKING CHANGE: polished PR breaking wording.\n"
        )
        pr558_body = (
            "Upgrade: `STEVE_ADAPTER_UPDATES` is replaced by the Node setting (off by default).\n"
        )
        pr559_body = "## Release note\n\nClosed security boundary holes.\n"
        pr560_body = "Upgrade: secret use needs an online Node.\n"
        set_ci(
            conclusion="success",
            commit_pulls={
                c547: [{"number": 547, "merged_at": "2026-11-25T01:00:00Z", "title": "refactor(workflow)!: remove legacy workflow engine", "body": pr547_body}],
                c558: [{"number": 558, "merged_at": "2026-11-25T02:00:00Z", "title": "feat(devices)!: harness auto-upgrade", "body": pr558_body}],
                c559a: [{"number": 559, "merged_at": "2026-11-25T03:00:00Z", "title": "fix(security): close boundary holes", "body": pr559_body}],
                c559b: [{"number": 559, "merged_at": "2026-11-25T03:00:00Z", "title": "fix(security): close boundary holes", "body": pr559_body}],
                c560: [{"number": 560, "merged_at": "2026-11-25T04:00:00Z", "title": "fix(security): require online node", "body": pr560_body}],
            },
        )

        r_next = cut(a, "next", "--range", "v0.10.1..origin/main")
        assert r_next.returncode == 0 and r_next.stdout.startswith("0.11.0 (minor, from v0.10.1)\n"), r_next
        r_dry = cut(a, "cut", "--dry-run", "--range", "v0.10.1..origin/main", extra_env={"GH_REPO": "acme/app"})
        assert r_dry.returncode == 0, r_dry
        before = r_dry.stdout.split("## Before upgrading\n\n", 1)[1].split("\n\n## ", 1)[0]
        bullets = [line for line in before.splitlines() if line.startswith("- ")]
        assert len(bullets) == 5, before
        assert "Remove `STEVE_WORKFLOW` from `steve.env` if set." in before and "STEVE_WORKFLOW from steve.env." not in before, before
        assert "polished PR breaking wording." in before and "old commit breaking wording." not in before, before
        assert "`STEVE_ADAPTER_UPDATES` is replaced" in before and "commit draft wording" not in before and "merge commit body wording" not in before, before
        assert before.count("secret use needs an online Node.") == 1, before
        assert "move custom env into harness config on the Node." in before, before
        assert before.count("#547") == 2 and before.count("#558") == 1 and before.count("#559") == 2 and "#560" not in before, before
        ok("per-PR Upgrade:/BREAKING CHANGE: prefers PR description, falls back to commits, and dedupes identical text")

    # 2. Integration PR (#569) landing multiple sub-PRs via local non-PR merges + commits/{sha}/pulls discovery.
    with tempfile.TemporaryDirectory(prefix="steve-cut-integ-") as tmp:
        fake_gh(tmp)
        _, a = setup(tmp)
        git(a, "tag", "-a", "v0.11.0", "-m", "Steve v0.11.0")
        git(a, "push", "-q", "origin", "v0.11.0")
        v011_sha = git(a, "rev-parse", "v0.11.0^{commit}")

        # Build 4 independent feature branches off main, then merge them locally into integ/batch-0.12 (step-1..step-4).
        git(a, "checkout", "-q", "-b", "pr/560", "main")
        commit(a, {"src/waits.txt": "1\n"}, "feat(waits): enforce unified WaitContract")
        c560_1 = git(a, "rev-parse", "HEAD")
        commit(a, {"src/waits.txt": "2\n"}, "fix(waits): align wait fixtures")
        c560_2 = git(a, "rev-parse", "HEAD")

        git(a, "checkout", "-q", "-b", "pr/507", "main")
        commit(
            a,
            {"src/backup.txt": "1\n"},
            "Bundle verified restic engine\n\nUpgrade: draft commit upgrade note for backup v2.\n",
        )
        c507_1 = git(a, "rev-parse", "HEAD")
        commit(a, {"src/backup.txt": "2\n"}, "Remove backup v2 transitional checks")
        c507_2 = git(a, "rev-parse", "HEAD")

        git(a, "checkout", "-q", "-b", "pr/563", "main")
        commit(
            a,
            {"src/store.txt": "1\n"},
            "refactor(store)!: remove legacy remnants\n\n"
            "Upgrade: back up SQLite before migration 128.\n",
        )
        c563_1 = git(a, "rev-parse", "HEAD")

        git(a, "checkout", "-q", "-b", "pr/553", "main")
        commit(a, {"src/handoff.txt": "1\n"}, "fix(handoff): return typed HandoffError codes")
        c553_1 = git(a, "rev-parse", "HEAD")

        # Assemble integ/batch-0.12 with local non-PR merge commits (matching real origin/integ/batch-0.12).
        git(a, "checkout", "-q", "-b", "integ/batch-0.12", "main")
        for step, br in enumerate(("pr/560", "pr/507", "pr/563", "pr/553"), 1):
            git(a, "merge", "-q", "--no-ff", br, "-m", f"Merge branch 'integ/batch-0.12' into step-{step}")
            git(a, "branch", "-q", "-D", br)

        # Land integ/batch-0.12 onto main through integration PR #569 (chore(integration)).
        git(a, "checkout", "-q", "main")
        git(
            a,
            "merge",
            "-q",
            "--no-ff",
            "integ/batch-0.12",
            "-m",
            "Merge pull request #569 from acme/integ/batch-0.12\n\nchore(integration): land batch 0.12",
        )
        git(a, "branch", "-q", "-D", "integ/batch-0.12")
        git(a, "push", "-q", "origin", "main")

        pr569_obj = {
            "number": 569,
            "merged_at": "2026-11-25T10:00:00Z",
            "title": "chore(integration): land batch 0.12",
            "body": "Batch integration PR landing #560, #507, #563, #553.",
        }
        pr560_obj = {
            "number": 560,
            "merged_at": "2026-11-25T10:00:00Z",
            "title": "feat(waits): unify wait contract and timeout handling",
            "body": "## Release note\n\nUnified item dependency wait deadlines.\n",
        }
        pr507_obj = {
            "number": 507,
            "merged_at": "2026-11-25T10:00:00Z",
            "title": "feat(backup)!: single encrypted backup repository",
            "body": "Upgrade: initialize the encrypted backup repository after upgrading.\n",
        }
        # Omit title/body on #563 in commits/{sha}/pulls to also exercise pulls/{number} fallback fetch.
        pr563_ref = {"number": 563, "merged_at": "2026-11-25T10:00:00Z"}
        pr553_obj = {
            "number": 553,
            "merged_at": "2026-11-25T10:00:00Z",
            "title": "fix(handoff): return typed HandoffError codes",
            "body": None,
        }
        # Unmerged (open / closed-without-merge) and already-released PRs must be ignored.
        open_pr = {"number": 590, "merged_at": None, "title": "wip not merged", "body": ""}
        closed_pr = {"number": 591, "merged_at": None, "title": "bad closed title", "body": ""}
        already_released_pr = {
            "number": 400,
            "merged_at": "2026-11-20T00:00:00Z",
            "merge_commit_sha": v011_sha,
            "title": "feat(old): already released before v0.11.0",
            "body": "Upgrade: old upgrade already shipped.\n",
        }

        commit_pulls = {
            c560_1: [pr560_obj, pr569_obj],
            c560_2: [pr560_obj, pr569_obj],
            c507_1: [pr507_obj, pr569_obj],
            c507_2: [pr507_obj],  # even if GitHub omits #569 on some commits
            c563_1: [pr563_ref, pr569_obj],
            c553_1: [pr553_obj, pr569_obj, open_pr, closed_pr, already_released_pr],
        }
        pr_meta = {
            "563": {"title": "refactor(store)!: remove legacy remnants", "body": ""},
            "569": {"title": "chore(integration): land batch 0.12", "body": "Batch PR."},
        }
        set_ci(conclusion="success", commit_pulls=commit_pulls, pr_meta=pr_meta)

        r_next = cut(a, "next")
        assert r_next.returncode == 0 and r_next.stdout.startswith("0.12.0 (minor, from v0.11.0)\n"), r_next
        assert "#560 [feat]" in r_next.stdout, r_next.stdout
        assert "#507 [breaking (pre-1.0 -> MINOR), Upgrade:, feat]" in r_next.stdout, r_next.stdout
        assert "#563 [breaking (pre-1.0 -> MINOR), Upgrade:]" in r_next.stdout, r_next.stdout
        assert "#569" not in r_next.stdout, r_next.stdout
        assert "#590" not in r_next.stdout and "#591" not in r_next.stdout and "#400" not in r_next.stdout, r_next.stdout

        r_dry = cut(a, "cut", "--dry-run", extra_env={"GH_REPO": "acme/app"})
        assert r_dry.returncode == 0, r_dry
        out = r_dry.stdout
        assert "# Steve v0.12.0" in out, out
        before = out.split("## Before upgrading\n\n", 1)[1].split("\n\n## ", 1)[0]
        assert "initialize the encrypted backup repository after upgrading." in before, before
        assert "draft commit upgrade note for backup v2" not in before, before
        assert "back up SQLite before migration 128." in before and "#563" in before, before
        assert "#569" not in before and "#400" not in before, before
        features = out.split("## Features\n\n", 1)[1].split("\n\n## ", 1)[0]
        assert "#560" in features and "Unified item dependency wait deadlines." in features and "#507" in features, features
        fixes = out.split("## Fixes\n\n", 1)[1].split("\n\n## ", 1)[0]
        assert "#553" in fixes and "return typed HandoffError codes" in fixes, fixes
        other = out.split("## Other changes\n\n", 1)[1]
        assert "**refactor(store):** remove legacy remnants ([#563](https://github.com/acme/app/pull/563))" in other, other
        assert "**chore(integration):** land batch 0.12 ([#569](https://github.com/acme/app/pull/569))" in other, other
        assert "#590" not in out and "#591" not in out and "#400" not in out, out
        ok("integration PR resolves all merged sub-PRs via commits/{sha}/pulls, computes MINOR, and keeps chore carrier in Other changes")

        # Non-conventional title on a sub-PR inside an integration PR is refused with an actionable hint.
        bad_pulls = dict(commit_pulls)
        bad_pulls[c507_1] = [dict(pr507_obj, title="Backup v2 overhaul"), pr569_obj]
        bad_pulls[c507_2] = [dict(pr507_obj, title="Backup v2 overhaul")]
        set_ci(conclusion="success", commit_pulls=bad_pulls, pr_meta=pr_meta)
        r_bad = cut(a, "next")
        assert r_bad.returncode == 1 and "PR #507" in r_bad.stderr and "Backup v2 overhaul" in r_bad.stderr, r_bad
        ok("non-conventional sub-PR title inside an integration PR is refused")

        # Post-1.0 MAJOR bump and single-PR carrier tie-break (sub-PR has higher number than integration PR).
        git(a, "tag", "-a", "v1.0.0", "-m", "Steve v1.0.0")
        git(a, "push", "-q", "origin", "v1.0.0")
        git(a, "checkout", "-q", "-b", "pr/571", "main")
        commit(
            a,
            {"src/api.txt": "v2\n"},
            "refactor(api)!: drop v1 endpoints\n\nUpgrade: point clients at /api/v2.\n",
        )
        c571 = git(a, "rev-parse", "HEAD")
        git(a, "checkout", "-q", "-b", "integ/single", "main")
        git(a, "merge", "-q", "--no-ff", "pr/571", "-m", "Merge branch 'integ/single' into step-1")
        git(a, "checkout", "-q", "main")
        git(
            a,
            "merge",
            "-q",
            "--no-ff",
            "integ/single",
            "-m",
            "Merge pull request #570 from acme/integ/single\n\nchore(integration): land single breaking PR",
        )
        git(a, "branch", "-q", "-D", "pr/571", "integ/single")
        git(a, "push", "-q", "origin", "main")
        set_ci(
            conclusion="success",
            commit_pulls={
                c571: [
                    {"number": 571, "merged_at": "2026-11-25T11:00:00Z", "title": "refactor(api)!: drop v1 endpoints", "body": ""},
                    {"number": 570, "merged_at": "2026-11-25T11:00:00Z", "title": "chore(integration): land single breaking PR", "body": ""},
                ]
            },
        )
        r_major = cut(a, "next")
        assert r_major.returncode == 0 and r_major.stdout.startswith("2.0.0 (major, from v1.0.0)\n"), r_major
        assert "#571 [breaking]" in r_major.stdout and "#570" not in r_major.stdout, r_major
        r_major_dry = cut(a, "cut", "--dry-run", extra_env={"GH_REPO": "acme/app"})
        assert r_major_dry.returncode == 0, r_major_dry
        before_major = r_major_dry.stdout.split("## Before upgrading\n\n", 1)[1].split("\n\n## ", 1)[0]
        assert "point clients at /api/v2." in before_major and "#571" in before_major and "#570" not in before_major, before_major
        ok("post-1.0 breaking sub-PR triggers MAJOR and single-PR carrier does not steal sub-PR commit trailers")


if __name__ == "__main__":
    unit_checks()
    integration_checks()
    latest_tested_checks()
    upgrade_dedupe_and_integration_checks()
    print("All cut-release checks passed")
