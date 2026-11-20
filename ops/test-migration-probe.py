#!/usr/bin/env python3
"""Storage phase progress uses a controlled clock; no wall-clock sleeps."""
import importlib.util
import json
import pathlib
import tempfile
import unittest
import uuid

spec=importlib.util.spec_from_file_location('local_release',pathlib.Path(__file__).with_name('local-release.py'))
release=importlib.util.module_from_spec(spec)
spec.loader.exec_module(release)

class MigrationProbeTests(unittest.TestCase):
    def setUp(self):
        self.temp=tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.home=pathlib.Path(self.temp.name);(self.home/'updates').mkdir()
        self.ident=str(uuid.uuid4());self.now=0
        self.budget=release.MigrationBudget(self.home,self.ident,123,clock=lambda:self.now)
    def write(self,steps=10,**fields):
        value=dict(format=2,upgrade_id=self.ident,pid=123,phase='validation',domain=None,vm_steps=steps,complete=False)
        value.update(fields);self.budget.path.write_text(json.dumps(value));self.budget.path.chmod(0o600)
    def test_long_sql_progress_extends_idle_but_never_total_timeout(self):
        for second in range(0,900,10):
            self.now=second;self.write(second);self.budget.check()
        self.now=900;self.write(900)
        with self.assertRaisesRegex(RuntimeError,'total timeout'):self.budget.check()
    def test_missing_corrupt_foreign_repeated_regressing_and_old_records_expire(self):
        cases=[dict(pid=124),dict(upgrade_id=str(uuid.uuid4())),dict(steps=9),dict(steps=10),
               dict(phase='legacy_replay'),dict(phase='unknown'),dict(domain='core'),
               dict(vm_steps='invalid'),dict(complete=1),dict(format=1),dict(migration=113),
               dict(complete=True),dict(vm_steps=2**64), 'corrupt','missing']
        for case in cases:
            with self.subTest(case=case):
                self.now=0;self.budget=release.MigrationBudget(self.home,self.ident,123,clock=lambda:self.now)
                self.write();self.budget.check();self.now=50
                if case=='corrupt':self.budget.path.write_text('{')
                elif case=='missing':self.budget.path.unlink()
                else:self.write(**case)
                self.budget.check();self.now=60
                with self.assertRaisesRegex(RuntimeError,'no migration progress'):self.budget.check()
    def test_stage_changes_are_progress_but_never_early_completion(self):
        for n,phase in enumerate(release.schema_gate.PHASES):
            self.now=n*50;self.write(phase=phase);self.budget.check()
        self.assertFalse(self.budget.seen['complete'])
        self.now=200;self.write(phase='install_contract',complete=True);self.budget.check()
        self.now=210
        with self.assertRaisesRegex(RuntimeError,'no migration progress'):self.budget.check()
    def test_domain_steps_and_one_final_health_window(self):
        self.write(phase='domain_migration',domain='core');self.budget.check()
        self.now=50;self.write(phase='domain_migration',domain='comms',complete=True);self.budget.check()
        self.now=100;self.write(20,phase='domain_migration',domain='comms');self.budget.check()
        self.now=110
        with self.assertRaisesRegex(RuntimeError,'migration completed; health verification timed out'):self.budget.check()
    def test_failed_final_transaction_without_complete_cannot_refresh_health(self):
        self.write(phase='domain_migration',domain='comms');self.budget.check()
        # Producer's COMMIT failed: its last record remains complete=False, never inferred
        # complete merely from reaching the last domain or ending a validation stage.
        self.now=50;self.budget.check();self.assertFalse(self.budget.seen['complete'])
        self.now=60
        with self.assertRaisesRegex(RuntimeError,'no migration progress'):self.budget.check()
    def test_record_cannot_revive_expired_probe(self):
        self.now=61;self.write(100)
        with self.assertRaisesRegex(RuntimeError,'no migration progress'):self.budget.check()

if __name__=='__main__':unittest.main()
