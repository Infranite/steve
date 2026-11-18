#!/usr/bin/env python3
"""Legacy deploy's optional progress contract; virtual time avoids a fifteen-minute test."""
import importlib.util
import json
import pathlib
import tempfile
import unittest
import uuid

spec = importlib.util.spec_from_file_location('local_release', pathlib.Path(__file__).with_name('local-release.py'))
release = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)


class MigrationProbeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.home = pathlib.Path(self.temp.name)
        (self.home / 'updates').mkdir()
        self.ident = str(uuid.uuid4())
        self.now = 0
        self.budget = release.MigrationBudget(self.home, self.ident, 123, clock=lambda: self.now)

    def write(self, steps=10, **fields):
        value = dict(format=1, upgrade_id=self.ident, pid=123, migration=76, target=76, vm_steps=steps, complete=False)
        value.update(fields)
        self.budget.path.write_text(json.dumps(value))
        self.budget.path.chmod(0o600)

    def test_long_statement_progress_extends_idle_but_not_total_timeout(self):
        for second in range(0, 900, 10):
            self.now = second
            self.write(second)
            self.budget.check()
        self.now = 900
        self.write(900)
        with self.assertRaisesRegex(RuntimeError, 'total timeout'):
            self.budget.check()

    def test_missing_corrupt_foreign_repeated_and_regressing_records_expire(self):
        cases = [dict(pid=124), dict(upgrade_id=str(uuid.uuid4())), dict(steps=9), dict(steps=10),
                 dict(migration=75), dict(target=77), dict(vm_steps='invalid'), dict(complete=1),
                 dict(migration=77), dict(format=2), 'corrupt', 'missing']
        for case in cases:
            with self.subTest(case=case):
                self.now = 0
                self.budget = release.MigrationBudget(self.home, self.ident, 123, clock=lambda: self.now)
                self.write()
                self.budget.check()
                self.now = 50
                if case == 'corrupt':
                    self.budget.path.write_text('{')
                elif case == 'missing':
                    self.budget.path.unlink()
                else:
                    self.write(**case)
                self.budget.check()
                self.now = 60
                with self.assertRaisesRegex(RuntimeError, 'no migration progress'):
                    self.budget.check()

    def test_completion_only_grants_one_health_window(self):
        self.write()
        self.budget.check()
        self.now = 50
        self.write(complete=True)
        self.budget.check()
        self.now = 100
        self.write(100, complete=False)
        self.budget.check()
        self.now = 110
        self.write(110, complete=True)
        with self.assertRaisesRegex(RuntimeError, 'migration completed; health verification timed out'):
            self.budget.check()

    def test_a_record_cannot_revive_an_expired_probe(self):
        self.now = 61
        self.write(100)
        with self.assertRaisesRegex(RuntimeError, 'no migration progress'):
            self.budget.check()


if __name__ == '__main__':
    unittest.main()
