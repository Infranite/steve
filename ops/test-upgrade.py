#!/usr/bin/env python3
"""Disposable installations, fake signatures/managers and published metadata. No production credentials."""
import ast
import concurrent.futures
import copy
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import urllib.error

spec = importlib.util.spec_from_file_location("upgrade", Path(__file__).with_name("upgrade.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Fixture(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.user = Path(self.temp.name).resolve()
        self.home = self.user / ".steve"
        for name in ("signing", "bin", "releases", "logs"):
            (self.home / name).mkdir(parents=True, exist_ok=True)
        self.identity = dict(label="test.steve", hub_identifier="test.hub", keyhelper_identifier="test.helper", vault_service="test.vault")
        (self.home / "signing/identity.json").write_text(json.dumps(self.identity))
        (self.home / "signing/steve-signing.keychain-db").write_text("not a real keychain")
        (self.home / "bin/steve-keyhelper").write_text("not executable")
        self.release = self.bundle("0.2.0", "a")
        (self.home / "current").symlink_to(self.release)
        self.plist = self.user / "Library/LaunchAgents/test.steve.plist"
        self.plist.parent.mkdir(parents=True)
        self.definition = dict(Label="test.steve", ProgramArguments=["/bin/zsh", str(self.home / "current/ops/run.sh")])
        self.save_service()
        self.account = SimpleNamespace(pw_uid=os.getuid(), pw_name="fixture", pw_dir=str(self.user))
        self.calls, self.job_state, self.job_path = [], "absent", None
        self.bootstrap_error, self.bootstrap_hook, self.fetch_hook = False, None, None
        self.repository = "Infranite/steve"
        self.manifest = dict(format_version=1, version="0.3.0", hub_version="0.3.0", web_version="0.3.0",
                             commit="b" * 40, api_protocol=1, schema=dict(min_read=24, max_read=24, target=24),
                             assets=[dict(target="aarch64-apple-darwin", name="steve-0.3.0-aarch64-apple-darwin.tar.gz", size=17, sha256="c" * 64)])
        self.remote = dict(tag_name="v0.3.0", draft=False, prerelease=False,
                           assets=[dict(name="steve-release.json"), dict(name=self.manifest["assets"][0]["name"])])

    def bundle(self, version, commit):
        path = self.home / "releases" / version
        (path / "bin").mkdir(parents=True)
        (path / "ops").mkdir()
        (path / "ops/run.sh").write_text("fixture")
        (path / "bin/steve-hub").write_text("not executable")
        info = dict(version=version, commit=commit * 40, target="aarch64-apple-darwin", schema=24,
                    upgrade_runner=dict(journal=1, kinds=["release_bundle", "local_release"]))
        (path / "release.json").write_text(json.dumps(info))
        return path

    def save_service(self):
        self.plist.write_bytes(plistlib.dumps(self.definition))

    def command(self, args, **kw):
        self.calls.append((args, kw.get("env", {})))
        out, error, code = "", "", 0
        if args[0] == "/usr/bin/codesign" and "-dv" in args:
            error = "Identifier=" + ("test.helper" if args[-1].endswith("steve-keyhelper") else "test.hub")
        elif "--build-info" in args:
            out = (Path(args[0]).parent.parent / "release.json").read_text()
        elif args[:2] == ["/bin/launchctl", "print"]:
            if self.job_state == "absent":
                code, error = 113, "Could not find service in domain"
            elif self.job_state == "live":
                out = "state = running\npid = 123\n"
            elif self.job_state == "unknown":
                code, error = 1, "not authorized"
            else:
                out = "state = exited\npath = " + str(self.job_path) + "\n"
        elif args[:2] == ["/bin/launchctl", "bootstrap"]:
            self.job_path = Path(args[-1])
            if self.bootstrap_hook:
                self.bootstrap_hook()
            if self.bootstrap_error:
                code, error = 5, "fixture bootstrap failed"
            else:
                self.job_state = "live"
        elif args[:2] == ["/bin/launchctl", "bootout"]:
            self.job_state = "absent"
        elif args[0] == "/usr/bin/git":
            self.assertIn("--bare", args)
            Path(args[-1]).mkdir()
            (Path(args[-1]) / "config").write_text("platform defaults")
        return SimpleNamespace(returncode=code, stdout=out, stderr=error)

    def fetch(self, url):
        self.assertIn("/" + self.repository + "/", url)
        if self.fetch_hook:
            self.fetch_hook()
        return copy.deepcopy(self.remote if "api.github.com" in url else self.manifest)

    def upgrade(self):
        return m.Upgrade(account=self.account, system="Darwin", run=self.command, fetch=self.fetch)

    def launches(self):
        return [c for c, _ in self.calls if c[:2] == ["/bin/launchctl", "bootstrap"]]

    def rewrite(self, operation, **fields):
        operation = dict(operation, **fields)
        m.atomic(self.home / "updates/operations" / (operation["id"] + ".json"), operation)
        return operation

    def lock_child(self, name, release=False):
        path = self.home / name
        path.parent.mkdir(exist_ok=True)
        program = "import fcntl,sys; f=open(sys.argv[1],'a'); " + ("fcntl.lockf" if release else "fcntl.flock") + "(f,fcntl.LOCK_EX); print('ready',flush=True); sys.stdin.read()"
        child = subprocess.Popen([sys.executable, "-I", "-c", program, str(path)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        def cleanup():
            if child.poll() is None:
                child.terminate()
                child.wait(timeout=5)
            child.stdin.close()
            child.stdout.close()
        self.addCleanup(cleanup)
        self.assertEqual(child.stdout.readline().strip(), "ready")
        return child

    def test_bundle_request_is_durable_actor_bound_and_inherits_no_caller_configuration(self):
        poison = dict(HOME="/wrong", USER="wrong", STEVE_HOME="/wrong", STEVE_UPDATE_REPOSITORY="attacker/repo",
                      PATH="/wrong", GIT_CONFIG_GLOBAL="/wrong", STEVE_SERVICE_CMD="/wrong", PYTHONPATH="/wrong", DYLD_INSERT_LIBRARIES="/wrong")
        with patch.dict(os.environ, poison):
            upgrade = self.upgrade()
            op = upgrade.request("0.3.0", "one")
        self.assertEqual(op["requested_by"], "local-os:" + str(os.getuid()))
        self.assertEqual(op["approved_by"], op["requested_by"])
        self.assertEqual(op["runner"]["kind"], "release_bundle")
        self.assertEqual(op["runner"]["repo"], "")
        self.assertFalse(any("git" in c[0] or "security" in c[0] for c, _ in self.calls))
        definition = plistlib.loads(self.job_path.read_bytes())
        self.assertEqual(definition["ProgramArguments"], [str(self.release / "bin/steve-hub"), "--upgrade-runner", str(self.home), op["id"]])
        environment = definition["EnvironmentVariables"]
        self.assertEqual(environment["HOME"], str(self.user))
        self.assertEqual(environment["STEVE_UPDATE_REPOSITORY"], self.repository)
        for key in ("PYTHONPATH", "DYLD_INSERT_LIBRARIES", "STEVE_SERVICE_CMD"):
            self.assertNotIn(key, environment)
        self.assertEqual(environment["GIT_CONFIG_GLOBAL"], "/dev/null")
        self.assertEqual((self.home / "updates/active.json").stat().st_mode & 0o777, 0o600)
        self.assertEqual(upgrade.request("0.3.0", "one")["id"], op["id"])
        self.assertEqual(len(self.launches()), 1)
        with self.assertRaisesRegex(m.Refused, "conflict"):
            upgrade.request("0.4.0", "one")

    def test_legacy_uses_only_owned_bare_cache_and_rejects_existing_candidate(self):
        info = json.loads((self.release / "release.json").read_text())
        del info["upgrade_runner"]
        (self.release / "release.json").write_text(json.dumps(info))
        upgrade = self.upgrade()
        candidate = self.home / "releases/0.3.0-local-bbbbbbb"
        candidate.mkdir()
        with self.assertRaisesRegex(m.Refused, "candidate already exists"):
            upgrade.request("0.3.0", "old")
        candidate.rmdir()
        op = upgrade.request("0.3.0", "old")
        self.assertEqual(op["runner"]["kind"], "local_release")
        self.assertEqual(op["runner"]["repo"], str(self.home / "updates/source-cache.git"))
        self.assertTrue(any("tarfile.data_filter" in " ".join(c) for c, _ in self.calls))
        self.assertEqual(upgrade.source_cache(), op["runner"]["repo"])
        (Path(op["runner"]["repo"]) / "config").write_text('[include]\npath=/other\n')
        with self.assertRaisesRegex(m.Refused, "configuration changed"):
            upgrade.source_cache()

    def test_unknown_capability_signature_and_install_identity_are_refused(self):
        path = self.release / "release.json"
        original = json.loads(path.read_text())
        for capability in ({"journal": 2, "kinds": ["release_bundle"]}, {}, {"journal": True, "kinds": ["release_bundle"]}):
            path.write_text(json.dumps(dict(original, upgrade_runner=capability)))
            with self.assertRaisesRegex(m.Refused, "capability"):
                self.upgrade()
        path.write_text(json.dumps(original))
        identity = self.home / "signing/identity.json"
        identity.write_text(json.dumps(dict(self.identity, hub_identifier="other.hub")))
        with self.assertRaisesRegex(m.Refused, "identifier"):
            self.upgrade()
        self.assertFalse((self.home / "updates").exists())

    def test_os_identity_external_install_and_writable_metadata_refuse(self):
        with patch.object(m.os, "geteuid", return_value=os.getuid() + 1):
            with self.assertRaisesRegex(m.Refused, "sudo"):
                self.upgrade()
        self.definition["EnvironmentVariables"] = dict(STEVE_HOME=str(self.home), STEVE_INSTALL_KIND="external")
        self.save_service()
        with self.assertRaisesRegex(m.Refused, "externally"):
            self.upgrade()
        self.definition.pop("EnvironmentVariables")
        self.save_service()
        self.plist.chmod(0o666)
        with self.assertRaisesRegex(m.Refused, "group/other"):
            self.upgrade()

    def test_install_configuration_precedence_and_unsupported_shell_are_explicit(self):
        self.definition["EnvironmentVariables"] = dict(STEVE_HOME=str(self.home), STEVE_UPDATE_REPOSITORY="owner/service", STEVE_PORT="9440")
        self.save_service()
        upgrade = self.upgrade()
        self.assertEqual((upgrade.repository, upgrade.env["STEVE_PORT"]), ("owner/service", "9440"))
        config = self.home / "steve.env"
        config.write_text("export STEVE_UPDATE_REPOSITORY='owner/config'\nSTEVE_PORT=9441\nIGNORED_SECRET=not_forwarded\n")
        upgrade = self.upgrade()
        self.assertEqual((upgrade.repository, upgrade.env["STEVE_PORT"]), ("owner/config", "9441"))
        self.assertNotIn("IGNORED_SECRET", upgrade.env)
        for text in ("STEVE_PORT=$OTHER", "STEVE_PORT=1\nSTEVE_PORT=2", "if true; then STEVE_PORT=5; fi", "STEVE_UPDATE_REPOSITORY=$(command)"):
            config.write_text(text)
            with self.assertRaises(m.Refused):
                self.upgrade()

    def test_stable_newer_target_and_published_integrity_are_required(self):
        upgrade = self.upgrade()
        for version in ("v0.3.0", "0.3.0-rc.1", "00.3.0", "0.3.0+other"):
            with self.assertRaisesRegex(m.Refused, "stable"):
                upgrade.request(version, "bad")
        self.remote["prerelease"] = True
        with self.assertRaisesRegex(m.Refused, "stable target"):
            upgrade.request("0.3.0", "bad")
        self.remote["prerelease"] = False
        self.manifest["assets"][0]["sha256"] = "bad"
        with self.assertRaisesRegex(m.Refused, "valid matching"):
            upgrade.request("0.3.0", "bad")
        self.assertEqual(upgrade.operations(), [])

    def test_rate_limit_explicit_version_reuses_published_manifest_fallback(self):
        upgrade = self.upgrade()
        def limited(url):
            if "api.github.com" in url:
                raise urllib.error.HTTPError(url, 429, "rate limited", {}, None)
            return self.fetch(url)
        upgrade.fetch = limited
        self.assertTrue(upgrade.check("0.3.0")["available"])
        with self.assertRaisesRegex(m.Refused, "explicit --version"):
            upgrade.check()

    def test_preflight_current_change_is_refused_before_operation_or_launch(self):
        upgrade = self.upgrade()
        newer = self.bundle("0.2.1", "d")
        def switched():
            current = self.home / "current"
            current.unlink()
            current.symlink_to(newer)
            self.fetch_hook = None
        self.fetch_hook = switched
        with self.assertRaisesRegex(m.Refused, "installation changed"):
            upgrade.request("0.3.0", "race")
        self.assertEqual(upgrade.operations(), [])
        self.assertEqual(self.launches(), [])

    def test_active_approval_and_other_operations_are_preserved(self):
        upgrade = self.upgrade()
        op = upgrade.request("0.3.0", "one")
        op = self.rewrite(op, phase="awaiting_approval", requested_by="web-user")
        before = (self.home / "updates/active.json").read_bytes()
        with self.assertRaisesRegex(m.Refused, "pending"):
            upgrade.request("0.3.0", "two")
        self.assertEqual(upgrade.operation(op["id"]), op)
        self.assertEqual((self.home / "updates/active.json").read_bytes(), before)

    def test_locks_and_live_or_unknown_jobs_refuse_without_bootout(self):
        upgrade = self.upgrade()
        for name, release in (("updates/runner.lock", False), ("releases/.release.lock", True), ("updates/journal.lock", False)):
            child = self.lock_child(name, release)
            with self.assertRaisesRegex(m.Refused, "another upgrade"):
                upgrade.request("0.3.0", "busy")
            child.stdin.close()
            child.wait(timeout=5)
        for state in ("live", "unknown", "exited"):
            self.job_state, self.job_path = state, Path("/unrelated/job.plist")
            with self.assertRaisesRegex(m.Refused, "live or unknown"):
                upgrade.request("0.3.0", "busy")
        self.assertFalse(any("bootout" in c for c, _ in self.calls))
        self.assertEqual(upgrade.operations(), [])

    def test_launch_failure_receipt_is_terminal_and_retry_never_relaunches(self):
        self.bootstrap_error = True
        upgrade = self.upgrade()
        op = upgrade.request("0.3.0", "failed")
        self.assertEqual(op["phase"], "failed")
        self.assertIn("bootstrap failed", op["error"])
        self.assertEqual(upgrade.request("0.3.0", "failed"), op)
        self.assertEqual(len(self.launches()), 1)

    def test_launch_error_does_not_overwrite_an_active_deploy_childs_receipt(self):
        upgrade = self.upgrade()
        advanced = []
        def deploying():
            op = upgrade.active()
            advanced.append(self.rewrite(op, phase="draining", backup="fixture-backup", target="fixture-candidate"))
            self.lock_child("releases/.release.lock", True)
        self.bootstrap_hook, self.bootstrap_error = deploying, True
        op = upgrade.request("0.3.0", "deploying")
        self.assertEqual(op, advanced[0])
        self.assertEqual(upgrade.request("0.3.0", "deploying"), advanced[0])
        self.assertEqual(len(self.launches()), 1)

    def test_uncertain_launch_with_live_job_stays_pending_and_is_not_repeated(self):
        def timeout_after_start():
            self.job_state = "live"
            raise subprocess.TimeoutExpired("fixture launchctl", 30)
        self.bootstrap_hook = timeout_after_start
        upgrade = self.upgrade()
        op = upgrade.request("0.3.0", "uncertain")
        self.assertEqual(op["phase"], "staging")
        self.assertIn("launch result", op["error"])
        self.assertEqual(upgrade.request("0.3.0", "uncertain"), op)
        self.assertEqual(len(self.launches()), 1)

    def test_only_owned_exited_job_can_be_replaced_after_its_terminal_receipt(self):
        upgrade = self.upgrade()
        op = upgrade.request("0.3.0", "old")
        self.rewrite(op, phase="failed")
        self.job_state = "exited"
        newer = upgrade.request("0.3.0", "new")
        self.assertNotEqual(newer["id"], op["id"])
        self.assertEqual(len([c for c, _ in self.calls if "bootout" in c]), 1)

    def test_dispatch_interruption_reconciles_without_repeating_possible_work(self):
        def interrupted():
            raise KeyboardInterrupt()
        self.bootstrap_hook = interrupted
        upgrade = self.upgrade()
        with self.assertRaises(KeyboardInterrupt):
            upgrade.request("0.3.0", "interrupted")
        op = upgrade.active()
        self.assertEqual(upgrade.request("0.3.0", "interrupted")["phase"], "staging")
        self.rewrite(op, updated_at=1, runner=dict(op["runner"], launched_at=1))
        op = upgrade.status()["operation"]
        self.assertEqual(op["phase"], "failed")
        self.assertIn("not restarted", op["error"])
        self.assertEqual(len(self.launches()), 1)

    def test_reap_preserves_deploy_child_holding_release_lock(self):
        upgrade = self.upgrade()
        op = upgrade.request("0.3.0", "child")
        op = self.rewrite(op, updated_at=1, runner=dict(op["runner"], launched_at=1))
        self.job_state = "absent"
        child = self.lock_child("releases/.release.lock", True)
        self.assertEqual(upgrade.status()["operation"], op)
        child.stdin.close()
        child.wait(timeout=5)
        self.assertEqual(upgrade.status()["operation"]["phase"], "failed")

    def test_status_reads_new_current_after_completed_upgrade(self):
        upgrade = self.upgrade()
        op = upgrade.request("0.3.0", "success")
        newer = self.bundle("0.3.0", "b")
        (self.home / "current").unlink()
        (self.home / "current").symlink_to(newer)
        self.rewrite(op, phase="succeeded", target=str(newer))
        result = upgrade.status(op["id"])
        self.assertEqual(result["current"]["version"], "0.3.0")
        self.assertEqual(result["operation"]["phase"], "succeeded")
        self.assertEqual(upgrade.request("0.3.0", "success")["id"], op["id"])

    def test_concurrent_same_key_launches_once_and_retry_recovers_receipt(self):
        upgrades = [self.upgrade(), self.upgrade()]
        barrier = threading.Barrier(2)
        def request(upgrade):
            barrier.wait(timeout=5)
            try:
                return upgrade.request("0.3.0", "concurrent")
            except m.Refused:
                return None  # A nonblocking journal lock reports busy; same-key retry is safe.
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            results = list(pool.map(request, upgrades))
        self.assertEqual(len(self.launches()), 1)
        op = next(r for r in results if r)
        self.assertEqual(upgrades[0].request("0.3.0", "concurrent")["id"], op["id"])

    def test_python39_syntax_and_help_need_no_installation(self):
        source = Path(m.__file__).read_text()
        ast.parse(source, feature_version=(3, 9))
        result = subprocess.run([sys.executable, "-I", m.__file__, "--help"], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("request", result.stdout)


if __name__ == "__main__":
    if sys.argv[1:] == ["--contract"]:
        fixture = Fixture()
        fixture.setUp()
        try:
            print(json.dumps(fixture.upgrade().request("0.3.0", "contract")))
        finally:
            fixture.doCleanups()
    else:
        unittest.main()
