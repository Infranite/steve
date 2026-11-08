#!/usr/bin/env python3
"""Exercise publication and installer integrity gates with a fake GitHub API."""
import copy
import importlib.util
import io
import json
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest


SOURCE = Path(__file__).parent / "tap"
spec = importlib.util.spec_from_file_location("homebrew_sync", SOURCE / "sync.py")
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)
API = "https://api.github.com/repos/Infranite/steve/"


def bundle(manifest, change=None):
    files = {name: (b"#!/bin/sh\nexit 0\n" if name.startswith(("bin/", "ops/")) else b"fixture\n")
             for name in sync.REQUIRED}
    files["release.json"] = sync.json_bytes(dict(manifest, assets=[]))
    files["web/dist/release.json"] = sync.json_bytes({k: manifest[k] for k in ("version", "commit")})
    if change:
        change(files)
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as archive:
        for name, data in sorted(files.items()):
            item = tarfile.TarInfo(name)
            item.mode = 0o755 if name.startswith(("bin/", "ops/")) else 0o644
            if isinstance(data, tarfile.TarInfo):
                archive.addfile(data)
            else:
                item.size = len(data)
                archive.addfile(item, io.BytesIO(data))
    return output.getvalue()


class FakeGitHub:
    def __init__(self, version="1.2.3", commit="a" * 40, change_bundle=None):
        self.downloads, self.calls = {}, []
        self.manifest = dict(format_version=1, cli_protocol=1, version=version, commit=commit,
                             hub_version=version, web_version=version, api_protocol=1,
                             schema=dict(min_read=24, max_read=24, target=24), assets=[])
        self.release = dict(draft=False, prerelease=False, published_at="2026-11-08T00:00:00Z",
                            tag_name="v" + version, assets=[])
        self.objects = {API + "git/ref/tags/v" + version: {"object": {"type": "commit", "sha": commit}}}
        for target in sync.TARGETS:
            name = f"steve-{version}-{target}-installer.tar.gz"
            data = bundle(self.manifest, change_bundle)
            self.manifest["assets"].append(dict(target=target, name=name, size=len(data), sha256=sync.digest(data)))
            self.add_remote(name, data)
        self.refresh_manifest()

    def add_remote(self, name, data):
        url = sync.release_base("Infranite/steve", self.manifest["version"]) + name
        self.downloads[url] = data
        self.release["assets"] = [a for a in self.release["assets"] if a["name"] != name]
        self.release["assets"].append(dict(name=name, state="uploaded", size=len(data),
                                           digest="sha256:" + sync.digest(data), browser_download_url=url))

    def refresh_manifest(self):
        self.add_remote(sync.MANIFEST_NAME, sync.json_bytes(self.manifest))

    def json(self, url):
        return copy.deepcopy(self.release if url == API + "releases/latest" else self.objects[url])

    def download(self, url, limit, destination=None):
        self.calls.append(url)
        data = self.downloads[url]
        sync.require(len(data) <= limit, "download exceeds its declared size limit")
        if destination:
            destination.write_bytes(data)
            return len(data), sync.digest(data)
        return data


class SyncTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        shutil.copyfile(SOURCE / "steve.rb.in", self.root / "steve.rb.in")

    def untouched(self):
        self.assertFalse((self.root / "Formula").exists())
        self.assertFalse((self.root / sync.LOCK_NAME).exists())

    def test_complete_release_generates_binary_only_formula_and_repeat_is_noop(self):
        github = FakeGitHub()
        self.assertEqual(sync.sync(github, self.root), dict(changed=True, version="1.2.3"))
        formula, lock = self.root / "Formula/steve.rb", self.root / sync.LOCK_NAME
        text = formula.read_text()
        self.assertIn('version "1.2.3"', text)
        self.assertIn('depends_on "python@3.13"', text)
        self.assertIn('#{libexec}/ops/steve.py', text)
        self.assertIn('(bin/"steve").chmod 0755', text)
        self.assertNotIn("post_install", text)
        self.assertNotIn("service do", text)
        self.assertNotIn("cargo", text)
        for asset in github.manifest["assets"]:
            self.assertIn(asset["sha256"], text)
            self.assertIn("/v1.2.3/" + asset["name"], text)
        before = (formula.read_bytes(), lock.read_bytes(), formula.stat().st_mtime_ns, lock.stat().st_mtime_ns)
        archive_downloads = len([url for url in github.calls if url.endswith(".tar.gz")])
        self.assertEqual(sync.sync(github, self.root), dict(changed=False, version="1.2.3"))
        self.assertEqual(before, (formula.read_bytes(), lock.read_bytes(), formula.stat().st_mtime_ns, lock.stat().st_mtime_ns))
        self.assertEqual(archive_downloads, 2)
        self.assertEqual(len([url for url in github.calls if url.endswith(".tar.gz")]), 2)

    def test_unpublished_prerelease_or_legacy_release_cannot_create_a_formula(self):
        cases = [lambda g: g.release.update(draft=True), lambda g: g.release.update(prerelease=True),
                 lambda g: g.release.update(published_at=None), lambda g: g.release.update(tag_name="v1.2.3-rc1"),
                 lambda g: g.release.update(assets=[])]
        for change in cases:
            with self.subTest(change=change):
                github = FakeGitHub()
                change(github)
                with self.assertRaises(sync.Refused):
                    sync.sync(github, self.root)
                self.untouched()

    def test_protocol_and_complete_platform_contract_are_required(self):
        cases = [lambda m: m.pop("cli_protocol"), lambda m: m.update(cli_protocol=2),
                 lambda m: m.update(cli_protocol=True), lambda m: m.update(version="01.2.3"),
                 lambda m: m["assets"].pop(), lambda m: m["assets"].append(m["assets"][0]),
                 lambda m: m["assets"][1].update(target=m["assets"][0]["target"])]
        for change in cases:
            with self.subTest(change=change):
                github = FakeGitHub()
                change(github.manifest)
                github.refresh_manifest()
                with self.assertRaises(sync.Refused):
                    sync.sync(github, self.root)
                self.untouched()

    def test_tag_commit_and_published_asset_metadata_must_match(self):
        def bad_commit(g):
            g.objects[API + "git/ref/tags/v1.2.3"]["object"]["sha"] = "b" * 40
        cases = [bad_commit, lambda g: g.release["assets"][0].update(state="new"),
                 lambda g: g.release["assets"][0].update(digest="sha256:" + "0" * 64),
                 lambda g: g.release["assets"][0].update(size=100),
                 lambda g: g.release["assets"][0].update(browser_download_url="https://example.test/bundle"),
                 lambda g: g.release["assets"].append(copy.deepcopy(g.release["assets"][0]))]
        for change in cases:
            with self.subTest(change=change):
                github = FakeGitHub()
                change(github)
                with self.assertRaises(sync.Refused):
                    sync.sync(github, self.root)
                self.untouched()

    def test_annotated_tag_resolves_to_the_manifest_commit(self):
        github = FakeGitHub()
        github.objects[API + "git/ref/tags/v1.2.3"] = {"object": {"type": "tag", "sha": "b" * 40}}
        github.objects[API + "git/tags/" + "b" * 40] = {"object": {"type": "commit", "sha": "a" * 40}}
        self.assertTrue(sync.sync(github, self.root)["changed"])

    def test_actual_archive_and_manifest_bytes_are_verified_before_any_write(self):
        for suffix in (sync.MANIFEST_NAME, ".tar.gz"):
            with self.subTest(suffix=suffix):
                github = FakeGitHub()
                url = next(url for url in github.downloads if url.endswith(suffix))
                original = github.downloads[url]
                github.downloads[url] = original[:-1] + bytes([original[-1] ^ 1])
                with self.assertRaises(sync.Refused):
                    sync.sync(github, self.root)
                self.untouched()

    def test_bundle_content_must_contain_the_cli_and_matching_identity_without_unsafe_members(self):
        def wrong_identity(files):
            manifest = json.loads(files["release.json"])
            manifest["commit"] = "b" * 40
            files["release.json"] = sync.json_bytes(manifest)
        def symlink(files):
            item = tarfile.TarInfo("ops/steve.py")
            item.type, item.linkname = tarfile.SYMTYPE, "../../outside"
            files["ops/steve.py"] = item
        cases = [lambda files: files.pop("ops/steve.py"), wrong_identity, symlink,
                 lambda files: files.update({"../escape": b"outside"})]
        for change in cases:
            with self.subTest(change=change):
                github = FakeGitHub(change_bundle=change)
                with self.assertRaises(sync.Refused):
                    sync.sync(github, self.root)
                self.untouched()

    def test_downgrades_and_changes_under_the_same_version_leave_the_existing_formula_alone(self):
        sync.sync(FakeGitHub(), self.root)
        before = {name: (self.root / name).read_bytes() for name in ("Formula/steve.rb", sync.LOCK_NAME)}
        candidates = [FakeGitHub(version="1.2.2"), FakeGitHub(commit="b" * 40),
                      FakeGitHub(change_bundle=lambda files: files.update({"LICENSE": b"different license\n"}))]
        for github in candidates:
            with self.subTest(version=github.manifest["version"], commit=github.manifest["commit"]):
                with self.assertRaises(sync.Refused):
                    sync.sync(github, self.root)
                self.assertEqual(before, {name: (self.root / name).read_bytes() for name in before})
        self.assertEqual(sync.sync(FakeGitHub(version="1.2.4", commit="b" * 40), self.root),
                         dict(changed=True, version="1.2.4"))

    def test_a_locally_changed_or_partially_installed_formula_is_not_overwritten(self):
        sync.sync(FakeGitHub(), self.root)
        formula = self.root / "Formula/steve.rb"
        formula.write_text("local changes\n")
        with self.assertRaisesRegex(sync.Refused, "verified lock"):
            sync.sync(FakeGitHub(version="1.2.4"), self.root)
        self.assertEqual(formula.read_text(), "local changes\n")
        (self.root / sync.LOCK_NAME).unlink()
        with self.assertRaisesRegex(sync.Refused, "installed together"):
            sync.sync(FakeGitHub(), self.root)


if __name__ == "__main__":
    unittest.main()
