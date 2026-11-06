#!/usr/bin/env python3
"""ops/local-release.py's part in a one-click upgrade, in a throwaway STEVE_HOME: the journal it writes for the Hub
(--operation) and the release lock it shares with ops/release.sh. Nothing is built, deployed or started."""
import json
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest
import uuid

OPS = pathlib.Path(__file__).resolve().parent
SCRIPT = OPS / "local-release.py"


class Journal(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = pathlib.Path(self.tmp.name)
        self.id = str(uuid.uuid4())
        ops = self.home / "updates/operations"
        ops.mkdir(parents=True)
        self.op = ops / f"{self.id}.json"
        self.op.write_text(json.dumps({"id": self.id, "idempotency_key": "k", "version": "0.3.0", "phase": "ready",
                                       "created_at": 1, "updated_at": 1, "requested_by": "owner", "error": None,
                                       "previous": None, "target": "/r", "runner": {"kind": "local_release"}}))

    def tearDown(self):
        self.tmp.cleanup()

    def run_script(self, *args):
        env = {k: os.environ[k] for k in ("PATH", "TMPDIR") if k in os.environ}
        env.update(HOME=str(self.home), STEVE_HOME=str(self.home))
        return subprocess.run([sys.executable, "-I", str(SCRIPT), *args], env=env, capture_output=True, text=True, timeout=60)

    def module(self):
        os.environ["STEVE_HOME"] = str(self.home)
        import importlib.util
        spec = importlib.util.spec_from_file_location("local_release", SCRIPT)
        m = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(m)
        return m

    def test_each_step_keeps_the_hubs_fields_and_stays_private(self):
        m = self.module()
        j = m.Journal(self.id)
        j("draining")
        j("backing_up", backup="/b/pre.sqlite3")
        j("restarting", previous="/old")
        op = json.loads(self.op.read_text())
        self.assertEqual((op["phase"], op["backup"], op["previous"], op["target"]), ("restarting", "/b/pre.sqlite3", "/old", "/r"))
        self.assertEqual(op["runner"], {"kind": "local_release"}, "the Hub's own fields are kept")
        self.assertGreater(op["updated_at"], 1)
        self.assertEqual(stat.S_IMODE(self.op.stat().st_mode), 0o600)
        self.assertFalse(j.terminal())
        j("succeeded")
        self.assertTrue(j.terminal())
        self.assertEqual(list(self.op.parent.glob(".*.tmp")), [])
        # without an id nothing is written; an id is only ever an operation id
        m.Journal(None)("failed")
        with self.assertRaises(ValueError):
            m.Journal("../../etc/passwd")
        with self.assertRaises(m.Refused):
            m.Journal(str(uuid.uuid4()))

    def test_a_deploy_refused_before_anything_changed_says_so_in_the_journal(self):
        # no signing identity in this STEVE_HOME: refused before the drain
        r = self.run_script("deploy", str(self.home / "releases/none"), "--operation", self.id)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        op = json.loads(self.op.read_text())
        self.assertEqual(op["phase"], "failed")
        self.assertTrue(op["error"].startswith("没有改动："), op["error"])

    def test_one_release_at_a_time_with_release_sh(self):
        lock = self.home / "releases/.release.lock"
        lock.parent.mkdir(parents=True)
        lock.touch()
        holder = subprocess.Popen([sys.executable, "-c", "import fcntl,sys,time; f=open(sys.argv[1],'a'); "
                                   "fcntl.lockf(f,fcntl.LOCK_EX); print('held',flush=True); time.sleep(30)", str(lock)],
                                  stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(holder.stdout.readline().strip(), "held")
            r = self.run_script("deploy", str(self.home / "releases/none"), "--operation", self.id)
            self.assertEqual(r.returncode, 1)
            self.assertIn("另一个发布正在进行", r.stdout)
            self.assertIn("另一个发布正在进行", json.loads(self.op.read_text())["error"])
        finally:
            holder.kill()
            holder.wait()

    def test_backups_become_the_owners_alone(self):
        b = self.home / "backups"
        (b / "failed-probe-x").mkdir(parents=True)
        os.chmod(b, 0o755)
        for f in (b / "pre-0.2.1.sqlite3", b / "failed-probe-x/steve.sqlite3", self.home / "steve.env.bak-1"):
            f.write_text("x")
            os.chmod(f, 0o644)
        self.module().private_backups()
        mode = lambda p: stat.S_IMODE(p.stat().st_mode)
        self.assertEqual((mode(b), mode(b / "failed-probe-x")), (0o700, 0o700))
        self.assertEqual(mode(b / "pre-0.2.1.sqlite3"), 0o600)
        self.assertEqual(mode(b / "failed-probe-x/steve.sqlite3"), 0o600)
        self.assertEqual(mode(self.home / "steve.env.bak-1"), 0o600)


class SigningPassword(unittest.TestCase):
    """Where build gets the signing keychain's password: the Hub's pipe, the old file, the keychain entry. The
    keychain is a fake `security` on PATH: the real one is never asked."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = pathlib.Path(self.tmp.name)
        (self.home / "signing").mkdir()
        self.bin = self.home / "bin"
        self.bin.mkdir()
        self.path = os.environ.get("PATH", "")
        os.environ["PATH"] = f"{self.bin}:{self.path}"
        self.m = Journal.module(self)
        self.ident = {"vault_service": "test.steve.vault"}

    def tearDown(self):
        os.environ["PATH"] = self.path
        self.tmp.cleanup()

    def security(self, script):
        f = self.bin / "security"
        f.write_text("#!/bin/sh\n" + script)
        f.chmod(0o755)

    def test_the_hubs_pipe_comes_first(self):
        import io
        (self.home / "signing/keychain-pass").write_text("from-file\n")
        old, sys.stdin = sys.stdin, io.StringIO("from-hub\n")
        try:
            self.assertEqual(self.m.signing_password(self.ident, from_stdin=True), "from-hub")
            sys.stdin = io.StringIO("")
            with self.assertRaises(self.m.Refused):
                self.m.signing_password(self.ident, from_stdin=True)
        finally:
            sys.stdin = old

    def test_an_install_not_moved_yet_uses_the_file(self):
        self.security("exit 44\n")
        (self.home / "signing/keychain-pass").write_text("from-file\n")
        self.assertEqual(self.m.signing_password(self.ident), "from-file")

    def test_a_moved_password_comes_from_the_keychain_entry(self):
        pw = "0123456789abcdef" * 4
        import base64
        b64 = base64.b64encode(bytes.fromhex(pw)).decode()
        args = self.home / "args"
        self.security(f'echo "$@" > {args}\necho {b64}\n')
        old, sys.stdin = sys.stdin, Tty()
        try:
            self.assertEqual(self.m.signing_password(self.ident), pw)
            self.assertEqual(args.read_text().split(),
                             ["find-generic-password", "-s", "test.steve.vault", "-a", "steve/signing/keychain-pass", "-w"])
            self.security("exit 44\n")
            with self.assertRaises(self.m.Refused):
                self.m.signing_password(self.ident)
        finally:
            sys.stdin = old

    def test_without_a_terminal_the_keychain_entry_is_not_asked_for(self):
        # the Hub's release job, an agent's shell: no macOS prompt to block on, or to click away with "Always Allow"
        import io
        args = self.home / "args"
        self.security(f'echo "$@" > {args}\necho AAAA\n')
        old, sys.stdin = sys.stdin, io.StringIO("")
        try:
            with self.assertRaisesRegex(self.m.Refused, "terminal"):
                self.m.signing_password(self.ident)
        finally:
            sys.stdin = old
        self.assertFalse(args.exists(), "security was not run")


class Tty:
    """A stdin that is a terminal."""

    def isatty(self):
        return True


class SigningKeychain(unittest.TestCase):
    """The signing keychain around codesign: unlocked with the password on stdin, never on a command line, and locked
    again after, also when codesign fails. `security` and `codesign` are fakes on PATH that log their arguments."""

    PW = "0123456789abcdef" * 4

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.home = pathlib.Path(self.tmp.name)
        self.bin = self.home / "bin"
        self.bin.mkdir()
        self.log = self.home / "calls"
        self.kc = self.home / "signing dir/steve-signing.keychain-db"
        self.path = os.environ.get("PATH", "")
        os.environ["PATH"] = f"{self.bin}:{self.path}"
        self.fake("security", f'echo "security $*" >> "{self.log}"\n'
                              f'[ "$1" = -i ] && sed "s/^/  stdin: /" >> "{self.log}"\nexit 0\n')
        self.codesign(0)

    def tearDown(self):
        os.environ["PATH"] = self.path
        self.tmp.cleanup()

    def fake(self, name, body):
        f = self.bin / name
        f.write_text("#!/bin/sh\n" + body)
        f.chmod(0o755)

    def codesign(self, rc):
        self.fake("codesign", f'echo "codesign $*" >> "{self.log}"\nexit {rc}\n')

    def calls(self):
        return self.log.read_text().splitlines()

    def assert_locked_after_signing(self, calls):
        unlock = calls.index("security -i")
        self.assertEqual(calls[unlock + 1], f'  stdin: unlock-keychain -p {self.PW} "{self.kc}"')
        sign = next(i for i, c in enumerate(calls) if c.startswith("codesign --force") or c.startswith("codesign -f"))
        lock = calls.index(f"security lock-keychain {self.kc}")
        self.assertLess(unlock, sign)
        self.assertLess(sign, lock)
        self.assertIn(f"security set-keychain-settings -l -u -t 300 {self.kc}", calls, "it also locks itself")
        for c in calls:
            if not c.startswith("  stdin:"):
                self.assertNotIn(self.PW, c, "the password is on no command line")

    def test_build_locks_the_keychain_after_codesign(self):
        m = Journal.module(self)
        m.sign(self.home / "steve-hub", self.kc, self.PW, "test.hub")
        self.assert_locked_after_signing(self.calls())

    def test_a_failed_codesign_still_locks_it(self):
        m = Journal.module(self)
        self.codesign(1)
        with self.assertRaises(subprocess.CalledProcessError):
            m.sign(self.home / "steve-hub", self.kc, self.PW, "test.hub")
        self.assert_locked_after_signing(self.calls())

    def test_a_password_security_can_not_take_is_refused(self):
        m = Journal.module(self)
        with self.assertRaises(m.Refused):
            m.sign(self.home / "steve-hub", self.kc, "two words", "test.hub")
        self.assertFalse(self.log.exists(), "nothing was run")

    def zsh(self, script, stdin=subprocess.DEVNULL):
        env = dict(os.environ, STEVE_KEYHELPER_SERVICE="test.steve.vault")
        return subprocess.run(["zsh", "-c", f'source "{OPS}/release-lib.sh"\n{script}'], env=env, stdin=stdin,
                              capture_output=True, text=True, timeout=30)

    def test_release_sh_locks_the_keychain_after_codesign(self):
        # ops/release.sh and ops/sign-setup.sh sign through release-lib.sh's kc_sign
        for rc in (0, 1):
            self.log.unlink(missing_ok=True)
            self.codesign(rc)
            r = self.zsh(f'kc_sign "{self.kc}" {self.PW} codesign -f -s "Steve Hub" x; print -r -- "rc=$?"')
            self.assertIn(f"rc={rc}", r.stdout, r.stderr)
            self.assert_locked_after_signing(self.calls())

    def test_release_sh_asks_for_the_keychain_entry_only_from_a_terminal(self):
        r = self.zsh(f'sign_pass "{self.kc}" || print -r -- refused')
        self.assertIn("refused", r.stdout)
        self.assertIn("terminal", r.stderr)
        self.assertFalse(self.log.exists(), "security was not run")
        # a password not moved yet still comes from the file
        self.kc.parent.mkdir()
        (self.kc.parent / "keychain-pass").write_text(self.PW + "\n")
        r = self.zsh(f'sign_pass "{self.kc}"')
        self.assertEqual(r.stdout.strip(), self.PW)


if __name__ == "__main__":
    unittest.main()
