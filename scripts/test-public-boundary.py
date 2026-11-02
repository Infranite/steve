#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import tempfile
import subprocess
import sys
import json
sys.dont_write_bytecode = True
import unittest

spec = importlib.util.spec_from_file_location("boundary", Path(__file__).with_name("check-public-boundary.py"))
boundary = importlib.util.module_from_spec(spec)
spec.loader.exec_module(boundary)

class PublicBoundaryTests(unittest.TestCase):
    def test_local_public_dependency_and_private_path(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "a").mkdir()
            (root / "b").mkdir()
            p = root / "a/Cargo.toml"
            p.write_text('[dependencies]\nb = { path = "../b" }\n')
            self.assertEqual(boundary.scan(root, ["a/Cargo.toml"]), [])
            p.write_text('[target.\'cfg(unix)\'.dependencies]\nprivate = { path = "../../outside" }\n')
            self.assertEqual(boundary.scan(root, ["a/Cargo.toml"])[0]["rule"], "dependency-outside-public-tree")

    def test_git_dependencies_and_personal_markers_are_reported_without_contents(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "hub-rs/crates/a").mkdir(parents=True)
            p = root / "hub-rs/crates/a/Cargo.toml"
            p.write_text('[build-dependencies]\nprivate = { git = "ssh://private.invalid/repo" }\n')
            s = root / "hub-rs/crates/a/main.rs"
            s.write_text('let endpoint = "https://pengxiang.dev/path/SECRET";\n')
            findings = boundary.scan(root, [str(p.relative_to(root)), str(s.relative_to(root))])
            self.assertEqual({x["rule"] for x in findings}, {"dependency-git-needs-review", "personal-domain"})
            self.assertNotIn("SECRET", str(findings))
            self.assertNotIn("private.invalid", str(findings))

    def test_baseline_does_not_allow_extra_identical_occurrences(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "hub-rs/crates/a").mkdir(parents=True)
            (root / "scripts").mkdir()
            p = root / "hub-rs/crates/a/main.rs"
            line = 'let endpoint = "https://pengxiang.dev";\n'
            p.write_text(line)
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            cli = [sys.executable, str(Path(__file__).with_name("check-public-boundary.py")), "--root", str(root)]
            first = subprocess.run(cli + ["--write-baseline"], capture_output=True, text=True)
            self.assertEqual(first.returncode, 0)
            p.write_text(line * 2)
            extra = subprocess.run(cli, capture_output=True, text=True)
            self.assertEqual(extra.returncode, 1)
            self.assertEqual(len(json.loads(extra.stdout)["violations"]), 1)
            strict = subprocess.run(cli + ["--strict"], capture_output=True, text=True)
            self.assertEqual(strict.returncode, 1)
            self.assertEqual(len(json.loads(strict.stdout)["violations"]), 2)

    def test_docs_and_runtime_artifacts_are_in_scope(self):
        with tempfile.TemporaryDirectory() as d:
            root = Path(d)
            (root / "docs").mkdir()
            (root / "docs/example.md").write_text("https://pengxiang.dev/private")
            found = boundary.scan(root, ["docs/example.md", ".steve/qa/capture.png"])
            self.assertEqual({x["rule"] for x in found}, {"personal-domain", "private-runtime-artifact"})

    def test_regex_literal_is_not_a_personal_home(self):
        self.assertIsNone(boundary.TOKENS["personal-home"].search("/Users/|/etc/"))
        self.assertIsNotNone(boundary.TOKENS["personal-home"].search("/Users/alice/work"))

    def test_fingerprint_moves_with_line_but_detects_new_content(self):
        a = boundary.finding("a", "rule", 1, "old")
        b = boundary.finding("a", "rule", 9, "old")
        c = boundary.finding("a", "rule", 9, "new")
        self.assertEqual(a["fingerprint"], b["fingerprint"])
        self.assertNotEqual(a["fingerprint"], c["fingerprint"])

if __name__ == "__main__":
    unittest.main()
