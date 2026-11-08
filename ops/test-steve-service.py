#!/usr/bin/env python3
"""Temporary homes and fake managers only: never starts launchd/systemd or reads production state."""
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import shlex
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
        self.fail_action = None
        self.fail_after_removal = False
        self.show_exit_code = 0
        self.absent = False
        self.absent_after_reload = False
        self.needs_reload = "no"
        self.dropin = ""
        self.pending_dropin = ""

    def __call__(self, args, **kw):
        self.calls.append(args)
        out, error, code = "", "", 0
        if self.fail_action in args and (not self.fail_after_removal or not self.config.exists()):
            return SimpleNamespace(returncode=1, stdout="", stderr="test manager failure")
        if "print" in args:
            if self.unknown:
                error, code = "access denied", 1
            elif not self.loaded:
                error, code = "Could not find service in domain", 113
            else:
                out = "state = running\n" + ("pid = 123\n" if self.running else "")
        elif "--property=NeedDaemonReload" in args:
            out = self.needs_reload + "\n"
        elif "cat" in args:
            out = "# " + str(self.config) + "\n" + self.config.read_text() + self.dropin
        elif "show" in args:
            out = ("" if self.unknown else "LoadState=" + ("not-found" if self.absent else "loaded") + "\nActiveState=" + ("active" if self.running else "inactive")
                   + "\nMainPID=" + ("123" if self.running else "0") + "\nExecMainStatus=0\n")
            code = self.show_exit_code
        elif "daemon-reload" in args:
            if self.absent_after_reload:
                self.absent = True
            if self.pending_dropin:
                self.dropin = self.pending_dropin
                self.pending_dropin = ""
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
        manager.config = service.config
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

    def test_doctor_update_command_preserves_legacy_custom_label_before_adoption(self):
        label = "test.legacy.node"
        manager = Manager("Darwin")
        service = m.Service("node", self.home, label=label, system="Darwin", user_home=self.root, runner=manager)
        # Legacy services have a plist but no services/node.json record. The suggested command must
        # still address that job, so its running-state guard cannot be bypassed by a default label.
        service.config.parent.mkdir(parents=True)
        old = plistlib.dumps(dict(Label=label, ProgramArguments=[str(self.home / "bin/steve-node"), "run"],
                                  EnvironmentVariables=dict(STEVE_HOME=str(self.home))))
        service.config.write_bytes(old)
        manager.loaded = manager.running = True
        args = shlex.split(service.status(self.next, doctor=True)["update_command"])
        emitted_label = args[args.index("--label") + 1]
        emitted_home = args[args.index("--home") + 1]
        self.assertEqual((emitted_label, emitted_home), (label, str(self.home)))
        resumed = m.Service("node", emitted_home, label=emitted_label, system="Darwin", user_home=self.root, runner=manager)
        with self.assertRaisesRegex(m.Refused, "no files changed"):
            resumed.install(self.next, update=True)
        self.assertEqual(service.config.read_bytes(), old)
        self.assertFalse((self.root / "Library/LaunchAgents/io.github.infranite.steve.node.plist").exists())

    def test_unknown_manager_state_never_allows_replacement(self):
        for system in ("Darwin", "Linux"):
            with self.subTest(system=system):
                s, manager = self.service(system)
                manager.unknown = True
                with self.assertRaisesRegex(m.Refused, "state is unknown"):
                    s.install(self.release)
                self.assertFalse(s.config.exists())

    def legacy_signed_hub(self):
        home = self.root / ".steve"
        (home / "signing").mkdir(parents=True)
        (home / "signing/identity.json").write_text('{"label":"test.original.hub"}')
        (home / "current").symlink_to(self.release)
        manager = Manager("Darwin")
        manager.loaded = manager.running = True
        service = m.Service("hub", home, system="Darwin", user_home=self.root, runner=manager)
        service.config.parent.mkdir(parents=True)
        definition = dict(Label=service.label, ProgramArguments=["/bin/zsh", str(home / "current/ops/run.sh")])
        service.config.write_bytes(plistlib.dumps(definition))
        return service, manager, definition

    def test_legacy_signed_default_hub_can_be_inspected_without_rewriting_its_plist(self):
        service, manager, definition = self.legacy_signed_hub()
        for environment in (None, {}):
            with self.subTest(environment=environment):
                if environment is not None:
                    definition["EnvironmentVariables"] = environment
                old = plistlib.dumps(definition)
                service.config.write_bytes(old)
                report = service.status(doctor=True)
                self.assertTrue(report["service"]["running"])
                self.assertEqual(report["configured_build"]["version"], "0.2.0")
                self.assertEqual(service.config.read_bytes(), old)
                self.assertFalse(service.record_path.exists())
        self.assertTrue(all(call[1] == "print" for call in manager.calls))

    def test_legacy_hub_ownership_does_not_follow_a_label_or_an_ambiguous_command(self):
        service, manager, original = self.legacy_signed_hub()
        cases = [
            {"Label": "another.hub"},
            {"ProgramArguments": ["/bin/zsh", str(self.root / ".steve-other/current/ops/run.sh")]},
            {"ProgramArguments": original["ProgramArguments"] + ["extra"]},
            {"ProgramArguments": ["/bin/zsh", "-c", "true"]},
            {"Program": "/usr/bin/false"},
            {"RootDirectory": str(self.root / "chroot")},
            {"UserName": "another-user"},
            {"EnvironmentVariables": {"HOME": str(self.root / "another-user")}},
            {"EnvironmentVariables": {"STEVE_HOME": str(self.root / ".steve-other")}},
            {"EnvironmentVariables": {"STEVE_HOME": ""}},
            {"EnvironmentVariables": {"PATH": "/usr/bin"}},
        ]
        for changes in cases:
            with self.subTest(changes=changes):
                old = plistlib.dumps({**original, **changes})
                service.config.write_bytes(old)
                with self.assertRaisesRegex(m.Refused, "another or unknown home"):
                    service.state()
                self.assertEqual(service.config.read_bytes(), old)
                self.assertEqual(manager.calls, [])

        # Even that exact entrypoint is ambiguous outside the account's default home, or when
        # no signing identity binds the Hub to its existing label.
        adjacent = self.root / ".steve-other"
        (adjacent / "signing").mkdir(parents=True)
        (adjacent / "signing/identity.json").write_text('{"label":"test.original.hub"}')
        (adjacent / "current").symlink_to(self.release)
        service.config.write_bytes(plistlib.dumps({**original, "ProgramArguments": [
            "/bin/zsh", str(adjacent / "current/ops/run.sh")]}))
        other = m.Service("hub", adjacent, system="Darwin", user_home=self.root, runner=manager)
        with self.assertRaisesRegex(m.Refused, "another or unknown home"):
            other.state()
        service.config.write_bytes(plistlib.dumps(original))
        (service.home / "signing/identity.json").unlink()
        unsigned = m.Service("hub", service.home, label=service.label, system="Darwin", user_home=self.root, runner=manager)
        with self.assertRaisesRegex(m.Refused, "another or unknown home"):
            unsigned.state()
        self.assertEqual(manager.calls, [])

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

    def test_uninstall_requires_stopped_known_state_and_unloaded_launchd(self):
        for system in ("Darwin", "Linux"):
            s, manager = self.service(system)
            s.install(self.release)
            before = s.config.read_bytes(), s.record_path.read_bytes()
            for mode in ("running", "unknown", "loaded"):
                if system == "Linux" and mode == "loaded":
                    continue
                with self.subTest(system=system, mode=mode):
                    manager.running = mode == "running"
                    manager.loaded = mode in ("running", "loaded")
                    manager.unknown = mode == "unknown"
                    manager.calls.clear()
                    with self.assertRaisesRegex(m.Refused, "explicitly stop"):
                        s.uninstall()
                    self.assertEqual((s.config.read_bytes(), s.record_path.read_bytes()), before)
                    self.assertTrue(all("print" in c or "show" in c for c in manager.calls))
                    self.assertFalse((self.home / "services/retired").exists())
                    self.assert_preserved()

    def test_uninstall_backs_up_registration_preserves_home_and_is_repeatable(self):
        for name, value in {"data/test.db":"database", "signing/test.key":"key", "releases/retained":"release"}.items():
            path = self.home / name
            path.parent.mkdir(exist_ok=True)
            path.write_text(value)
            self.protected[name] = value
        for system in ("Darwin", "Linux"):
            with self.subTest(system=system):
                s, manager = self.service(system)
                s.install(self.release)
                before = s.config.read_bytes(), s.record_path.read_bytes()
                manager.calls.clear()
                result = s.uninstall()
                archive = Path(result["archive"])
                self.assertEqual(archive.parent, self.home / "services/retired")
                self.assertEqual((archive / "definition").read_bytes(), before[0])
                self.assertEqual((archive / "record.json").read_bytes(), before[1])
                self.assertFalse(s.config.exists())
                self.assertTrue(json.loads(s.record_path.read_text())["uninstalled"])
                self.assertTrue(result["home_preserved"])
                self.assertTrue(result["changed"])
                self.assertFalse(result["started"])
                mutation = [c for c in manager.calls if "print" not in c and "show" not in c and "cat" not in c]
                expected = [] if system == "Darwin" else [
                    ["systemctl", "--user", "daemon-reload"],
                    ["systemctl", "--user", "disable", s.label + ".service"],
                    ["systemctl", "--user", "daemon-reload"]]
                self.assertEqual(mutation, expected)
                retained = {p:p.read_bytes() for p in (self.home / "services").rglob("*") if p.is_file()}
                manager.calls.clear()
                resumed = m.Service("node", self.home, system=system, user_home=self.root, runner=manager)
                self.assertFalse(resumed.uninstall()["changed"])
                self.assertEqual(retained, {p:p.read_bytes() for p in (self.home / "services").rglob("*") if p.is_file()})
                self.assertTrue(all("print" in c or "show" in c for c in manager.calls))
                self.assert_preserved()
                self.assertTrue(resumed.install(self.release)["installed"])
                self.assertNotIn("uninstall", resumed.record)
                self.assertFalse(manager.running)
                self.assert_preserved()

    def test_uninstall_refuses_foreign_home_label_and_symlink_without_changes(self):
        for system in ("Darwin", "Linux"):
            with self.subTest(system=system):
                s, manager = self.service(system)
                s.install(self.release)
                original = s.config.read_bytes()
                manager.calls.clear()
                other = m.Service("node", self.root / "foreign", system=system, user_home=self.root, runner=manager)
                with self.assertRaisesRegex(m.Refused, "another or unknown home"):
                    other.uninstall()
                self.assertEqual(manager.calls, [])
                if system == "Darwin":
                    changed = plistlib.loads(original)
                    changed["Label"] = "foreign.label"
                    s.config.write_bytes(plistlib.dumps(changed))
                    with self.assertRaisesRegex(m.Refused, "exact owned label"):
                        s.uninstall()
                    self.assertEqual(manager.calls, [])
                    s.config.write_bytes(original)
                target = self.root / (system + "-foreign-definition")
                s.config.rename(target)
                s.config.symlink_to(target)
                with self.assertRaisesRegex(m.Refused, "owned regular file"):
                    s.uninstall()
                self.assertEqual(target.read_bytes(), original)
                self.assertTrue(s.config.is_symlink())
                self.assertEqual(manager.calls, [])
                self.assert_preserved()

    def test_uninstall_resumes_failed_reload_without_losing_backup_or_data(self):
        s, manager = self.service("Linux")
        s.install(self.release)
        original = s.config.read_bytes()
        manager.fail_action = "daemon-reload"
        manager.fail_after_removal = True
        with self.assertRaisesRegex(m.Refused, "manager command failed"):
            s.uninstall()
        self.assertFalse(s.config.exists())
        record = json.loads(s.record_path.read_text())
        self.assertEqual(record["uninstall"]["phase"], "pending")
        archive = self.home / "services/retired" / record["uninstall"]["archive"]
        self.assertEqual((archive / "definition").read_bytes(), original)
        manager.fail_action = None
        manager.calls.clear()
        resumed = m.Service("node", self.home, system="Linux", user_home=self.root, runner=manager)
        self.assertTrue(resumed.uninstall()["changed"])
        self.assertFalse(any("disable" in c for c in manager.calls))
        self.assertTrue(any("daemon-reload" in c for c in manager.calls))
        self.assertEqual((archive / "definition").read_bytes(), original)
        self.assert_preserved()

    def test_systemd_missing_unit_is_known_stopped_but_failed_query_is_unknown(self):
        s, manager = self.service("Linux")
        manager.absent = True
        self.assertFalse(s.uninstall()["changed"])
        self.assertFalse(s.record_path.exists())
        s.install(self.release)
        manager.absent = False
        manager.show_exit_code = 1
        with self.assertRaisesRegex(m.Refused, "state is unknown"):
            s.uninstall()
        self.assertTrue(s.config.is_file())
        self.assertFalse((self.home / "services/retired").exists())
        manager.show_exit_code = 0
        manager.absent_after_reload = True
        self.assertTrue(s.uninstall()["changed"])
        self.assertTrue(manager.absent)
        self.assertFalse(s.uninstall()["changed"])

    def test_uninstall_preserves_definition_on_disable_failure_and_detects_changed_backup(self):
        s, manager = self.service("Linux")
        s.install(self.release)
        original = s.config.read_bytes()
        manager.fail_action = "disable"
        with self.assertRaises(m.Refused):
            s.uninstall()
        self.assertEqual(s.config.read_bytes(), original)
        archive = self.home / "services/retired" / s.record["uninstall"]["archive"]
        (archive / "definition").write_text("changed archive")
        manager.fail_action = None
        manager.calls.clear()
        resumed = m.Service("node", self.home, system="Linux", user_home=self.root, runner=manager)
        with self.assertRaisesRegex(m.Refused, "backup changed"):
            resumed.uninstall()
        self.assertTrue(all("show" in c or "cat" in c or "daemon-reload" in c for c in manager.calls))
        self.assertEqual(s.config.read_bytes(), original)
        self.assert_preserved()

    def test_uninstall_rejects_also_in_main_or_dropin_and_stale_dropin_cache(self):
        s, manager = self.service("Linux")
        s.install(self.release)
        original = s.config.read_text()
        for source in ("main", "dropin", "continued_dropin", "new_dropin_directory", "stale", "unknown_freshness"):
            with self.subTest(source=source):
                manager.calls.clear()
                manager.dropin = ""
                manager.needs_reload = "no"
                s.config.write_text(original)
                if source == "main":
                    s.config.write_text(original + "Also=another.service\n")
                elif source == "dropin":
                    manager.dropin = "\n# override.conf\n[Install]\nAlso=another.service\n"
                elif source == "continued_dropin":
                    manager.dropin = "\n# override.conf\n[Install]\nAl\\\nso=another.service\n"
                elif source == "new_dropin_directory":
                    manager.pending_dropin = "\n# new-dir/override.conf\n[Install]\nAlso=another.service\n"
                else:
                    manager.needs_reload = "yes" if source == "stale" else ""
                before = s.config.read_bytes(), s.record_path.read_bytes()
                with self.assertRaisesRegex(m.Refused, "Also=|freshness is unknown"):
                    s.uninstall()
                self.assertEqual((s.config.read_bytes(), s.record_path.read_bytes()), before)
                self.assertTrue(all("show" in c or "cat" in c or "daemon-reload" in c for c in manager.calls))
                self.assertFalse((self.home / "services/retired").exists())
                self.assert_preserved()

    def test_legacy_signed_hub_uninstall_preserves_identity_current_and_release(self):
        s, manager, _ = self.legacy_signed_hub()
        manager.loaded = manager.running = False
        identity = (s.home / "signing/identity.json").read_bytes()
        before = s.config.read_bytes()
        result = s.uninstall()
        self.assertEqual((Path(result["archive"]) / "definition").read_bytes(), before)
        self.assertFalse((Path(result["archive"]) / "record.json").exists())
        self.assertEqual((s.home / "signing/identity.json").read_bytes(), identity)
        self.assertEqual((s.home / "current").resolve(), self.release)
        self.assertTrue(self.release.is_dir())
        self.assertTrue(all("print" in c for c in manager.calls))


if __name__ == "__main__":
    unittest.main()
