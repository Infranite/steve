#!/usr/bin/env python3
"""Boot an actual identity-bearing Hub; verify candidate gating and reject forged metadata."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", type=Path)
parser.add_argument("--version", required=True)
parser.add_argument("--commit", required=True)
parser.add_argument("--target")
args = parser.parse_args()
binary = args.binary.resolve(strict=True)
identity = json.loads(subprocess.check_output([str(binary), "--build-info"], timeout=15))
assert identity["version"] == args.version and identity["commit"] == args.commit, identity
if args.target:
    assert identity["target"] == args.target, identity
assert type(identity["schema"]) is int and identity["schema"] > 0, identity

with tempfile.TemporaryDirectory(prefix="steve-release-identity-") as temp:
    root = Path(temp)
    home, keys, web, os_home = (root / name for name in ("runtime", "keys", "web", "user-home"))
    for path in (home, keys, web, os_home):
        path.mkdir(mode=0o700)
    (web / "index.html").write_text("<html>isolated release identity test</html>")
    (web / "release.json").write_text(json.dumps({"version": args.version}))
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    env = {k: os.environ[k] for k in ("PATH", "TMPDIR", "SYSTEMROOT") if k in os.environ}
    env.update(HOME=str(os_home), CODEX_HOME=str(root / "codex-home"),
        STEVE_HOME=str(home), STEVE_PORT=str(port), STEVE_HOST="127.0.0.1",
        STEVE_WEB_DIR=str(web), STEVE_VAULT_KEYSTORE="dir", STEVE_VAULT_KEY_DIR=str(keys),
        STEVE_VERSION="forged-runtime-version", STEVE_NO_RESUME="1", STEVE_NODE_MODE="off", STEVE_REPO=str(root / "repo"),
        STEVE_ADAPTER_CLAUDE="/nonexistent", STEVE_ADAPTER_CODEX="/nonexistent",
        CLAUDE_CODE_EXECUTABLE="/nonexistent", CODEX_EXECUTABLE="/nonexistent")

    @contextmanager
    def running(runtime_env, label):
        with (root / (label + ".log")).open("w+") as log:
            process = subprocess.Popen([str(binary)], env=runtime_env, cwd=root,
                stdout=log, stderr=log, start_new_session=True)
            try:
                yield process
            except Exception:
                log.flush(); log.seek(0)
                print(log.read()[-6000:])
                raise
            finally:
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=5)

    def wait_health(process, maintenance):
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise AssertionError(f"Hub exited before health: code={process.returncode}")
            try:
                with urllib.request.urlopen(f"http://127.0.0.1:{port}/healthz", timeout=1) as response:
                    health = json.load(response)
                assert health["version"] == args.version, health
                assert health["build_commit"] == args.commit, health
                assert health["web_version"] == args.version, health
                if bool(health.get("maintenance")) == maintenance:
                    return health
            except OSError:
                pass
            time.sleep(.1)
        raise AssertionError(f"Hub health timeout; expected maintenance={maintenance}")

    def candidate(commit, label):
        ident = str(uuid.uuid4())
        target = root / label
        target.mkdir()
        (target / "release.json").write_text(json.dumps(dict(format_version=1, version=args.version,
            commit=commit, hub_version=args.version, web_version=args.version, api_protocol=1,
            schema=dict(min_read=identity["schema"], max_read=identity["schema"], target=identity["schema"]), assets=[])))
        operations = home / "updates/operations"
        operations.mkdir(parents=True, exist_ok=True)
        receipt = operations / f"{ident}.json"
        receipt.write_text(json.dumps(dict(id=ident, idempotency_key=label,
            version=args.version, phase="verifying", created_at=0, updated_at=0, requested_by="fixture",
            error=None, previous=None, target=str(target))))
        return ident, receipt

    with running(env, "normal") as process:
        wait_health(process, False)
        print("PASS actual Hub reports compiled version/commit/target despite forged runtime version")

    # A negative-only test could pass if all candidates were rejected. Prove a matching binary
    # serves maintenance only until a durable commit and then starts its ordinary business router.
    ident, receipt = candidate(args.commit, "matching-bundle")
    with running(dict(env, STEVE_UPGRADE_ID=ident), "matching-gate") as process:
        health = wait_health(process, True)
        assert health["upgrade_id"] == ident, health
        try:
            urllib.request.urlopen(f"http://127.0.0.1:{port}/api/state", timeout=2)
        except urllib.error.HTTPError as error:
            assert error.code == 503, error.code
        else:
            raise AssertionError("candidate business API was reachable before commit")
        operation = json.loads(receipt.read_text())
        operation["phase"] = "committed"
        staged = receipt.with_suffix(".tmp")
        staged.write_text(json.dumps(operation))
        staged.replace(receipt)
        wait_health(process, False)
        print("PASS matching compiled identity gates business APIs until durable commit")

    wrong_commit = ("b" if args.commit != "b" * 40 else "a") * 40
    ident, _ = candidate(wrong_commit, "forged-bundle")
    failed = subprocess.run([str(binary)], env=dict(env, STEVE_UPGRADE_ID=ident), cwd=root,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, timeout=30)
    assert failed.returncode != 0 and "compiled identity" in failed.stderr, (failed.returncode, failed.stdout, failed.stderr)
    print("PASS actual candidate gate rejects a mismatched compiled commit before business startup")
