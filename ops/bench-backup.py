#!/usr/bin/env python3
"""Measure real release backup paths in a disposable installation. Run on an idle machine."""

import argparse
import contextlib
import http.client
import json
import os
from pathlib import Path
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
RUST = ROOT / "hub-rs"
TARGET = Path(os.environ.get("CARGO_TARGET_DIR", RUST / "target")).resolve()
sys.path.insert(0, str(ROOT / "scripts/reform/bench"))
from isolation import DISABLED_ENV
from telemetry import parse


def stage_engine(bundle):
    rust = subprocess.check_output(["rustc", "-vV"], cwd=RUST, text=True)
    target = next(line.removeprefix("host: ") for line in rust.splitlines() if line.startswith("host: "))
    subprocess.run([sys.executable, str(ROOT / "scripts/package-release.py"), "restic",
                    "--target", target, "--out", bundle], check=True, stdout=sys.stderr)


def unused_port():
    for port in range(18700, 18800):
        with socket.socket() as probe:
            try:
                probe.bind(("127.0.0.1", port))
            except OSError:
                continue
            return str(port)
    raise RuntimeError("No available port in 18700–18799")


def isolated_env(home):
    port = unused_port()
    return dict(DISABLED_ENV, HOME=str(home), TMPDIR=str(home), STEVE_HOME=str(home),
                STEVE_PORT=port, STEVE_HOST="127.0.0.1", STEVE_PUBLIC_URL=f"http://127.0.0.1:{port}",
                STEVE_BACKUP="off", STEVE_NODE_LISTEN="0", STEVE_VAULT_KEYSTORE="file",
                STEVE_NO_RESUME="1", STEVE_FORBID_REAL_HOSTS="1")


def scrape(home):
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(2)
        stream.connect(str(home / "run/hub.sock"))
        stream.sendall(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        with http.client.HTTPResponse(stream) as response:
            response.begin()
            if response.status != 200:
                raise RuntimeError(f"Management metrics returned HTTP {response.status}")
            return parse(response.read().decode())


@contextlib.contextmanager
def running_hub(hub, home, env):
    with tempfile.TemporaryFile(dir=home) as log:
        process = subprocess.Popen([hub], cwd=home, env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        try:
            deadline = time.monotonic() + 60
            while process.poll() is None and time.monotonic() < deadline:
                try:
                    scrape(home)
                    break
                except (OSError, http.client.HTTPException, RuntimeError):
                    time.sleep(0.1)
            else:
                log.seek(max(0, log.tell() - 4096))
                raise RuntimeError("Isolated Hub did not start\n" + log.read().decode(errors="replace"))
            yield
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def seed_database(db, megabytes):
    with contextlib.closing(sqlite3.connect(db)) as connection:
        with connection:
            connection.execute("CREATE TABLE backup_benchmark(payload BLOB NOT NULL)")
            connection.execute("""
                WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?)
                INSERT INTO backup_benchmark SELECT randomblob(1000000) FROM n
                """, (megabytes,))
        if connection.execute("PRAGMA wal_checkpoint(TRUNCATE)").fetchone()[0]:
            raise RuntimeError("Could not checkpoint the isolated benchmark database")


def backup(hub, home, env, purpose):
    start = time.perf_counter()
    with subprocess.Popen([hub, "--admin", "backup", "--offline", "--reason", purpose],
                          cwd=home, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          text=True, start_new_session=True) as process:
        try:
            stdout, stderr = process.communicate(timeout=150 if purpose == "pre-upgrade" else 3630)
        except BaseException:
            if process.returncode is None:
                with contextlib.suppress(ProcessLookupError):
                    os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            raise
    elapsed = time.perf_counter() - start
    if process.returncode:
        raise RuntimeError(f"{purpose} backup failed after {elapsed:.3f} s\n{stderr}")
    receipt = json.loads(stdout)
    if receipt["purpose"] != purpose:
        raise RuntimeError("Backup receipt purpose did not match the command")
    return dict(elapsed_seconds=elapsed, written_bytes=receipt["written_bytes"],
                database_sha256=receipt["database"]["sha256"])


def initialization_metrics(home):
    labels = (("operation", "initialize"), ("purpose", "repository"))
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        samples = scrape(home)
        elapsed = samples.get(("steve_backup_operation_seconds_sum", labels))
        written = samples.get(("steve_backup_written_bytes_total", labels))
        count = samples.get(("steve_backup_operation_seconds_count", labels))
        if elapsed is not None and written is not None and count is not None:
            if count != 1:
                raise RuntimeError("Expected exactly one repository initialization")
            return dict(elapsed_seconds=elapsed, written_bytes=int(written))
        time.sleep(0.2)
    raise RuntimeError("The Hub did not report repository initialization")


def benchmark(bundle, home, megabytes, report):
    hub = bundle / "steve-hub"
    env = isolated_env(home)
    report["hub"] = json.loads(subprocess.check_output([hub, "--build-info"], cwd=home, env=env))
    report["restic"] = subprocess.check_output([bundle / "restic", "version"], cwd=home, env=env, text=True).strip()
    db = home / "data/steve.sqlite3"
    with running_hub(hub, home, env):
        if not db.is_file():
            raise RuntimeError("The isolated Hub has no SQLite database")
    seed_database(db, megabytes)
    report["database_bytes"] = db.stat().st_size
    report["first_pre_upgrade"] = backup(hub, home, env, "pre-upgrade")
    report["existing_repository_pre_upgrade"] = backup(hub, home, env, "pre-upgrade")
    first_daily = backup(hub, home, env, "daily")
    report["unchanged_daily"] = backup(hub, home, env, "daily")
    if len({report["first_pre_upgrade"]["database_sha256"],
            report["existing_repository_pre_upgrade"]["database_sha256"],
            first_daily["database_sha256"], report["unchanged_daily"]["database_sha256"]}) != 1:
        raise RuntimeError("The verified database changed between backup samples")
    with running_hub(hub, home, env):
        report["first_initialization"] = initialization_metrics(home)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database-mb", type=int, default=1500, help="Decimal MB of random SQLite payload (default: 1500)")
    args = parser.parse_args()
    if args.database_mb < 1:
        parser.error("--database-mb must be positive")
    report = {"payload_bytes": args.database_mb * 1_000_000, "build_profile": "release"}
    try:
        subprocess.run(["cargo", "build", "--locked", "--release", "--target-dir", TARGET,
                        "-p", "steve-hub", "--bin", "steve-hub"], cwd=RUST, check=True, stdout=sys.stderr)
        with tempfile.TemporaryDirectory(dir=TARGET, prefix="backup-bench-") as stage, \
                tempfile.TemporaryDirectory(dir="/tmp", prefix="steve-backup-bench-") as sandbox:
            bundle, home = Path(stage), Path(sandbox).resolve()
            os.link(TARGET / "release/steve-hub", bundle / "steve-hub")
            stage_engine(bundle)
            benchmark(bundle, home, args.database_mb, report)
    except (OSError, RuntimeError, ValueError, KeyError, sqlite3.Error, subprocess.SubprocessError) as error:
        report["error"] = str(error)
    print(json.dumps(report, indent=2))
    return int("error" in report)


if __name__ == "__main__":
    raise SystemExit(main())
