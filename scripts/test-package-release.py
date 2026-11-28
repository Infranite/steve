#!/usr/bin/env python3
"""Exercise distribution boundary and artifact validation without compiling a Hub."""
import importlib.util
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package-release.py"))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)
tap_spec = importlib.util.spec_from_file_location(
    "homebrew_sync", Path(__file__).parents[1] / "packaging/homebrew/tap/sync.py")
tap = importlib.util.module_from_spec(tap_spec)
tap_spec.loader.exec_module(tap)


class BundleTests(unittest.TestCase):
    def setUp(self):
        def stage_restic(_target, out):
            (out / "restic").write_bytes(b"backup engine")
            (out / "restic").chmod(0o755)
            (out / "restic.LICENSE").write_bytes(b"license")

        download = patch.object(release, "restic", stage_restic)
        download.start()
        self.addCleanup(download.stop)
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
        self.cli = root / "steve"
        self.cli.write_text(self.binary.read_text())
        self.cli.chmod(0o755)
        self.helper = root / "helper"
        self.helper.write_bytes(b"helper")
        (root / "private.env").write_text("must not enter release")
        self.out = root / "out"
        self.args = SimpleNamespace(version="0.2.0", commit="a" * 40,
            target="aarch64-apple-darwin", schema=24, binary=str(self.binary), node=str(self.node), cli=str(self.cli), keyhelper=str(self.helper), web=str(self.web), out=str(self.out))

    def test_bundle_contains_only_selected_outputs_and_consistent_version(self):
        release.package(self.args)
        manifest = self.out / "manifest-aarch64-apple-darwin.json"
        release.merge(SimpleNamespace(out=str(self.out / "steve-release.json"), manifests=[str(manifest)]))
        metadata = json.loads((self.out / "steve-release.json").read_text())
        with tarfile.open(self.out / metadata["assets"][0]["name"]) as bundle:
            names = bundle.getnames()
            self.assertNotIn("private.env", names)
            self.assertEqual(bundle.getmember("bin/steve-node").mode, 0o755)
            self.assertEqual(bundle.getmember("bin/steve-keyhelper").mode, 0o755)
            self.assertEqual([n for n in names if n == "ops" or n.startswith("ops/")], [])
            self.assertEqual([n for n in names if n.endswith((".py", ".sh"))], [])
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

    def test_runtime_and_installer_include_the_backup_engine(self):
        self.args.installer = True
        release.package(self.args)
        runtime = json.loads((self.out / "manifest-aarch64-apple-darwin.json").read_text())
        installer = json.loads((self.out / "installer-manifest-aarch64-apple-darwin.json").read_text())
        for manifest in (runtime, installer):
            with tarfile.open(self.out / manifest["assets"][0]["name"]) as bundle:
                self.assertEqual(bundle.getmember("bin/restic").mode, 0o755)
                self.assertEqual(bundle.extractfile("bin/restic").read(), b"backup engine")
                self.assertEqual(bundle.extractfile("bin/restic.LICENSE").read(), b"license")
        with tarfile.open(self.out / installer["assets"][0]["name"]) as bundle:
            names = bundle.getnames()
            self.assertEqual([name for name in names if name == "ops" or name.startswith("ops/")], [])
            self.assertEqual([name for name in names if name.endswith((".py", ".sh"))], [])
            self.assertEqual(bundle.getmember("bin/steve").mode, 0o755)
            self.assertEqual(bundle.extractfile("bin/steve").read(), self.cli.read_bytes())
            self.assertEqual(bundle.getmember("LICENSE").mode, 0o644)
            self.assertEqual(bundle.extractfile("LICENSE").read(), (Path(__file__).parents[1] / "LICENSE").read_bytes())
            installed = json.load(bundle.extractfile("release.json"))
            self.assertEqual(installed["cli_protocol"], 2)
            self.assertEqual(installed["assets"], [])
            self.assertEqual(dict(installed, assets=[]), dict(installer, assets=[]))

    def test_official_archives_preserve_the_staged_signed_bytes(self):
        self.args.installer = True
        self.args.restic_dir = stage = Path(self.tmp.name) / "signed-engine"
        stage.mkdir()
        engine = stage / "restic"
        engine.write_bytes(b"signed engine, must not be replaced by fresh download")
        engine.chmod(0o755)
        (stage / "restic.LICENSE").write_text("upstream license")
        with patch.object(release, "restic", side_effect=AssertionError("downloaded after signing")):
            release.package(self.args)
        for archive in self.out.glob("*.tar.gz"):
            with tarfile.open(archive) as bundle:
                self.assertEqual(bundle.extractfile("bin/restic").read(), engine.read_bytes())
                self.assertEqual(bundle.getmember("bin/restic").mode, 0o755)
        engine.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "not executable"):
            release.package(self.args)

    def test_packaged_script_free_installer_passes_tap_verification_and_rejects_scripts(self):
        self.args.installer = True
        # Only build identity execution is stubbed. Real archives, manifests and the tap's
        # native-header, file inventory and identity checks run against the packaged output.
        header = b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01" + b"\0" * 24
        for binary in (self.binary, self.node, self.cli):
            binary.write_bytes(header)
        identity = json.dumps(dict(version=self.args.version, commit=self.args.commit,
                                   schema=self.args.schema, target=self.args.target)).encode()
        with patch.object(release.subprocess, "check_output", return_value=identity):
            release.package(self.args)
        manifest = json.loads((self.out / "installer-manifest-aarch64-apple-darwin.json").read_text())
        archive = self.out / manifest["assets"][0]["name"]
        with tarfile.open(archive) as bundle:
            self.assertEqual([n for n in bundle.getnames() if n.endswith((".py", ".sh")) or n.startswith("ops/")], [])
        tap.verify_bundle(archive, manifest, self.args.target)
        # Tap verification rejects any legacy runtime script reintroduced into an installer archive.
        with_script = self.out / "legacy-script-installer.tar.gz"
        with tarfile.open(archive) as source, tarfile.open(with_script, "w:gz") as destination:
            for member in source:
                destination.addfile(member, source.extractfile(member) if member.isfile() else None)
            script = b"#!/bin/sh\nexit 0\n"
            member = tarfile.TarInfo("ops/run.sh")
            member.mode, member.size = 0o755, len(script)
            destination.addfile(member, io.BytesIO(script))
        with self.assertRaisesRegex(tap.Refused, "unexpected installer member"):
            tap.verify_bundle(with_script, manifest, self.args.target)
        # Packager also refuses stray .sh/.py files placed under web/dist.
        (self.web / "stray.sh").write_text("#!/bin/sh\n")
        with self.assertRaisesRegex(ValueError, "runtime script forbidden"):
            with patch.object(release.subprocess, "check_output", return_value=identity):
                release.package(self.args)

    def test_installer_merge_requires_matching_protocol_and_both_platforms(self):
        self.args.installer = True
        release.package(self.args)
        mac = self.out / "installer-manifest-aarch64-apple-darwin.json"
        args = SimpleNamespace(out=str(self.out / "steve-installer.json"), manifests=[str(mac)],
                               expected_target=["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"])
        with self.assertRaisesRegex(ValueError, "missing expected platforms"):
            release.merge(args)
        self.args.target = "x86_64-unknown-linux-gnu"
        self.binary.write_text(self.binary.read_text().replace("aarch64-apple-darwin", self.args.target))
        self.node.write_text(self.binary.read_text())
        self.cli.write_text(self.binary.read_text())
        release.package(self.args)
        linux = self.out / "installer-manifest-x86_64-unknown-linux-gnu.json"
        args.manifests.append(str(linux))
        release.merge(args)
        self.assertEqual(len(json.loads(Path(args.out).read_text())["assets"]), 2)
        args.manifests[1] = str(self.out / "manifest-x86_64-unknown-linux-gnu.json")
        with self.assertRaisesRegex(ValueError, "disagree"):
            release.merge(args)

    def test_installer_requires_the_cli_and_refuses_symlinked_license(self):
        self.args.installer = True
        self.args.cli = None
        with self.assertRaisesRegex(ValueError, "prebuilt --cli binary and LICENSE"):
            release.package(self.args)
        self.args.cli = str(self.out / "missing-cli")
        with self.assertRaisesRegex(ValueError, "prebuilt --cli binary and LICENSE"):
            release.package(self.args)
        self.args.cli = str(self.cli)
        self.args.license = str(self.out / "missing-license")
        with self.assertRaisesRegex(ValueError, "prebuilt --cli binary and LICENSE"):
            release.package(self.args)
        license_link = Path(self.tmp.name) / "license-link"
        license_link.symlink_to(Path(__file__).parents[1] / "LICENSE")
        self.args.license = str(license_link)
        with self.assertRaisesRegex(ValueError, "symlink"):
            release.package(self.args)

    def test_installer_refuses_wrong_cli_identity_or_symlink(self):
        self.args.installer = True
        self.cli.write_text(self.binary.read_text().replace("a" * 40, "b" * 40))
        with self.assertRaisesRegex(ValueError, "CLI build identity"):
            release.package(self.args)
        self.cli.unlink()
        self.cli.symlink_to(self.binary)
        with self.assertRaisesRegex(ValueError, "symlink"):
            release.package(self.args)

    def _package_native_stub(self):
        self.args.installer = True
        header = b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01" + b"\0" * 24
        for binary in (self.binary, self.node, self.cli):
            binary.write_bytes(header)
        identity = json.dumps(dict(version=self.args.version, commit=self.args.commit,
                                   schema=self.args.schema, target=self.args.target)).encode()
        with patch.object(release.subprocess, "check_output", return_value=identity):
            release.package(self.args)
        return identity

    def test_verify_checks_unpacked_runtime_and_installer_without_mutating_out(self):
        identity = self._package_native_stub()
        stub_script = Path(self.tmp.name) / "fake-identity.py"
        record = Path(self.tmp.name) / "called.json"
        stub_script.write_text(
            "import json, pathlib, sys\n"
            "args = sys.argv[1:]\n"
            "web = pathlib.Path(args[args.index('--web') + 1])\n"
            "root = pathlib.Path(args[0]).parents[1]\n"
            "for rel in ('bin/steve-hub', 'bin/steve-node', 'bin/restic', 'web/dist/index.html', 'web/dist/release.json'):\n"
            "    assert (root / rel).is_file(), rel\n"
            "assert (web / 'index.html').is_file()\n"
            f"pathlib.Path({str(record)!r}).write_text(json.dumps(args))\n"
        )
        before = sorted(p.name for p in self.out.iterdir())
        verify_args = SimpleNamespace(
            version=self.args.version,
            commit=self.args.commit,
            target=self.args.target,
            out=str(self.out),
            installer=True,
            identity_script=str(stub_script),
        )
        seen_cmds = []

        def fake_check_output(cmd, timeout=15):
            seen_cmds.append(cmd)
            return identity

        with patch.object(release.subprocess, "check_output", side_effect=fake_check_output):
            release.verify(verify_args)

        self.assertEqual(sorted(p.name for p in self.out.iterdir()), before)
        called = json.loads(record.read_text())
        self.assertIn("--web", called)
        self.assertIn(self.args.version, called)
        self.assertIn(self.args.commit, called)
        self.assertIn(self.args.target, called)
        self.assertEqual([Path(c[0]).name for c in seen_cmds], ["steve-node", "steve-hub", "steve"])

    def test_verify_rejects_corrupt_archive_wrong_identity_and_failed_boot(self):
        identity = self._package_native_stub()
        stub_script = Path(self.tmp.name) / "ok-identity.py"
        stub_script.write_text("import sys; sys.exit(0)\n")
        base_args = dict(
            version=self.args.version,
            commit=self.args.commit,
            target=self.args.target,
            out=str(self.out),
            installer=True,
            identity_script=str(stub_script),
        )
        with patch.object(release.subprocess, "check_output", return_value=identity):
            with self.assertRaisesRegex(ValueError, "does not match release identity"):
                release.verify(SimpleNamespace(**dict(base_args, commit="b" * 40)))

            fail_script = Path(self.tmp.name) / "fail-identity.py"
            fail_script.write_text("import sys; sys.exit(2)\n")
            with self.assertRaises(subprocess.CalledProcessError):
                release.verify(SimpleNamespace(**dict(base_args, identity_script=str(fail_script))))

            installer_tar = self.out / f"steve-{self.args.version}-{self.args.target}-installer.tar.gz"
            saved_installer = installer_tar.read_bytes()
            installer_tar.write_bytes(b"corrupt")
            with self.assertRaisesRegex(ValueError, "does not match"):
                release.verify(SimpleNamespace(**base_args))
            installer_tar.write_bytes(saved_installer)

            runtime_tar = self.out / f"steve-{self.args.version}-{self.args.target}.tar.gz"
            runtime_tar.write_bytes(b"corrupt")
            with self.assertRaisesRegex(ValueError, "does not match"):
                release.verify(SimpleNamespace(**base_args))


if __name__ == "__main__":
    unittest.main()
