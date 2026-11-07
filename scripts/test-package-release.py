#!/usr/bin/env python3
"""Exercise distribution boundary and artifact validation without compiling a Hub."""
import importlib.util
import json
from pathlib import Path
import tarfile
import tempfile
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class BundleTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        self.web = root / "web"
        self.web.mkdir()
        (self.web / "index.html").write_text('<html><head></head><body></body></html>')
        self.binary = root / "hub"
        self.binary.write_text('#!/bin/sh\nprintf \'%s\\n\' \'{"version":"0.2.0","commit":"' + 'a' * 40 + '","schema":24,"target":"aarch64-apple-darwin"}\'\n')
        self.binary.chmod(0o755)
        self.node = root / "node"
        self.node.write_text(self.binary.read_text())
        self.node.chmod(0o755)
        self.helper = root / "helper"
        self.helper.write_bytes(b"helper")
        (root / "private.env").write_text("must not enter release")
        self.out = root / "out"
        self.args = SimpleNamespace(version="0.2.0", commit="a" * 40,
            target="aarch64-apple-darwin", schema=24, binary=str(self.binary), node=str(self.node), keyhelper=str(self.helper), web=str(self.web), out=str(self.out))

    def test_bundle_contains_only_selected_outputs_and_consistent_version(self):
        release.package(self.args)
        manifest = self.out / "manifest-aarch64-apple-darwin.json"
        release.merge(SimpleNamespace(out=str(self.out / "steve-release.json"), manifests=[str(manifest)]))
        metadata = json.loads((self.out / "steve-release.json").read_text())
        with tarfile.open(self.out / metadata["assets"][0]["name"]) as bundle:
            self.assertNotIn("private.env", bundle.getnames())
            self.assertEqual(bundle.getmember("bin/steve-node").mode, 0o755)
            self.assertEqual(bundle.getmember("bin/steve-keyhelper").mode, 0o755)
            self.assertEqual(set(n for n in bundle.getnames() if n.startswith("ops/")),
                             {"ops/" + n for n in release.RUNTIME_OPS})
            self.assertIn("ops/upgrade.py", bundle.getnames())
            self.assertEqual(bundle.getmember("ops/upgrade.py").mode, 0o755)
            self.assertEqual(bundle.getmember("bin/steve-hub").mode, 0o755)
            self.assertEqual(json.load(bundle.extractfile("release.json"))["version"], "0.2.0")
            self.assertIn(b'content="0.2.0"', bundle.extractfile("web/dist/index.html").read())

    def test_refuses_symlink(self):
        (self.web / "secret").symlink_to(self.binary)
        with self.assertRaisesRegex(ValueError, "symlink"):
            release.package(self.args)

    def test_merge_rejects_modified_asset(self):
        release.package(self.args)
        next(self.out.glob("*.tar.gz")).write_bytes(b"corrupt")
        with self.assertRaisesRegex(ValueError, "does not match"):
            release.merge(SimpleNamespace(out=str(self.out / "steve-release.json"), manifests=[str(next(self.out.glob("manifest-*.json")))]))

    def test_merge_rejects_different_commit(self):
        release.package(self.args)
        self.args.target = "x86_64-unknown-linux-gnu"
        self.args.commit = "b" * 40
        self.binary.write_text(self.binary.read_text().replace("a" * 40, "b" * 40).replace("aarch64-apple-darwin", "x86_64-unknown-linux-gnu"))
        self.node.write_text(self.binary.read_text())
        release.package(self.args)
        with self.assertRaisesRegex(ValueError, "disagree"):
            release.merge(SimpleNamespace(out=str(self.out / "steve-release.json"), manifests=[str(p) for p in sorted(self.out.glob("manifest-*.json"))]))

    def test_refuses_noncanonical_stable_versions(self):
        for version in ("01.2.3", "1.02.3", "1.2.03", "1.2.3-rc1", "１.2.3"):
            with self.subTest(version=version):
                self.args.version = version
                with self.assertRaisesRegex(ValueError, "stable SemVer"):
                    release.package(self.args)

    def test_refuses_wrong_node_identity(self):
        self.node.write_text(self.node.read_text().replace("a" * 40, "b" * 40))
        with self.assertRaisesRegex(ValueError, "Node build identity"):
            release.package(self.args)

    def test_refuses_missing_keyhelper_on_macos(self):
        self.args.keyhelper = None
        with self.assertRaisesRegex(ValueError, "keyhelper"):
            release.package(self.args)

    def test_refuses_wrong_binary_identity(self):
        self.args.version = "0.3.0"
        with self.assertRaisesRegex(ValueError, "build identity"):
            release.package(self.args)


if __name__ == "__main__":
    unittest.main()
