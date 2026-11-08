#!/usr/bin/env python3
"""Pinned installer tests: fake HTTP/processes and temporary directories only."""
import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
import urllib.error
import urllib.request
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("install_nextest", Path(__file__).with_name("install-nextest.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


def bundle(entries=None):
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w:gz") as archive:
        for name, kind, data in entries or [("cargo-nextest", tarfile.REGTYPE, b"test executable")]:
            member = tarfile.TarInfo(name)
            member.type = kind
            member.size = len(data) if kind == tarfile.REGTYPE else 0
            member.linkname = "../outside"
            archive.addfile(member, io.BytesIO(data) if kind == tarfile.REGTYPE else None)
    return out.getvalue()


class Response(io.BytesIO):
    def __init__(self, data, headers=None):
        super().__init__(data)
        self.headers = headers if headers is not None else {"Content-Length": str(len(data))}
        self.status = 200

    def geturl(self):
        return "https://release-assets.githubusercontent.com/asset?signature=not-for-logs"


class Network:
    def __init__(self, responses):
        self.responses = list(responses)
        self.requests = []

    def open(self, request, **kwargs):
        self.requests.append(request)
        item = self.responses.pop(0)
        if isinstance(item, Exception):
            raise item
        return Response(*item) if isinstance(item, tuple) else Response(item)


class Installer(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="steve-nextest-test-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.parent = self.root / "temporary parent"
        self.parent.mkdir()
        self.github_path = self.root / "github-path"
        self.github_path.write_text("/retained/path\n")
        self.data = bundle()
        self.asset = dict(m.select_asset("Darwin", "arm64"), size=len(self.data), sha256=hashlib.sha256(self.data).hexdigest())
        self.calls = []

    def runner(self, args, **kwargs):
        self.calls.append((args, kwargs))
        return subprocess.CompletedProcess(args, 0, "cargo-nextest 0.9.151 (6b3f99457 2026-11-02)\nrelease: 0.9.151\n", "")

    def install(self, network=None, runner=None, data=None):
        with contextlib.redirect_stdout(io.StringIO()):
            return m.install(self.parent, self.github_path, asset=self.asset,
                             opener=network or Network([data or self.data]), runner=runner or self.runner, token="test-token")

    def test_fixed_platforms_and_metadata(self):
        mac = m.select_asset("Darwin", "arm64")
        self.assertEqual(mac, m.select_asset("Darwin", "x86_64"))
        self.assertEqual((mac["id"], mac["size"]), (703616437, 16809298))
        linux = m.select_asset("Linux", "x86_64")
        self.assertEqual((linux["id"], linux["size"]), (703600788, 12161169))
        self.assertTrue(linux["name"].endswith("x86_64-unknown-linux-gnu.tar.gz"))
        for system, machine in (("Linux", "aarch64"), ("Windows", "x86_64")):
            with self.assertRaises(m.Refused):
                m.select_asset(system, machine)

    def test_success_publishes_only_verified_temporary_binary_and_path(self):
        before = os.environ.get("PATH")
        target = self.install()
        self.assertEqual(target.parent, self.parent)
        self.assertEqual([p.name for p in target.iterdir()], ["cargo-nextest"])
        self.assertEqual((target / "cargo-nextest").read_bytes(), b"test executable")
        self.assertEqual((target / "cargo-nextest").stat().st_mode & 0o777, 0o755)
        self.assertEqual(self.github_path.read_text(), "/retained/path\n" + str(target) + "\n")
        self.assertEqual(os.environ.get("PATH"), before)
        args, kwargs = self.calls[0]
        self.assertEqual(args, [str(target / "cargo-nextest"), "nextest", "--version"])
        self.assertNotIn("GITHUB_TOKEN", kwargs["env"])
        self.assertNotIn("test-token", repr(kwargs))
        self.assertFalse((self.root / ".cargo").exists())
        self.assertFalse((self.root / ".steve").exists())

    def test_api_then_public_fallback_uses_fresh_queries_without_credentials(self):
        failure = urllib.error.HTTPError("https://api.github.com/?secret", 504, "gateway", {}, None)
        network = Network([failure, urllib.error.URLError("signed URL must not be logged"), self.data])
        nonces = iter(("one", "two"))
        dest = self.root / "archive"
        route = m.download(self.asset, dest, opener=network, token="test-token", nonce=lambda: next(nonces))
        self.assertEqual(route, "public-2")
        self.assertEqual(dest.read_bytes(), self.data)
        self.assertEqual(network.requests[0].get_header("Authorization"), "Bearer test-token")
        for request in network.requests[1:]:
            self.assertIsNone(request.get_header("Authorization"))
            self.assertTrue(request.full_url.startswith("https://github.com/nextest-rs/nextest/releases/download/"))
        self.assertTrue(network.requests[1].full_url.endswith("?steve_ci=one"))
        self.assertTrue(network.requests[2].full_url.endswith("?steve_ci=two"))

    def test_redirect_removes_authorization_at_non_api_host_and_never_readds_it(self):
        handler = m.GitHubRedirect()
        original = urllib.request.Request("https://api.github.com/repos/nextest-rs/nextest/releases/assets/1", headers={"Authorization":"Bearer test-token"})
        same = handler.redirect_request(original, None, 302, "", {}, "https://api.github.com/another")
        self.assertEqual(same.get_header("Authorization"), "Bearer test-token")
        public = handler.redirect_request(original, None, 302, "", {}, "https://github.com/file")
        self.assertIsNone(public.get_header("Authorization"))
        storage = handler.redirect_request(original, None, 302, "", {}, "https://release-assets.githubusercontent.com/file?signature=x")
        self.assertIsNone(storage.get_header("Authorization"))
        back = handler.redirect_request(storage, None, 302, "", {}, "https://api.github.com/back")
        self.assertIsNone(back.get_header("Authorization"))

    def test_redirect_rejects_non_https_userinfo_ports_and_lookalike_domains(self):
        handler = m.GitHubRedirect()
        req = urllib.request.Request("https://api.github.com/start")
        for url in ("http://github.com/a", "https://github.com.evil.test/a", "https://x.githubusercontent.com.evil.test/a", "https://user@github.com/a", "https://github.com:444/a", "https://evil.test/a?signature=secret"):
            with self.subTest(url=url), self.assertRaises(m.Refused):
                handler.redirect_request(req, None, 302, "", {}, url)

    def test_wrong_length_hash_truncation_and_oversize_never_execute_or_publish(self):
        variants = [(self.data, {"Content-Length":"1"}), (self.data[:-1], {}), (self.data + b"excess", {}), (b"x" * len(self.data), {})]
        for payload in variants:
            with self.subTest(payload=payload[1]):
                network = Network([payload, payload, payload])
                with self.assertRaises(m.Refused):
                    self.install(network)
                self.assertEqual(self.calls, [])
                self.assertEqual(self.github_path.read_text(), "/retained/path\n")
                self.assertEqual(list(self.parent.iterdir()), [])

    def test_slow_stream_obeys_deadline(self):
        times = iter((0, m.DOWNLOAD_SECONDS + 1, 0, m.DOWNLOAD_SECONDS + 1, 0, m.DOWNLOAD_SECONDS + 1))
        with self.assertRaisesRegex(m.Refused, "time limit"):
            m.download(self.asset, self.root / "archive", opener=Network([self.data] * 3), now=lambda: next(times))
        self.assertFalse((self.root / "archive").exists())

    def test_interrupted_http_falls_back_and_binary_size_is_bounded(self):
        network = Network([m.http.client.IncompleteRead(b"partial"), self.data])
        self.install(network)
        self.assertEqual(len(network.requests), 2)
        archive = self.root / "large.tar.gz"
        archive.write_bytes(self.data)
        with patch.object(m, "MAX_BINARY", 1), self.assertRaises(m.Refused):
            m.extract_binary(archive, self.root / "cargo-nextest")
        self.assertFalse((self.root / "cargo-nextest").exists())

    def test_tar_rejects_links_paths_duplicates_extra_files_and_empty_archives(self):
        cases = [
            [("../cargo-nextest", tarfile.REGTYPE, b"x")],
            [("/cargo-nextest", tarfile.REGTYPE, b"x")],
            [("dir/cargo-nextest", tarfile.REGTYPE, b"x")],
            [("cargo-nextest", tarfile.SYMTYPE, b"")],
            [("cargo-nextest", tarfile.LNKTYPE, b"")],
            [("cargo-nextest", tarfile.DIRTYPE, b"")],
            [("cargo-nextest", tarfile.REGTYPE, b"x"), ("cargo-nextest", tarfile.REGTYPE, b"y")],
            [("cargo-nextest", tarfile.REGTYPE, b"x"), ("extra", tarfile.REGTYPE, b"y")],
            [("cargo-nextest", tarfile.REGTYPE, b"")],
        ]
        for entries in cases:
            with self.subTest(entries=entries):
                archive = self.root / "bad.tar.gz"
                archive.write_bytes(bundle(entries))
                with self.assertRaises(m.Refused):
                    m.extract_binary(archive, self.root / "cargo-nextest")
                self.assertFalse((self.root / "cargo-nextest").exists())
        empty = self.root / "empty.tar.gz"
        with tarfile.open(empty, "w:gz"):
            pass
        with self.assertRaises(m.Refused):
            m.extract_binary(empty, self.root / "cargo-nextest")

    def test_wrong_or_failed_version_keeps_path_unchanged_and_removes_temporary_files(self):
        for code, output in ((0, "cargo-nextest 0.9.152\n"), (0, "cargo-nextest 0.9.151-beta\n"), (1, "cargo-nextest 0.9.151\n"), (0, "cargo-nextest 0.9.151\nrelease: 9.9.9\n")):
            with self.subTest(code=code, output=output):
                with self.assertRaises(m.Refused):
                    self.install(runner=lambda args, **kw: subprocess.CompletedProcess(args, code, output, "secret output"))
                self.assertEqual(self.github_path.read_text(), "/retained/path\n")
                self.assertEqual(list(self.parent.iterdir()), [])

    def test_github_path_symlink_is_rejected_before_any_network_or_binary(self):
        self.github_path.unlink()
        outside = self.root / "untouched"
        outside.write_text("preserve")
        self.github_path.symlink_to(outside)
        network = Network([])
        with self.assertRaises(m.Refused):
            self.install(network)
        self.assertEqual(network.requests, [])
        self.assertEqual(outside.read_text(), "preserve")

    def test_failed_download_diagnostics_never_include_urls_token_or_signed_query(self):
        network = Network([urllib.error.URLError("https://release-assets.githubusercontent.com/file?signature=secret test-token")] * 3)
        with self.assertRaises(m.Refused) as result:
            self.install(network)
        self.assertNotIn("secret", str(result.exception))
        self.assertNotIn("test-token", str(result.exception))
        self.assertNotIn("https://", str(result.exception))

    def test_cli_requires_explicit_ci_paths_before_network(self):
        with patch.dict(os.environ, {}, clear=True), contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(m.main([]), 1)
        self.assertEqual(list(self.parent.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
