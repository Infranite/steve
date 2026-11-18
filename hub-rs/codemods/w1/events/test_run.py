#!/usr/bin/env python3
"""Small isolated driver regressions; all repositories and backups live under scratch/."""
import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("event_codemod", HERE / "run.py")
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class DriverTests(unittest.TestCase):
    def test_dry_run_apply_repeat_and_rollback(self):
        scratch = driver.ROOT / "scratch"
        scratch.mkdir(exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="events-test-", dir=scratch) as tmp:
            root = Path(tmp)
            rel = Path("hub-rs/crates/steve-fixture/src/lib.rs")
            src = root / rel
            src.parent.mkdir(parents=True)
            original = '''fn production() {
    send(ChangeEvent::Item { id: format!("a,{}", "b").into() });
    send(s::ChangeEvent::Party { ws, id });
    send(HubEvent::SteveThinking { on: true, ws: None, phase: None });
    send(ChangeEvent::Future { id });
}
#[cfg(test)] mod tests {
 fn test() { send(ChangeEvent::Item { id }); }
}
fn consume(e: ChangeEvent) { match e { ChangeEvent::Item { id } => (), _ => () } }
'''
            src.write_text(original)
            subprocess.run(["git", "init", "-q", root], check=True)
            subprocess.run(["git", "-C", root, "add", "."], check=True)
            subprocess.run(["git", "-C", root, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.org", "commit", "-qm", "fixture"], check=True)

            def run(*args):
                stream = io.StringIO()
                with patch.object(driver, "ROOT", root), patch.object(sys, "argv", ["run.py", *args]), contextlib.redirect_stdout(stream):
                    driver.main()
                return stream.getvalue()

            first = run("--dry-run")
            self.assertEqual(first, run("--dry-run"))
            self.assertEqual(src.read_text(), original)
            self.assertIn("covered: 3; manual: 1", first)
            self.assertIn("Workspace placeholders to resolve: 2", first)
            self.assertIn("no rule for this legacy constructor", first)
            applied = run("--apply", "--crate", "steve-fixture")
            changed = src.read_text()
            self.assertIn('let value: String = format!("a,{}", "b").into()', changed)
            self.assertIn('ws: { let value: String = ws;', changed)
            self.assertIn('send(ChangeEvent::Future { id });', changed)
            self.assertIn('fn test() { send(ChangeEvent::Item { id }); }', changed)
            self.assertIn('match e { ChangeEvent::Item { id }', changed)
            self.assertIn("covered: 0; manual: 1", run("--apply"))
            self.assertEqual(src.read_text(), changed)
            backup = next(line.removeprefix("Backup: ") for line in applied.splitlines() if line.startswith("Backup: "))
            run("--rollback", str(root / backup))
            self.assertEqual(src.read_text(), original)

    def test_nested_literals_comments_and_raw_strings_keep_test_boundaries(self):
        text = 'fn p(){let x=r###"{ /* not comment */ }"###;}\n#[cfg(test)]mod t{ /* /* nested */ */ fn f(){} }'
        self.assertEqual(len(driver.mask(text)), len(text))
        start, end = driver.test_ranges(text)[0]
        self.assertTrue(text[start:end].startswith("#[cfg(test)]"))
        self.assertEqual(text[end - 1], "}")


if __name__ == "__main__":
    unittest.main()
