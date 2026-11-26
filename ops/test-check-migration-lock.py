#!/usr/bin/env python3
"""Tests for ops/check-migration-lock.sh (--offline mode, contiguous numbering, bad suffixes, immutability)."""
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parent.parent
SCRIPT = ROOT / "ops/check-migration-lock.sh"


def sha256(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()


def init_repo(tmp: Path, migrations: list[tuple[str, str]], tag: str | None = None) -> Path:
    repo = tmp / "repo"
    (repo / "ops").mkdir(parents=True)
    shutil.copyfile(SCRIPT, repo / "ops/check-migration-lock.sh")
    (repo / "ops/check-migration-lock.sh").chmod(0o755)
    mig_dir = repo / "hub-rs/crates/steve-store/migrations"
    mig_dir.mkdir(parents=True)
    lock_lines = ["# migration lock"]
    for name, body in migrations:
        (mig_dir / name).write_text(body)
        lock_lines.append(f"{name} {sha256(body)}")
    (mig_dir / "LOCK").write_text("\n".join(lock_lines) + "\n")
    env = {
        **os.environ,
        "GIT_AUTHOR_NAME": "Test",
        "GIT_AUTHOR_EMAIL": "test@example.com",
        "GIT_COMMITTER_NAME": "Test",
        "GIT_COMMITTER_EMAIL": "test@example.com",
    }
    subprocess.run(["git", "init", "-q"], cwd=repo, check=True, env=env)
    subprocess.run(["git", "add", "."], cwd=repo, check=True, env=env)
    subprocess.run(["git", "commit", "-q", "-m", "chore: seed"], cwd=repo, check=True, env=env)
    if tag:
        subprocess.run(["git", "tag", tag], cwd=repo, check=True, env=env)
    return repo


class CheckMigrationLockTests(unittest.TestCase):
    def test_repo_passes_offline_without_network(self):
        r = subprocess.run(
            ["bash", str(SCRIPT), "--offline"],
            cwd=ROOT,
            capture_output=True,
            text=True,
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("contiguous migrations and hashes verified", r.stdout)

    def test_accepts_contiguous_and_allowlisted_027_items(self):
        with tempfile.TemporaryDirectory(prefix="mig-ok-") as tmp:
            migs = [(f"{i:03d}.sql", f"-- m{i}\n") for i in range(1, 28)]
            migs.append(("027_items.sql", "-- helper\n"))
            migs.append(("028.sql", "-- m28\n"))
            repo = init_repo(Path(tmp), migs, tag="v0.1.0")
            r = subprocess.run(
                ["bash", str(repo / "ops/check-migration-lock.sh"), "--offline"],
                cwd=repo,
                capture_output=True,
                text=True,
            )
            self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_rejects_gap_and_disallowed_suffix(self):
        with tempfile.TemporaryDirectory(prefix="mig-gap-") as tmp:
            repo = init_repo(
                Path(tmp),
                [("001.sql", "-- m1\n"), ("003.sql", "-- m3\n")],
                tag="v0.1.0",
            )
            r = subprocess.run(
                ["bash", str(repo / "ops/check-migration-lock.sh"), "--offline"],
                cwd=repo,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("gap or order mismatch", r.stdout + r.stderr)

        with tempfile.TemporaryDirectory(prefix="mig-suffix-") as tmp:
            repo = init_repo(
                Path(tmp),
                [("001.sql", "-- m1\n"), ("999_bad.sql", "-- bad\n")],
                tag="v0.1.0",
            )
            r = subprocess.run(
                ["bash", str(repo / "ops/check-migration-lock.sh"), "--offline"],
                cwd=repo,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("disallowed migration filename", r.stdout + r.stderr)

    def test_rejects_editing_released_migration(self):
        with tempfile.TemporaryDirectory(prefix="mig-edit-") as tmp:
            repo = init_repo(Path(tmp), [("001.sql", "-- m1\n")], tag="v0.1.0")
            mig_dir = repo / "hub-rs/crates/steve-store/migrations"
            (mig_dir / "001.sql").write_text("-- edited\n")
            (mig_dir / "LOCK").write_text(f"# lock\n001.sql {sha256('-- edited\n')}\n")
            r = subprocess.run(
                ["bash", str(repo / "ops/check-migration-lock.sh"), "--offline"],
                cwd=repo,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(r.returncode, 0)
            self.assertIn("changed migrations", r.stdout + r.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
