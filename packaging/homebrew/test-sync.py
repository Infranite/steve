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
from unittest import mock
import urllib.request


SOURCE = Path(__file__).parent / "tap"
spec = importlib.util.spec_from_file_location("homebrew_sync", SOURCE / "sync.py")
sync = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sync)
API = "https://api.github.com/repos/Infranite/steve/"


def bundle(manifest, change=None, target="aarch64-apple-darwin"):
    files = {name: (b"#!/bin/sh\nexit 0\n" if name.startswith(("bin/", "ops/")) else b"fixture\n")
             for name in sync.REQUIRED}
    files["release.json"] = sync.json_bytes(dict(manifest, assets=[]))
    files["bin/steve"] = (b"\xcf\xfa\xed\xfe\x0c\x00\x00\x01" + b"\0" * 24
                          if target == "aarch64-apple-darwin" else b"\x7fELF\x02\x01" + b"\0" * 12 + b"\x3e\x00" + b"\0" * 44)
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
        self.manifest = dict(format_version=1, cli_protocol=2, version=version, commit=commit,
                             hub_version=version, web_version=version, api_protocol=1,
                             schema=dict(min_read=24, max_read=24, target=24), assets=[])
        self.release = dict(draft=False, prerelease=False, published_at="2026-11-08T00:00:00Z",
                            tag_name="v" + version, assets=[])
        self.objects = {API + "git/ref/tags/v" + version: {"object": {"type": "commit", "sha": commit}}}
        for target in sync.TARGETS:
            name = f"steve-{version}-{target}-installer.tar.gz"
            data = bundle(self.manifest, change_bundle, target)
            self.manifest["assets"].append(dict(target=target, name=name, size=len(data), sha256=sync.digest(data)))
            self.add_remote(name, data)
        self.refresh_manifest()

    def add_remote(self, name, data):
        url = sync.release_base("Infranite/steve", self.manifest["version"]) + name
        existing = next((a for a in self.release["assets"] if a["name"] == name), None)
        asset_id = existing["id"] if existing else len(self.release["assets"]) + 1
        self.downloads[API + f"releases/assets/{asset_id}"] = data
        self.release["assets"] = [a for a in self.release["assets"] if a["name"] != name]
        self.release["assets"].append(dict(id=asset_id, name=name, state="uploaded", size=len(data),
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
        self.assertNotIn('depends_on "python', text)
        self.assertNotIn('ops/steve.py', text)
        self.assertIn('bin.install_symlink libexec/"bin/steve"', text)
        self.assertNotIn("post_install", text)
        self.assertNotIn("service do", text)
        self.assertNotIn("cargo", text)
        platforms = {"aarch64-apple-darwin": "macos", "x86_64-unknown-linux-gnu": "linux"}
        for asset in github.manifest["assets"]:
            block = text.split(f'  on_{platforms[asset["target"]]} do\n', 1)[1].split("\n  end", 1)[0]
            metadata = next(a for a in github.release["assets"] if a["name"] == asset["name"])
            url = API + f"releases/assets/{metadata['id']}"
            self.assertIn(f'url "{url}",', block)
            self.assertIn(f'mirror "{metadata["browser_download_url"]}"', block)
            self.assertIn(f'sha256 "{asset["sha256"]}"', block)
            self.assertEqual(json.loads(lock.read_text())["download_urls"][asset["target"]], url)
        self.assertEqual(text.count('    mirror "'), 2)
        self.assertEqual(text.count('headers: ["Accept: application/octet-stream"]'), 2)
        self.assertNotIn("Authorization", text)
        before = (formula.read_bytes(), lock.read_bytes(), formula.stat().st_mtime_ns, lock.stat().st_mtime_ns)
        archive_urls = {API + f"releases/assets/{a['id']}" for a in github.release["assets"]
                        if a["name"].endswith(".tar.gz")}
        archive_downloads = len([url for url in github.calls if url in archive_urls])
        self.assertEqual(set(github.calls), {API + f"releases/assets/{a['id']}" for a in github.release["assets"]})
        self.assertEqual(sync.sync(github, self.root), dict(changed=False, version="1.2.3"))
        self.assertEqual(before, (formula.read_bytes(), lock.read_bytes(), formula.stat().st_mtime_ns, lock.stat().st_mtime_ns))
        self.assertEqual(archive_downloads, 2)
        self.assertEqual(len([url for url in github.calls if url in archive_urls]), 2)

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
        cases = [lambda m: m.pop("cli_protocol"), lambda m: m.update(cli_protocol=1),
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
                 *(lambda g, value=value: g.release["assets"][0].update(id=value)
                   for value in (None, True, 0, -1, "1")),
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
                asset = next(a for a in github.release["assets"] if a["name"].endswith(suffix))
                url = API + f"releases/assets/{asset['id']}"
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
            item = tarfile.TarInfo("bin/steve")
            item.type, item.linkname = tarfile.SYMTYPE, "../../outside"
            files["bin/steve"] = item
        cases = [lambda files: files.pop("bin/steve"), wrong_identity, symlink,
                 lambda files: files.update({"bin/steve": b"#!/usr/bin/env python3\n"}),
                 lambda files: files.update({"bin/steve": b"\x7fELF\x02\x01" + b"\0" * 12 + b"\xb7\x00"}),
                 lambda files: files.update({"ops/upgrade.py": b"legacy bridge must stay in runtime\n"}),
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
        replaced_asset = FakeGitHub()
        replaced_asset.release["assets"][0]["id"] += 100
        candidates.append(replaced_asset)
        for github in candidates:
            with self.subTest(version=github.manifest["version"], commit=github.manifest["commit"]):
                with self.assertRaises(sync.Refused):
                    sync.sync(github, self.root)
                self.assertEqual(before, {name: (self.root / name).read_bytes() for name in before})
        self.assertEqual(sync.sync(FakeGitHub(version="1.2.4", commit="b" * 40), self.root),
                         dict(changed=True, version="1.2.4"))

    def test_a_legacy_protocol_one_lock_can_advance_to_a_new_native_release(self):
        sync.sync(FakeGitHub(), self.root)
        lock_path = self.root / sync.LOCK_NAME
        lock = json.loads(lock_path.read_text())
        lock["manifest"]["cli_protocol"] = 1
        lock_path.write_bytes(sync.json_bytes(lock))
        self.assertEqual(sync.sync(FakeGitHub(version="1.2.4"), self.root), dict(changed=True, version="1.2.4"))
        self.assertEqual(json.loads(lock_path.read_text())["manifest"]["cli_protocol"], 2)

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


class DownloadTests(unittest.TestCase):
    def test_api_asset_request_uses_binary_accept_while_metadata_uses_json(self):
        client = sync.GitHub()
        requests = []
        def opened(request, **kwargs):
            requests.append(request)
            return io.BytesIO(b"{}")
        with mock.patch.object(client.opener, "open", side_effect=opened), \
                mock.patch.dict(sync.os.environ, {"GITHUB_TOKEN": "test-token"}):
            self.assertEqual(client.download(API + "releases/assets/123", 2), b"{}")
            self.assertEqual(client.json(API + "releases/latest"), {})
            self.assertEqual(client.download("https://release-assets.githubusercontent.com/file", 2), b"{}")
        self.assertEqual([request.get_header("Accept") for request in requests],
                         ["application/octet-stream", "application/vnd.github+json", "application/octet-stream"])
        self.assertEqual([request.get_header("Authorization") for request in requests],
                         ["Bearer test-token", "Bearer test-token", None])

    def test_redirect_strips_token_before_storage_and_never_restores_it(self):
        handler = sync.Redirect()
        request = urllib.request.Request(API + "releases/assets/123",
                                         headers={"Authorization": "Bearer test-token",
                                                  "Accept": "application/octet-stream"})
        same_host = handler.redirect_request(request, None, 302, "Found", {}, API + "releases/assets/456")
        self.assertEqual(same_host.get_header("Authorization"), "Bearer test-token")
        storage = handler.redirect_request(request, None, 302, "Found", {},
                                           "https://release-assets.githubusercontent.com/file")
        self.assertIsNone(storage.get_header("Authorization"))
        self.assertEqual(storage.get_header("Accept"), "application/octet-stream")
        returned = handler.redirect_request(storage, None, 302, "Found", {}, API + "releases/assets/123")
        self.assertIsNone(returned.get_header("Authorization"))
        self.assertEqual(request.get_header("Authorization"), "Bearer test-token")
        for url in ("http://api.github.com/file", "https://example.test/file",
                    "https://user@github.com/file", "https://github.com/file#fragment",
                    "https://api.github.com:444/file", "https://api.github.com:invalid/file"):
            with self.subTest(url=url), self.assertRaises(sync.Refused):
                handler.redirect_request(request, None, 302, "Found", {}, url)


if __name__ == "__main__":
    unittest.main()
