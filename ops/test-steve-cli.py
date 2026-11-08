#!/usr/bin/env python3
"""Installed CLI acceptance with fake releases/processes; no real services or Keychain."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


spec = importlib.util.spec_from_file_location("steve_cli", Path(__file__).with_name("steve.py"))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)


class FakeRunner:
    def __init__(self):
        self.calls = []
        self.fail_install = False
        self.bad_node = False
        self.return_code = 0

    def __call__(self, args, **kwargs):
        self.calls.append((args, kwargs))
        home = Path(kwargs["env"].get("STEVE_HOME", "/not-used"))
        output = ""
        if "--build-info" in args:
            root = Path(args[0]).parent.parent
            info = json.loads((root / "release.json").read_text())
            output = json.dumps(dict(version="9.9.9" if self.bad_node and args[0].endswith("steve-node") else info["version"],
                                     commit=info["commit"], target=info["test_target"], schema=info["schema"]["target"]))
        elif "--check-config" in args:
            assert Path(kwargs["env"]["STEVE_OIDC_CLIENT_SECRET_FILE"]).read_text() == "secret-for-test"
            output = '{"ok":true,"errors":[],"warnings":[]}'
        elif "setup-keychain" in args:
            candidate = Path(args[-1])
            (candidate / "bin/steve-hub").write_text("signed test hub")
            (home / "signing").mkdir()
            (home / "signing/identity.json").write_text('{"test":"identity"}')
            (home / "bin").mkdir()
            (home / "bin/steve-keyhelper").write_text("fixed installed helper")
        elif len(args) > 1 and args[1] == "pair":
            (home / "node").mkdir(exist_ok=True)
            identity = home / "node/identity.key"
            if not identity.exists():
                identity.write_text("test identity")
                identity.chmod(0o600)
            (home / "node/node.json").write_text(json.dumps(dict(hub_url=args[2], node_id="n_test",
                pin=args[args.index("--pin")+1] if "--pin" in args else None)))
            output = "fingerprint test-fingerprint"
        elif "install" in args:
            if self.fail_install:
                return subprocess.CompletedProcess(args, 7, '{"error":"test service manager unavailable"}', "")
            (home / "services").mkdir(exist_ok=True)
            (home / "services/test.json").write_text("{}")
            output = '{"installed":true,"started":false}'
        return subprocess.CompletedProcess(args, self.return_code, output, "")


class CLITest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="steve-cli-test-")
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name).resolve()
        self.bundle = self.root / "package"
        self.bundle.mkdir()
        self.user = self.root / "user"
        self.user.mkdir()
        self.runner = FakeRunner()
        self.metadata = dict(format_version=1, version="1.2.3", commit="a" * 40, hub_version="1.2.3",
                             web_version="1.2.3", schema={"target":54}, test_target="x86_64-unknown-linux-gnu")
        self.write("release.json", json.dumps(self.metadata))
        self.write("web/dist/release.json", json.dumps({k:self.metadata[k] for k in ("version", "commit")}))
        self.write("web/dist/index.html", '<meta name="steve-web-version" content="1.2.3">')
        for name in ("steve-hub", "steve-node", "steve-keyhelper"):
            self.write("bin/" + name, "unsigned fake binary", 0o555)
        for name in ("steve-service.py", "local-release.py", "upgrade.py", "run.sh", "run-managed.sh", "sign-setup.sh", "release-lib.sh"):
            self.write("ops/" + name, "fake script", 0o444)

    def write(self, relative, content, mode=0o444):
        p = self.bundle / relative
        p.parent.mkdir(parents=True, exist_ok=True)
        if p.exists():
            p.chmod(0o600)
        p.write_text(content)
        p.chmod(mode)

    def cli(self, answers=(), system="Linux"):
        answers = iter(answers)
        return m.CLI(self.bundle, runner=self.runner, ask=lambda _:next(answers),
                     secret=lambda _:"secret-for-test", system=system,
                     machine="arm64" if system == "Darwin" else "x86_64", user_home=self.user)

    def hub_answers(self, keys=None):
        return ["http://127.0.0.1:8440", "https://auth.example.test", "steve-client", str(keys or self.root / "keys")]

    def setup_hub(self, cli=None):
        with contextlib.redirect_stdout(io.StringIO()):
            return (cli or self.cli(self.hub_answers())).setup("hub")

    def test_linux_hub_copies_readonly_bundle_and_never_starts_or_exports_secret(self):
        before = {str(p.relative_to(self.bundle)):(p.read_bytes(), stat.S_IMODE(p.stat().st_mode))
                  for p in self.bundle.rglob("*") if p.is_file()}
        self.setup_hub()
        home = self.user / ".steve"
        self.assertEqual((home / "secrets/oidc-client-secret").read_text(), "secret-for-test")
        for path in (home / "setup.json", home / "steve.env", home / "secrets/oidc-client-secret"):
            self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(home.stat().st_mode), 0o700)
        self.assertEqual(stat.S_IMODE((self.root / "keys").stat().st_mode), 0o700)
        self.assertFalse((home / "releases/1.2.3").is_symlink())
        self.assertEqual(before, {str(p.relative_to(self.bundle)):(p.read_bytes(), stat.S_IMODE(p.stat().st_mode))
                                  for p in self.bundle.rglob("*") if p.is_file()})
        for args, kw in self.runner.calls:
            self.assertNotIn("start", args)
            self.assertNotIn("secret-for-test", json.dumps([args, kw]))
            self.assertNotIn("STEVE_DEV_LOGIN", kw["env"])

    def test_mac_signs_managed_copy_and_keeps_source_helper_unchanged(self):
        self.metadata["test_target"] = "aarch64-apple-darwin"
        self.write("release.json", json.dumps(self.metadata))
        app = self.cli(self.hub_answers()[:3], system="Darwin")
        self.setup_hub(app)
        home = self.user / ".steve"
        self.assertEqual((self.bundle / "bin/steve-hub").read_text(), "unsigned fake binary")
        self.assertEqual((home / "releases/1.2.3/bin/steve-hub").read_text(), "signed test hub")
        self.assertEqual((home / "bin/steve-keyhelper").read_text(), "fixed installed helper")
        self.setup_hub(self.cli(system="Darwin"))
        self.assertEqual(sum("setup-keychain" in a for a, _ in self.runner.calls), 1)

    def test_pending_service_failure_resumes_without_new_config_or_secret(self):
        self.runner.fail_install = True
        with self.assertRaises(m.Refused):
            self.setup_hub()
        home = self.user / ".steve"
        files = {p:p.read_bytes() for p in (home / "steve.env", home / "secrets/oidc-client-secret")}
        self.runner.fail_install = False
        self.setup_hub(self.cli())
        self.assertEqual(files, {p:p.read_bytes() for p in files})

    def test_changed_config_is_preserved_and_not_adopted_on_resume(self):
        self.setup_hub()
        config = self.user / ".steve/steve.env"
        config.write_text("operator change")
        with self.assertRaisesRegex(m.Refused, "已被修改"):
            self.setup_hub(self.cli())
        self.assertEqual(config.read_text(), "operator change")

    def test_changed_runtime_script_blocks_resume(self):
        self.setup_hub()
        script = self.user / ".steve/releases/1.2.3/ops/run.sh"
        script.write_text("unexpected replacement")
        with self.assertRaisesRegex(m.Refused, "版本文件已改变"):
            self.setup_hub(self.cli())

    def test_existing_home_and_dangling_link_are_never_adopted(self):
        home = self.user / ".steve"
        home.mkdir(mode=0o700)
        marker = home / "old-data"
        marker.write_text("preserve")
        with self.assertRaisesRegex(m.Refused, "不会覆盖"):
            self.setup_hub()
        self.assertEqual(marker.read_text(), "preserve")
        marker.unlink()
        home.rmdir()
        home.symlink_to(self.root / "missing")
        with self.assertRaisesRegex(m.Refused, "符号链接"):
            self.setup_hub()
        self.assertTrue(home.is_symlink())

    def test_invalid_node_build_or_package_symlink_creates_no_home(self):
        self.runner.bad_node = True
        with self.assertRaises(m.Refused):
            self.setup_hub()
        self.assertFalse((self.user / ".steve").exists())
        self.runner.bad_node = False
        (self.bundle / "linked").symlink_to(self.bundle / "release.json")
        with self.assertRaises(m.Refused):
            self.setup_hub()
        self.assertFalse((self.user / ".steve").exists())

    def test_linux_key_directory_cannot_be_inside_home_or_adopt_old_keys(self):
        nested = self.user / ".steve/keys"
        with self.assertRaisesRegex(m.Refused, "不能互相包含"):
            self.setup_hub(self.cli(self.hub_answers(nested)))
        self.assertFalse(nested.exists())
        keys = self.root / "old-keys"
        keys.mkdir(mode=0o700)
        (keys / "old").write_text("key")
        with self.assertRaisesRegex(m.Refused, "不会接管旧密钥"):
            self.setup_hub(self.cli(self.hub_answers(keys)))
        self.assertEqual((keys / "old").read_text(), "key")

    def test_node_pair_uses_stdin_and_empty_roots_with_separate_home(self):
        with patch.dict(os.environ, {"STEVE_PAIR_CODE":"injected-code", "STEVE_HOME":"/not-the-node-home"}), contextlib.redirect_stdout(io.StringIO()):
            self.cli().setup("node", hub="https://hub.example.test")
        home = self.user / ".steve-node"
        self.assertEqual(json.loads((home / "roots.json").read_text()), [])
        self.assertFalse((self.user / ".steve").exists())
        calls = [(a,k) for a,k in self.runner.calls if len(a)>1 and a[1]=="pair"]
        self.assertEqual(len(calls), 1)
        args, kw = calls[0]
        self.assertEqual(args[1:], ["pair", "https://hub.example.test"])
        self.assertNotIn("STEVE_PAIR_CODE", kw["env"])
        self.assertNotIn("stdin", kw)
        self.assertNotIn("input", kw)
        self.assertEqual(kw["env"]["STEVE_HOME"], str(home))
        with contextlib.redirect_stdout(io.StringIO()):
            self.cli().setup("node", hub="https://hub.example.test")
        self.assertEqual(sum(len(a)>1 and a[1]=="pair" for a,_ in self.runner.calls), 1)
        with self.assertRaises(m.Refused), contextlib.redirect_stdout(io.StringIO()):
            self.cli().setup("node", hub="https://different.example.test")

    def test_interactive_role_can_prepare_unpaired_node_without_identity(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.cli(["node", ""]).setup(None)
        home = self.user / ".steve-node"
        self.assertFalse((home / "node/identity.key").exists())
        self.assertFalse(any(len(a)>1 and a[1]=="pair" for a,_ in self.runner.calls))
        self.assertTrue((home / "services/test.json").exists())

    def test_node_pin_is_explicit_and_cannot_replace_existing_trust(self):
        pin = "sha256:" + "a" * 64
        with contextlib.redirect_stdout(io.StringIO()):
            self.cli().setup("node", hub="https://hub.example.test", pin=pin)
        pairs = [a for a,_ in self.runner.calls if len(a)>1 and a[1]=="pair"]
        self.assertEqual(pairs[0][-2:], ["--pin", pin])
        with self.assertRaises(m.Refused), contextlib.redirect_stdout(io.StringIO()):
            self.cli().setup("node", hub="https://hub.example.test", pin="sha256:" + "b" * 64)

    def test_resume_refuses_changed_installed_keyhelper(self):
        self.metadata["test_target"] = "aarch64-apple-darwin"
        self.write("release.json", json.dumps(self.metadata))
        self.setup_hub(self.cli(self.hub_answers()[:3], system="Darwin"))
        helper = self.user / ".steve/bin/steve-keyhelper"
        helper.write_text("changed helper")
        with self.assertRaisesRegex(m.Refused, "keyhelper 已改变"):
            self.setup_hub(self.cli(system="Darwin"))
        self.assertEqual(helper.read_text(), "changed helper")

    def test_resume_refuses_changed_signed_hub_before_executing_it(self):
        self.metadata["test_target"] = "aarch64-apple-darwin"
        self.write("release.json", json.dumps(self.metadata))
        self.setup_hub(self.cli(self.hub_answers()[:3], system="Darwin"))
        binary = self.user / ".steve/releases/1.2.3/bin/steve-hub"
        binary.write_text("changed Hub still reporting the original build info")
        self.runner.calls.clear()
        with self.assertRaisesRegex(m.Refused, "Hub 文件发生变化"):
            self.setup_hub(self.cli(system="Darwin"))
        self.assertFalse(any(a[0] == str(binary) for a, _ in self.runner.calls))

    def test_interrupted_signing_without_digest_never_executes_changed_hub(self):
        self.metadata["test_target"] = "aarch64-apple-darwin"
        self.write("release.json", json.dumps(self.metadata))
        self.setup_hub(self.cli(self.hub_answers()[:3], system="Darwin"))
        receipt_path = self.user / ".steve/setup.json"
        receipt = json.loads(receipt_path.read_text())
        receipt.pop("signed_hub_sha256")
        receipt["steps"].remove("keychain")
        receipt_path.write_text(json.dumps(receipt))
        self.runner.calls.clear()
        with self.assertRaisesRegex(m.Refused, "本地签名中断"):
            self.setup_hub(self.cli(system="Darwin"))
        candidate = self.user / ".steve/releases/1.2.3"
        self.assertFalse(any(str(candidate) in a[0] or "setup-keychain" in a for a, _ in self.runner.calls))

    def test_outer_process_uses_only_fake_bundle_children(self):
        system, machine = m.platform.system(), m.platform.machine()
        if (system, machine) not in (("Darwin", "arm64"), ("Linux", "x86_64")):
            self.skipTest("no release target for this test host")
        self.metadata["test_target"] = "aarch64-apple-darwin" if system == "Darwin" else "x86_64-unknown-linux-gnu"
        self.write("release.json", json.dumps(self.metadata))
        info = {k:self.metadata[k] for k in ("version", "commit")}
        info.update(target=self.metadata["test_target"], schema=54)
        for name in ("steve-hub", "steve-node"):
            self.write("bin/" + name, "#!/usr/bin/env python3\nimport json\nprint(json.dumps(" + repr(info) + "))\n", 0o555)
        self.write("ops/steve.py", Path(m.__file__).read_text())
        self.write("ops/steve-service.py", "import json,sys\nfrom pathlib import Path\nh=Path(sys.argv[sys.argv.index('--home')+1])\n(h/'fake-service-call.json').write_text(json.dumps(sys.argv[1:]))\nprint(json.dumps({'installed':True,'started':False}))\n")
        home = self.root / "explicit home with spaces"
        result = subprocess.run([sys.executable, str(self.bundle / "ops/steve.py"), "setup", "node", "--home", str(home)],
                                input="\n", capture_output=True, text=True, timeout=20)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("本次 setup 未启动或重启服务", result.stdout)
        self.assertEqual(json.loads((home / "fake-service-call.json").read_text())[:2], ["node", "install"])
        self.assertFalse((home / "node/identity.key").exists())
        self.assertEqual(json.loads((home / "roots.json").read_text()), [])
        self.assertEqual(list(self.user.iterdir()), [])

    def test_version_and_help_do_not_initialize_or_invoke_a_process(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(m.main(["--version"], self.cli()), 0)
            with self.assertRaises(SystemExit) as stopped:
                m.main(["--help"], self.cli())
        self.assertEqual(stopped.exception.code, 0)
        self.assertIn("steve CLI/package 1.2.3", output.getvalue())
        self.assertEqual(self.runner.calls, [])
        self.assertEqual(list(self.user.iterdir()), [])

    def test_lifecycle_does_not_initialize_and_preserves_exit_status(self):
        self.runner.return_code = 9
        self.assertEqual(m.main(["node", "status"], self.cli()), 9)
        self.assertFalse((self.user / ".steve-node").exists())
        args, kw = self.runner.calls[-1]
        self.assertEqual(args[2:4], ["node", "status"])
        self.assertFalse(kw["capture_output"])

    def test_upgrade_and_hub_update_use_existing_guarded_entries(self):
        self.assertEqual(m.main(["upgrade", "check", "--version", "1.2.3"], self.cli()), 0)
        self.assertEqual(self.runner.calls[-1][0][1:], [str(self.bundle / "ops/upgrade.py"), "check", "--version", "1.2.3"])
        self.assertEqual(m.main(["hub", "update", "--release", str(self.bundle)], self.cli()), 0)
        self.assertEqual(self.runner.calls[-1][0][1:4], [str(self.bundle / "ops/steve-service.py"), "hub", "update"])
        self.assertFalse(any("deploy" in a or "--from-source" in a for a,_ in self.runner.calls))

    def test_explicit_uninstall_delegates_without_initializing_or_stopping(self):
        self.runner.return_code = 7
        for role in ("hub", "node"):
            self.assertEqual(m.main([role, "uninstall"], self.cli()), 7)
            args, kwargs = self.runner.calls[-1]
            self.assertEqual(args[1:4], [str(self.bundle / "ops/steve-service.py"), role, "uninstall"])
            self.assertNotIn("stop", args)
        self.assertEqual(list(self.user.iterdir()), [])

    def test_shell_metacharacters_are_quoted_in_environment_file(self):
        app = self.cli(["http://127.0.0.1:8440", "https://auth.example.test", "a'$(false)", str(self.root / "keys space")])
        self.setup_hub(app)
        config = (self.user / ".steve/steve.env").read_text()
        self.assertIn("STEVE_OIDC_CLIENT_ID='a'\"'\"'$(false)'", config)

    def test_url_rejects_plain_remote_userinfo_and_fragments(self):
        for value in ("http://host.example.test", "https://u:p@host.example.test", "https://host.example.test/#x"):
            with self.subTest(value=value), self.assertRaises(m.Refused):
                m.url(value, "Hub")


if __name__ == "__main__":
    unittest.main()
