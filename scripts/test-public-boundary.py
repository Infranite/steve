#!/usr/bin/env python3
"""Unit and CLI tests for scripts/check-public-boundary.py using synthetic fixtures only."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True

SCRIPT = Path(__file__).with_name("check-public-boundary.py")
spec = importlib.util.spec_from_file_location("boundary", SCRIPT)
boundary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(boundary)


def _clean_env(**extra):
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in (boundary.ENV_RULES_FILES, boundary.ENV_RULES_PATTERNS, boundary.ENV_NO_DEFAULT_RULES)
    }
    env[boundary.ENV_NO_DEFAULT_RULES] = "1"
    env.update(extra)
    return env


class PublicBoundaryTests(unittest.TestCase):
    def test_cargo_dependencies_public_and_private(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "a").mkdir()
            (root / "b").mkdir()
            p = root / "a/Cargo.toml"
            p.write_text('[dependencies]\nb = { path = "../b" }\n')
            self.assertEqual(boundary.scan(root, ["a/Cargo.toml"]), [])
            p.write_text('[target.\'cfg(unix)\'.dependencies]\nprivate = { path = "../../outside" }\n')
            self.assertEqual(boundary.scan(root, ["a/Cargo.toml"])[0]["rule"], "dependency-outside-public-tree")
            p.write_text('[build-dependencies]\nprivate = { git = "ssh://private.invalid/repo" }\n')
            findings = boundary.scan(root, ["a/Cargo.toml"])
            self.assertEqual({x["rule"] for x in findings}, {"dependency-git-needs-review"})
            self.assertNotIn("private.invalid", str(findings))

    def test_package_json_and_lockfile_dependencies(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "web").mkdir()
            (root / "shared").mkdir()
            pkg = root / "web/package.json"
            pkg.write_text(
                json.dumps(
                    {
                        "name": "demo",
                        "dependencies": {
                            "ok-local": "file:../shared",
                            "ok-semver": "^1.2.3",
                        },
                    }
                )
            )
            self.assertEqual(boundary.scan(root, ["web/package.json"]), [])

            pkg.write_text(
                json.dumps(
                    {
                        "name": "demo",
                        "workspaces": ["../outside-ws"],
                        "dependencies": {
                            "escape": "file:../../private-lib",
                            "tilde": "link:~/private/pkg",
                            "gitdep": "git+ssh://git@private.invalid/org/repo.git",
                        },
                        "overrides": {
                            "nested": {"inner": "../missing-dir"},
                        },
                    }
                )
            )
            findings = boundary.scan(root, ["web/package.json"])
            rules = [x["rule"] for x in findings]
            self.assertEqual(rules.count("dependency-outside-public-tree"), 4)
            self.assertEqual(rules.count("dependency-git-needs-review"), 1)
            self.assertNotIn("private.invalid", str(findings))

            lock = root / "package-lock.json"
            lock.write_text(
                json.dumps(
                    {
                        "packages": {
                            "node_modules/@demo/web": {"resolved": "web", "link": True},
                            "node_modules/ok": {"resolved": "https://registry.npmjs.org/ok/-/ok-1.0.0.tgz"},
                            "node_modules/ext-link": {"resolved": "../outside", "link": True},
                            "node_modules/custom-reg": {"resolved": "https://npm.internal.invalid/a.tgz"},
                            "node_modules/git-pkg": {"resolved": "git+https://example.invalid/repo.git"},
                        }
                    }
                )
            )
            lock_findings = boundary.scan(root, ["package-lock.json"])
            self.assertEqual(
                {x["rule"] for x in lock_findings},
                {"dependency-outside-public-tree", "dependency-custom-registry", "dependency-git-needs-review"},
            )
            self.assertNotIn("internal.invalid", str(lock_findings))

    def test_package_swift_dependencies(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "apple/Pkg").mkdir(parents=True)
            (root / "apple/LocalLib").mkdir(parents=True)
            manifest = root / "apple/Pkg/Package.swift"
            manifest.write_text(
                '// .package(path: "../../commented-out")\n'
                'let package = Package(dependencies: [\n'
                '    .package(path: "../LocalLib"),\n'
                '])\n'
            )
            self.assertEqual(boundary.scan(root, ["apple/Pkg/Package.swift"]), [])

            manifest.write_text(
                'let package = Package(dependencies: [\n'
                '    .package(\n'
                '        path: "../../../private-swift"\n'
                '    ),\n'
                '    .package(url: "file:///opt/private/lib"),\n'
                '    .package(url: "ssh://git@private.invalid/repo.git", from: "1.0.0"),\n'
                '], targets: [\n'
                '    .binaryTarget(name: "Priv", path: "../../missing.xcframework"),\n'
                '])\n'
            )
            findings = boundary.scan(root, ["apple/Pkg/Package.swift"])
            rules = [x["rule"] for x in findings]
            self.assertEqual(rules.count("dependency-outside-public-tree"), 3)
            self.assertEqual(rules.count("dependency-git-needs-review"), 1)
            self.assertNotIn("private.invalid", str(findings))

    def test_process_documents_reject_arbitrary_tests_markdown_and_proposal_frontmatter(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            paths = [
                "docs/design.md",
                "docs/ST-9/prototype.html",
                "hub-rs/README.md",
                "web/DESIGN.md",
                "docs/releases/draft.md",
                # Unallowlisted Markdown under tests/ must now be rejected (no blanket /tests/ exemption).
                "hub-rs/crates/a/tests/notes.md",
                "hub-rs/crates/a/tests/fixtures/x/README.md",
                "web/test/design-plan.md",
                # Allowlisted user docs, release notes, and explicit fixture directories.
                "README.md",
                "docs/deployment.md",
                "docs/releases/0.11.0.md",
                "docs/assets/logo.svg",
                "hub-rs/crates/steve-channels/tests/fixtures/feishu/README.md",
                "hub-rs/crates/steve-github/tests/fixtures/github/README.md",
                "hub-rs/crates/steve-knowledge/tests/eval/corpus/wiki/sample.md",
            ]
            found = boundary.scan(root, paths)
            self.assertEqual(
                sorted(x["path"] for x in found if x["rule"] == "process-document"),
                [
                    "docs/ST-9/prototype.html",
                    "docs/design.md",
                    "docs/releases/draft.md",
                    "hub-rs/README.md",
                    "hub-rs/crates/a/tests/fixtures/x/README.md",
                    "hub-rs/crates/a/tests/notes.md",
                    "web/DESIGN.md",
                    "web/test/design-plan.md",
                ],
            )

            # Even inside an allowlisted corpus directory, proposal front matter is rejected.
            fixture = root / "hub-rs/crates/steve-knowledge/tests/eval/corpus/wiki/sample.md"
            fixture.parent.mkdir(parents=True)
            fixture.write_text("---\ntype: proposal\ntitle: Internal Plan\nstatus: reviewing\n---\nBody\n")
            fm_found = boundary.scan(root, ["hub-rs/crates/steve-knowledge/tests/eval/corpus/wiki/sample.md"])
            self.assertEqual([x["rule"] for x in fm_found], ["process-document-frontmatter"])

    def test_builtin_rules_catch_step1_and_step2_leak_classes(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            wiki = root / "hub-rs/crates/steve-knowledge/tests/eval/corpus/wiki/leak.md"
            wiki.parent.mkdir(parents=True)
            # Construct synthetic trigger strings dynamically so this test file itself stays clean.
            home_leak = "/" + "Users" + "/alice/work/repo"
            vol_leak = "/" + "Volumes" + "/PrivateDrive/backups"
            link_leak = "[[" + "30_projects/internal/plan|Internal Plan]]"
            hw_leak = "Mac" + "17,16"
            wiki.write_text(
                "\n".join(
                    [
                        "---",
                        "title: Synthetic Fixture",
                        "---",
                        f"Home: {home_leak}",
                        f"Volume: {vol_leak}",
                        f"Ref: {link_leak}",
                        f"Machine: {hw_leak}",
                    ]
                )
                + "\n"
            )
            found = boundary.scan(
                root,
                [
                    "hub-rs/crates/steve-knowledge/tests/eval/corpus/wiki/leak.md",
                    ".steve/qa/capture.png",
                ],
            )
            self.assertEqual(
                {x["rule"] for x in found},
                {
                    "personal-home",
                    "private-volume",
                    "private-wiki-link",
                    "hardware-model-id",
                    "private-runtime-artifact",
                },
            )
            # No matched source content leaks into reports.
            report_text = json.dumps(found)
            for secret in (home_leak, vol_leak, link_leak, hw_leak):
                self.assertNotIn(secret, report_text)

    def test_neutral_paths_and_links_are_permitted(self):
        self.assertIsNone(boundary.BUILTIN_TOKENS["personal-home"].search("/Users/|/etc/"))
        self.assertIsNone(boundary.BUILTIN_TOKENS["personal-home"].search("/Users/runner/work/repo"))
        self.assertIsNone(boundary.BUILTIN_TOKENS["personal-home"].search("/home/example/projects/app"))
        self.assertIsNone(boundary.BUILTIN_TOKENS["private-volume"].search("/Volumes/Macintosh HD/Apps"))
        self.assertIsNone(boundary.BUILTIN_TOKENS["private-wiki-link"].search("[[guides/overview|Overview]]"))

    def test_external_private_rules_file_and_env_catch_personal_identifiers(self):
        with tempfile.TemporaryDirectory() as repo_dir, tempfile.TemporaryDirectory() as cfg_dir:
            root = Path(repo_dir)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "hub-rs/crates/steve-auth/src").mkdir(parents=True)
            (root / "hub-rs/crates/steve-modules/tests").mkdir(parents=True)
            (root / "hub-rs/crates/steve-knowledge/tests/eval/corpus/wiki").mkdir(parents=True)

            auth_file = root / "hub-rs/crates/steve-auth/src/lib.rs"
            auth_file.write_text("// OIDC against the private IdP at https://idp.maintainer-fixture.invalid\n")
            teams_file = root / "hub-rs/crates/steve-modules/tests/teams.rs"
            teams_file.write_text('let org = "ExamplePrivateLab";\n')
            wiki_file = root / "hub-rs/crates/steve-knowledge/tests/eval/corpus/wiki/plan.md"
            wiki_file.write_text("Workspace: ~/PrivateAgents/app via gw-fixture 198.51.100.10\n")
            subprocess.run(["git", "add", "."], cwd=root, check=True)

            # 1) External JSON rules file via --rules (supports regex patterns and case-insensitive literals)
            rules_file = Path(cfg_dir) / "private-rules.json"
            rules_file.write_text(
                json.dumps(
                    {
                        "version": 1,
                        "rules": [
                            {"name": "personal-domain", "pattern": r"maintainer-fixture\.invalid", "ignore_case": True},
                            {"name": "private-org", "literal": "ExamplePrivateLab", "ignore_case": True},
                            {"name": "personal-workspace", "literal": "~/PrivateAgents/", "ignore_case": False},
                        ],
                    }
                )
            )
            res = subprocess.run(
                [sys.executable, str(SCRIPT), "--root", str(root), "--strict", "--rules", str(rules_file)],
                capture_output=True,
                text=True,
                env=_clean_env(),
            )
            self.assertEqual(res.returncode, 1, res.stderr)
            payload = json.loads(res.stdout)
            self.assertEqual(payload["private_rules_loaded"], 3)
            self.assertEqual(
                {v["rule"] for v in payload["violations"]},
                {"personal-domain", "private-org", "personal-workspace"},
            )
            for secret in ("maintainer-fixture.invalid", "ExamplePrivateLab", "PrivateAgents"):
                self.assertNotIn(secret, res.stdout)

            # 2) Inline JSON rules via STEVE_PUBLIC_BOUNDARY_PATTERNS env var + STEVE_PUBLIC_BOUNDARY_RULES
            env_res = subprocess.run(
                [sys.executable, str(SCRIPT), "--root", str(root), "--strict"],
                capture_output=True,
                text=True,
                env=_clean_env(
                    **{
                        boundary.ENV_RULES_FILES: str(rules_file),
                        boundary.ENV_RULES_PATTERNS: json.dumps(
                            {
                                "private-gateway": {"literal": "gw-fixture"},
                                "private-network": r"(?<![\d.])198\.51\.100\.\d+(?![\d.])",
                            }
                        ),
                    }
                ),
            )
            self.assertEqual(env_res.returncode, 1, env_res.stderr)
            env_payload = json.loads(env_res.stdout)
            self.assertEqual(env_payload["private_rules_loaded"], 5)
            self.assertEqual(
                {v["rule"] for v in env_payload["violations"]},
                {"personal-domain", "private-org", "personal-workspace", "private-gateway", "private-network"},
            )
            self.assertNotIn("gw-fixture", env_res.stdout)
            self.assertNotIn("198.51.100", env_res.stdout)

            # 3) Refuses a private rules file accidentally placed inside the scanned repository root
            inside_rules = root / "private-rules.json"
            inside_rules.write_text('{"x": "secret"}\n')
            bad = subprocess.run(
                [sys.executable, str(SCRIPT), "--root", str(root), "--rules", str(inside_rules)],
                capture_output=True,
                text=True,
                env=_clean_env(),
            )
            self.assertEqual(bad.returncode, 2)
            self.assertIn("outside the public repository", bad.stderr)

    def test_scanner_does_not_exempt_itself_or_its_tests(self):
        repo_root = SCRIPT.resolve().parents[1]
        self.assertEqual(
            boundary.scan(
                repo_root,
                ["scripts/check-public-boundary.py", "scripts/test-public-boundary.py"],
            ),
            [],
        )
        # Prove a rule matching inside check-public-boundary.py is reported (no self-exemption).
        hit = boundary.scan(
            repo_root,
            ["scripts/check-public-boundary.py"],
            extra_tokens={"probe": boundary.re.compile(r"BUILTIN_TOKENS")},
        )
        self.assertTrue(any(x["rule"] == "probe" for x in hit))

    def test_fingerprint_moves_with_line_but_detects_new_content(self):
        a = boundary.finding("a", "rule", 1, "old")
        b = boundary.finding("a", "rule", 9, "old")
        c = boundary.finding("a", "rule", 9, "new")
        self.assertEqual(a["fingerprint"], b["fingerprint"])
        self.assertNotEqual(a["fingerprint"], c["fingerprint"])


if __name__ == "__main__":
    unittest.main()
