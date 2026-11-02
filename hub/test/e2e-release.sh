#!/bin/bash
# Release hand-off end to end, without a model and without touching ~/.steve, launchd or port 8440:
# a temp git repo as Steve's repo, a temp STEVE_HOME whose `current` holds this checkout's ops/release.sh,
# a throwaway Hub from this checkout with the scripted adapter (test/fake-acp.js); the release restarts that Hub
# through STEVE_RESTART_CMD. Usage: hub/test/e2e-release.sh
set -euo pipefail
self="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"; cd "$(dirname "$self")/../.."
start() {
  local v; v=$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$STEVE_HOME/current/release.json")
  STEVE_VERSION=$v STEVE_DEV_LOGIN=1 STEVE_PUBLIC_URL="http://127.0.0.1:$STEVE_PORT" nohup node --import tsx "$ROOT/hub/src/main.ts" >>"$STEVE_HOME/hub.log" 2>&1 & echo $! >"$STEVE_HOME/hub.pid"
}
stop() { local p; p=$(cat "$STEVE_HOME/hub.pid" 2>/dev/null) || return 0; kill "$p" 2>/dev/null || true; while kill -0 "$p" 2>/dev/null; do sleep 0.2; done; }
if [ "${1:-}" = __restart ]; then stop; start; exit 0; fi

export ROOT; ROOT=$(pwd)
T=$(mktemp -d -t steve-e2e-release)
export STEVE_HOME=$T/home STEVE_REPO=$T/repo FAKE_ACP_LOG=$T/prompts.jsonl
export STEVE_PORT; STEVE_PORT=$(node -e 'const s=require("net").createServer().listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
[ "$STEVE_PORT" != 8440 ] || { echo "refusing port 8440"; exit 1; }
export STEVE_ADAPTER_CLAUDE=$ROOT/hub/test/fake-acp.js STEVE_RESTART_CMD="bash $self __restart" STEVE_LABEL=io.github.infranite.steve-e2e-unused STEVE_DRAIN_S=120
trap 'rc=$?; stop; if [ $rc = 0 ]; then rm -rf -- "$T"; else echo "failed; hub log: $STEVE_HOME/hub.log, release logs: $STEVE_HOME/logs"; fi' EXIT

# Steve's repo: a tiny package that builds instantly
mkdir -p "$STEVE_REPO" "$STEVE_HOME/releases/v0/ops"
git -C "$STEVE_REPO" init -q -b main
git -C "$STEVE_REPO" config user.email e2e@example.invalid; git -C "$STEVE_REPO" config user.name e2e
echo '{"name":"fake","version":"0.0.0","private":true,"scripts":{"build":"true","build:hub":"true"}}' >"$STEVE_REPO/package.json"
echo '{"name":"fake","version":"0.0.0","lockfileVersion":3,"requires":true,"packages":{"":{"name":"fake","version":"0.0.0"}}}' >"$STEVE_REPO/package-lock.json"
echo base >"$STEVE_REPO/shared.txt"
git -C "$STEVE_REPO" add -A; git -C "$STEVE_REPO" commit -q -m base
# the release in `current` is the one whose release.sh runs: this checkout's
cp ops/release.sh ops/release-lib.sh "$STEVE_HOME/releases/v0/ops/"; echo '{"version":"v0"}' >"$STEVE_HOME/releases/v0/release.json"
ln -s "$STEVE_HOME/releases/v0" "$STEVE_HOME/current"
# a web build is installed and the repo has no web/: these releases are hub-only (ops/test-release.sh covers the web side)
mkdir -p "$STEVE_HOME/web/v0"; echo '{"version":"v0"}' >"$STEVE_HOME/web/v0/release.json"; ln -s "$STEVE_HOME/web/v0" "$STEVE_HOME/web/current"

start
base="http://127.0.0.1:$STEVE_PORT"
for _ in $(seq 60); do curl -sf "$base/healthz" >/dev/null && break; sleep 0.5; done
echo "hub on $base, STEVE_HOME=$STEVE_HOME"
node hub/test/e2e-release.mjs "$base"
