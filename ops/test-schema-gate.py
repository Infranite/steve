#!/usr/bin/env python3
"""ops/schema_gate.py: the rule every switch of `current` goes through, on throwaway databases and release dirs."""
import json
import os
import pathlib
import sqlite3
import stat
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import schema_gate  # noqa: E402


class Gate(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = pathlib.Path(self.tmp.name)
        self.db = self.dir / "steve.sqlite3"

    def tearDown(self):
        self.tmp.cleanup()

    def database(self, schema, min_reader=None):
        with sqlite3.connect(self.db) as c:
            c.execute("CREATE TABLE schema_version (v INTEGER NOT NULL)")
            c.execute("INSERT INTO schema_version VALUES (?)", (schema,))
            if min_reader is not None:
                c.execute("CREATE TABLE schema_compat (min_reader INTEGER NOT NULL)")
                c.execute("INSERT INTO schema_compat VALUES (?)", (min_reader,))

    def release(self, name, manifest_schema=None, binary_schema=None):
        d = self.dir / name
        d.mkdir()
        m = {"version": name}
        if manifest_schema is not None:
            m["schema"] = {"min_read": manifest_schema, "max_read": manifest_schema, "target": manifest_schema}
        (d / "release.json").write_text(json.dumps(m))
        if binary_schema is not None:
            (d / "bin").mkdir()
            b = d / "bin/steve-hub"
            b.write_text(f"#!/bin/sh\necho '{{\"schema\":{binary_schema}}}'\n")
            b.chmod(b.stat().st_mode | stat.S_IEXEC)
        return d

    def test_a_release_that_knows_the_stamped_schema_opens_it(self):
        self.database(27, min_reader=26)
        self.assertIsNone(schema_gate.check(self.db, self.release("old", manifest_schema=26)))
        self.assertIn("只认识到 schema v25", schema_gate.check(self.db, self.release("older", manifest_schema=25)))

    def test_an_unstamped_database_needs_its_own_schema(self):
        self.database(26)
        self.assertEqual(schema_gate.state(self.db), {"schema": 26, "min_reader": 26})
        self.assertIsNone(schema_gate.check(self.db, self.release("same", manifest_schema=26)))
        self.assertIsNotNone(schema_gate.check(self.db, self.release("older", manifest_schema=25)))

    def test_only_release_json_counts(self):
        # like the Hub's Core::rollback_refusal: a binary is not asked
        self.database(26, min_reader=26)
        self.assertIn("说不清", schema_gate.check(self.db, self.release("rs", binary_schema=26)))

    def test_a_release_that_cannot_say_is_refused(self):
        self.database(3)
        self.assertIn("说不清", schema_gate.check(self.db, self.release("ts")))

    def test_no_database_needs_nothing(self):
        self.assertEqual(schema_gate.state(self.db), {"schema": 0, "min_reader": 0})
        self.assertIsNone(schema_gate.check(self.db, self.release("ts")))

    def test_the_command_line(self):
        self.database(26, min_reader=26)
        gate = [sys.executable, str(pathlib.Path(schema_gate.__file__))]
        out = subprocess.run([*gate, "state", str(self.db)], capture_output=True, text=True)
        self.assertEqual(json.loads(out.stdout), {"schema": 26, "min_reader": 26})
        ok = subprocess.run([*gate, "check", str(self.db), str(self.release("a", manifest_schema=26))])
        self.assertEqual(ok.returncode, 0)
        no = subprocess.run([*gate, "check", str(self.db), str(self.release("b", manifest_schema=25))],
                            capture_output=True, text=True)
        self.assertEqual(no.returncode, 1)
        self.assertIn("v26", no.stdout)


if __name__ == "__main__":
    unittest.main()
