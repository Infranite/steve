#!/usr/bin/env python3
"""Temporary homes and fake managers only: never starts launchd/systemd or reads production state."""
import importlib.util
import json
import os
from pathlib import Path
import plistlib
from types import SimpleNamespace
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("steve_service", Path(__file__).with_name("steve-service.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class Manager:
    def __init__(self, system):
        self.system, self.running, self.loaded, self.calls = system, False, False, []
        self.unknown = False

    def __call__(self, args, **kw):
        self.calls.append(args)
        out, error, code = "", "", 0
        if "print" in args:
            if self.unknown:
                error, code = "access denied", 1
            elif not self.loaded:
                error, code = "Could not find service in domain", 113
            else:
                out = "state = running\n" + ("pid = 123\n" if self.running else "")
        elif "show" in args:
            out = ("" if self.unknown else "LoadState=loaded\nActiveState=" + ("active" if self.running else "inactive")
                   + "\nMainPID=" + ("123" if self.running else "0") + "\nExecMainStatus=0\n")
        elif any(a in args for a in ("bootstrap", "kickstart", "start", "restart")):
            self.loaded = self.running = True
        elif any(a in args for a in ("bootout", "stop")):
            self.loaded = self.running = False
        return SimpleNamespace(returncode=code, stdout=out, stderr=error)


class Services(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve() / 'space & quote " % $'
        self.root.mkdir()
        self.home = self.root / "node-home"
        self.home.mkdir()
        (self.home / "node").mkdir()
        self.protected = {
            "node/identity.key": "private identity", "node/node.json": json.dumps({"node_id": "test", "loopback": True}),
            "roots.json": "[]", "policy.json": '{"max_permissions":"auto"}', "harness.json": "{}"}
        for name, value in self.protected.items():
            (self.home / name).write_text(value)
        self.release = self.bundle("0.2.0", "a")
        self.next = self.bundle("0.2.1", "b")

    def bundle(self, version, commit):
        d = self.root / version
        (d / "bin").mkdir(parents=True)
        manifest = dict(format_version=1, version=version, commit=commit * 40)
        (d / "release.json").write_text(json.dumps(manifest))
        for name in ("steve-node", "steve-hub"):
            binary = d / "bin" / name
            binary.write_text("#!/bin/sh\nprintf '%s\\n' '" + json.dumps(manifest) + "'\n")
            binary.chmod(0o755)
        (d / "web/dist").mkdir(parents=True)
        (d / "web/dist/release.json").write_text(json.dumps(manifest))
        (d / "ops").mkdir()
        for name in ("run-managed.sh", "run.sh"):
            (d / "ops" / name).write_text("runtime")
        return d

    def service(self, system="Darwin", component="node"):
        manager = Manager(system)
        service = m.Service(component, self.home, system=system, user_home=self.root, runner=manager)
        return service, manager

    def assert_preserved(self):
        for name, value in self.protected.items():
            self.assertEqual((self.home / name).read_text(), value)

    def test_launchd_install_is_idempotent_preserves_pairing_and_keeps_paths_as_arguments(self):
        s, manager = self.service()
        result = s.install(self.release)
        self.assertTrue(result["changed"])
        self.assertFalse(manager.running)
        plist = plistlib.loads(s.config.read_bytes())
        self.assertEqual(plist["ProgramArguments"], [str(self.release / "bin/steve-node"), "run"])
        self.assertEqual(plist["EnvironmentVariables"]["STEVE_HOME"], str(self.home))
        self.assertEqual(plist["EnvironmentVariables"]["STEVE_NODE_LOOPBACK_PLAINTEXT"], "1")
        s.action("start")
        self.assertTrue(manager.running)
        before = len(manager.calls)
        self.assertFalse(s.install(self.release)["changed"])
        self.assertEqual([c for c in manager.calls[before:] if "bootout" in c], [])
        self.assert_preserved()

    def test_running_update_refuses_without_touching_service_or_identity(self):
        s, manager = self.service()
        s.install(self.release)
        s.action("start")
        before = s.config.read_bytes(), s.record_path.read_bytes()
        with self.assertRaisesRegex(m.Refused, "no files changed"):
            s.install(self.next, update=True)
        self.assertEqual((s.config.read_bytes(), s.record_path.read_bytes()), before)
        self.assertTrue(manager.running)
        self.assertFalse(any("bootout" in c for c in manager.calls))
        self.assert_preserved()

    def test_stopped_update_repoints_only_service_and_reports_candidate(self):
        s, manager = self.service()
        s.install(self.release)
        report = s.status(self.next, doctor=True)
        self.assertEqual(report["configured_build"]["version"], "0.2.0")
        self.assertEqual(report["candidate"]["build"]["version"], "0.2.1")
        self.assertEqual(report["connection"], "unknown")
        self.assertTrue(report["pairing"]["configured"])
        self.assertFalse(report["service"]["running"])
        s.install(self.next, update=True)
        self.assertEqual(s.record["previous"], str(self.release))
        s.install(self.next, update=True)
        self.assertEqual(s.record["previous"], str(self.release))
        self.assertFalse(manager.running)
        s.action("start")
        self.assertIsNone(s.status()["running_build"])
        self.assertEqual(s.status()["configured_build"]["version"], "0.2.1")
        self.assert_preserved()

    def test_unknown_manager_state_never_allows_replacement(self):
        for system in ("Darwin", "Linux"):
            with self.subTest(system=system):
                s, manager = self.service(system)
                manager.unknown = True
                with self.assertRaisesRegex(m.Refused, "state is unknown"):
                    s.install(self.release)
                self.assertFalse(s.config.exists())

    def test_systemd_user_service_and_offline_actions(self):
        s, manager = self.service("Linux")
        s.install(self.release)
        text = s.config.read_text()
        self.assertIn('%% $$', text)
        self.assertIn('\\"', text)
        self.assertIn("KillMode=control-group", text)
        self.assertIn(["systemctl", "--user", "enable", s.label + ".service"], manager.calls)
        for action, running in (("start", True), ("restart", True), ("stop", False)):
            s.action(action)
            self.assertEqual(manager.running, running)
        self.assert_preserved()

    def test_hub_retains_existing_signed_mode_and_refuses_unsafe_update(self):
        (self.home / "signing").mkdir()
        (self.home / "signing/identity.json").write_text('{"label":"test.original.hub"}')
        with self.assertRaisesRegex(m.Refused, "must match"):
            m.Service("hub", self.home, label="another.hub", system="Darwin", user_home=self.root, runner=Manager("Darwin"))
        s, _ = self.service(component="hub")
        s.install(self.release)
        self.assertEqual(s.label, "test.original.hub")
        self.assertFalse(s.status(doctor=True)["automatic_upgrade"]["supported"])
        self.assertEqual((self.home / "web/current").resolve(), self.release / "web/dist")
        self.assertEqual(plistlib.loads(s.config.read_bytes())["ProgramArguments"],
                         ["/bin/zsh", str(self.home / "current/ops/run.sh")])
        with self.assertRaisesRegex(m.Refused, "Settings"):
            s.install(self.next, update=True)
        self.assertEqual((self.home / "current").resolve(), self.release)

    def test_fresh_linux_hub_uses_supervisor_and_macos_requires_explicit_setup(self):
        s, _ = self.service("Linux", component="hub")
        s.install(self.release)
        self.assertIn("run-managed.sh", s.config.read_text())
        mac, _ = self.service(component="hub")
        with self.assertRaisesRegex(m.Refused, "setup-keychain"):
            mac.install(self.release)

    def test_another_home_cannot_replace_or_control_existing_label(self):
        for system in ("Darwin", "Linux"):
            with self.subTest(system=system):
                s, manager = self.service(system)
                s.install(self.release)
                before = s.config.read_bytes()
                other = m.Service("node", self.root / "another-home", system=system, user_home=self.root, runner=manager)
                for action in (lambda: other.install(self.next), lambda: other.action("stop"), lambda: other.status()):
                    with self.assertRaisesRegex(m.Refused, "another or unknown home"):
                        action()
                self.assertEqual(s.config.read_bytes(), before)

    def test_loaded_launchd_without_pid_must_be_explicitly_stopped(self):
        s, manager = self.service()
        s.install(self.release)
        manager.loaded = True
        manager.running = False
        with self.assertRaisesRegex(m.Refused, "no files changed"):
            s.install(self.next, update=True)
        self.assertFalse(any("bootout" in c for c in manager.calls))

    def test_bundle_identity_mismatch_is_rejected(self):
        p = self.next / "release.json"
        p.write_text(p.read_text().replace("0.2.1", "0.2.2"))
        s, _ = self.service()
        with self.assertRaisesRegex(m.Refused, "differs"):
            s.install(self.next)
        self.assertFalse(s.config.exists())

    def test_no_pairing_is_created_by_install(self):
        (self.home / "node/node.json").unlink()
        (self.home / "node/identity.key").unlink()
        s, _ = self.service()
        s.install(self.release)
        report = s.status(doctor=True)
        self.assertFalse(report["pairing"]["configured"])
        self.assertFalse(report["pairing"]["identity_present"])


if __name__ == "__main__":
    unittest.main()
