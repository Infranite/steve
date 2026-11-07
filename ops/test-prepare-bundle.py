#!/usr/bin/env python3
"""Prepare a bundle in temporary storage: fake signing, no real keychain or service access."""
import importlib.util
import io
import hashlib
import tarfile
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid

SPEC = importlib.util.spec_from_file_location("local_release_bundle", Path(__file__).with_name("local-release.py"))
m = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(m)


class PrepareBundle(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.home = Path(self.tmp.name).resolve()
        self.addCleanup(patch.stopall)
        patch.object(m, "HOME", self.home).start()
        self.candidate = self.home / "releases" / ("0.2.0-" + str(uuid.uuid4()))
        for d in ("bin", "web/dist", "ops"):
            (self.candidate / d).mkdir(parents=True)
        self.info = dict(version="0.2.0", commit="a" * 40, target="aarch64-apple-darwin", schema=24)
        self.manifest = dict(self.info, format_version=1, hub_version="0.2.0", web_version="0.2.0", schema={"target":24})
        (self.candidate / "release.json").write_text(json.dumps(self.manifest))
        (self.candidate / "web/dist/release.json").write_text(json.dumps(self.info))
        (self.candidate / "web/dist/index.html").write_text('<meta name="steve-web-version" content="0.2.0" />')
        for name in ("run.sh", "backup.sh", "schema_gate.py", "local-release.py"):
            (self.candidate / "ops" / name).write_text("runtime")
        self.hub = self.candidate / "bin/steve-hub"
        self.hub.write_bytes(b"official hub")
        (self.candidate / "bin/steve-keyhelper").write_bytes(b"new generic helper must not be installed")
        (self.home / "bin").mkdir()
        self.helper = self.home / "bin/steve-keyhelper"
        self.helper.write_bytes(b"original machine helper")
        (self.home / "signing").mkdir()
        (self.home / "signing/steve-signing.keychain-db").write_bytes(b"test placeholder")
        self.ident = dict(hub_identifier="test.hub")
        patch.object(m, "identity", return_value=self.ident).start()
        patch.object(m, "build_info", return_value=self.info).start()
        patch.object(m, "signing_password", return_value="test-password").start()
        patch.object(m, "signed_identifier", return_value="test.hub").start()
        self.sign = patch.object(m, "sign").start()
        self.args = SimpleNamespace(from_source=False, release=str(self.candidate), version="0.2.0", commit="a" * 40, keychain_pass_stdin=True)

    def test_signs_only_hub_without_compilation_or_helper_replacement(self):
        with patch.object(m.subprocess, "run", side_effect=AssertionError("no build or network")):
            self.assertEqual(m.prepare_bundle(self.args), 0)
        self.sign.assert_called_once_with(self.hub, self.home / "signing/steve-signing.keychain-db", "test-password", "test.hub")
        self.assertEqual(self.helper.read_bytes(), b"original machine helper")
        self.assertEqual(self.hub.read_bytes(), b"official hub")

    def test_rejects_current_outside_and_symlinked_candidates_before_signing(self):
        (self.home / "current").symlink_to(self.candidate)
        with self.assertRaisesRegex(m.Refused, "running"):
            m.prepare_bundle(self.args)
        (self.home / "current").unlink()
        self.args.release = str(self.home)
        with self.assertRaisesRegex(m.Refused, "direct child"):
            m.prepare_bundle(self.args)
        alias = self.home / "releases/alias"
        alias.symlink_to(self.candidate)
        self.args.release = str(alias)
        with self.assertRaisesRegex(m.Refused, "direct child"):
            m.prepare_bundle(self.args)
        self.sign.assert_not_called()

    def test_rejects_mismatched_manifest_web_and_binary(self):
        for filename in ("release.json", "web/dist/release.json"):
            with self.subTest(filename=filename):
                p = self.candidate / filename
                old = p.read_text()
                p.write_text(old.replace("0.2.0", "0.2.1"))
                with self.assertRaisesRegex(m.Refused, "identity differs"):
                    m.prepare_bundle(self.args)
                p.write_text(old)
        self.info["commit"] = "b" * 40
        with self.assertRaisesRegex(m.Refused, "Hub identity"):
            m.prepare_bundle(self.args)
        self.sign.assert_not_called()

    def test_refuses_to_create_missing_installed_helper(self):
        self.helper.unlink()
        with self.assertRaisesRegex(m.Refused, "helper is missing"):
            m.prepare_bundle(self.args)
        self.sign.assert_not_called()

    def test_rejects_symlink_inside_bundle(self):
        (self.candidate / "ops/secret").symlink_to(self.helper)
        with self.assertRaisesRegex(m.Refused, "symlink"):
            m.prepare_bundle(self.args)
        self.sign.assert_not_called()


class LegacyBundleBridge(PrepareBundle):
    def archive(self):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w:gz") as archive:
            for p in sorted(self.candidate.rglob("*")):
                archive.add(p, arcname=str(p.relative_to(self.candidate)), recursive=False)
        return output.getvalue()

    def test_legacy_build_downloads_verified_bundle_without_source_build(self):
        content = self.archive()
        asset = dict(target=self.info["target"], name="steve-0.2.0-aarch64-apple-darwin.tar.gz",
                     sha256=hashlib.sha256(content).hexdigest(), size=len(content))
        manifest = dict(self.manifest, assets=[asset])
        def download(url, destination=None, expected_size=None, expected_sha=None):
            self.assertTrue(url.startswith("https://github.com/Infranite/steve/releases/download/v0.2.0/"))
            if not destination:
                return manifest
            self.assertEqual((expected_size, expected_sha), (len(content), asset["sha256"]))
            destination.write_bytes(content)
        self.args.repo = "ignored-repository-path"
        with patch.dict(os.environ, {"STEVE_UPDATE_REPOSITORY": "Infranite/steve"}), \
                patch.object(m, "release_download", side_effect=download), \
                patch.object(m, "source_build", side_effect=AssertionError("no compilation")):
            self.assertEqual(m.build(self.args), 0)
        self.assertEqual(Path(self.args.release).name, "0.2.0-local-aaaaaaa")
        self.assertEqual(self.helper.read_bytes(), b"original machine helper")

    def test_source_build_requires_explicit_flag(self):
        self.args.from_source = True
        with patch.object(m, "source_build", return_value=0) as source, \
                patch.object(m, "release_download", side_effect=AssertionError("no download")):
            self.assertEqual(m.build(self.args), 0)
            source.assert_called_once_with(self.args)

    def test_bad_manifest_refused_before_candidate_created(self):
        with patch.object(m, "release_download", return_value=dict(self.manifest, commit="b" * 40)):
            with self.assertRaisesRegex(m.Refused, "identity differs"):
                m.build(self.args)
        self.assertFalse((self.home / "releases/0.2.0-local-aaaaaaa").exists())


class FirstMacInstall(PrepareBundle):
    def fresh(self):
        self.helper.unlink()
        for name in ("sign-setup.sh", "release-lib.sh"):
            (self.candidate / "ops" / name).write_text("runtime")
        self.args.label = "test.fresh.hub"
        patch.object(m.sys, "platform", "darwin").start()
        patch.object(m, "signed_identifier", side_effect=lambda p: m.PUBLIC_IDENTITY[
            "hub_identifier" if Path(p).name == "steve-hub" else "keyhelper_identifier"]).start()

    def test_fresh_setup_uses_precompiled_helper_and_is_idempotent(self):
        self.fresh()
        with patch.object(m.subprocess, "run") as run:
            self.assertEqual(m.setup_keychain(self.args), 0)
            self.assertEqual(run.call_args.args[0], ["/bin/zsh", str(self.candidate / "ops/sign-setup.sh")])
            self.assertEqual(self.helper.read_bytes(), b"new generic helper must not be installed")
            self.assertEqual(len(self.sign.call_args_list), 2)
            identity = json.loads((self.home / "signing/identity.json").read_text())
            self.assertEqual(identity["label"], "test.fresh.hub")
            self.assertEqual(identity["vault_service"], m.PUBLIC_IDENTITY["vault_service"])
            self.assertEqual(m.setup_keychain(self.args), 0)
            self.assertEqual(run.call_count, 1)
            self.assertEqual(len(self.sign.call_args_list), 2)

    def test_first_setup_never_replaces_existing_helper_or_data(self):
        self.args.label = "test.fresh.hub"
        patch.object(m.sys, "platform", "darwin").start()
        with patch.object(m.subprocess, "run") as run:
            with self.assertRaisesRegex(m.Refused, "already has"):
                m.setup_keychain(self.args)
            self.helper.unlink()
            (self.home / "data").mkdir()
            (self.home / "data/steve.sqlite3").write_bytes(b"existing database")
            with self.assertRaisesRegex(m.Refused, "already has"):
                m.setup_keychain(self.args)
            run.assert_not_called()
            self.sign.assert_not_called()


class ArchiveBoundary(unittest.TestCase):
    def test_download_checks_length_and_checksum(self):
        with tempfile.TemporaryDirectory() as tmp:
            for size, digest, message in ((2, "0" * 64, "exceeds"), (4, "0" * 64, "SHA256"), (3, "0" * 64, "SHA256")):
                opener = SimpleNamespace(open=lambda *a, **kw: io.BytesIO(b"abc"))
                path = Path(tmp) / str(size)
                with patch.object(m.urllib.request, "build_opener", return_value=opener):
                    with self.assertRaisesRegex(m.Refused, message):
                        m.release_download("https://github.com/example/asset", path, size, digest)

    def test_unsafe_members_never_escape_candidate(self):
        for name, kind in (("../outside", tarfile.REGTYPE), ("web/dist/link", tarfile.SYMTYPE),
                           ("ops/secret.env", tarfile.REGTYPE), ("bin/device", tarfile.CHRTYPE),
                           ("/absolute", tarfile.REGTYPE), ("web/dist/hard", tarfile.LNKTYPE)):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                archive = root / "bundle.tar.gz"
                with tarfile.open(archive, "w:gz") as output:
                    member = tarfile.TarInfo(name)
                    member.type = kind
                    member.linkname = "../../outside"
                    output.addfile(member, io.BytesIO())
                candidate = root / "candidate"
                candidate.mkdir()
                with self.assertRaises(m.Refused):
                    m.unpack_bundle(archive, candidate)
                self.assertFalse((root / "outside").exists())


if __name__ == "__main__":
    unittest.main()
