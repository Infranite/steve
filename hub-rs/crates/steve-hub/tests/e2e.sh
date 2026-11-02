#!/bin/bash
# hub/test/e2e.sh with a configurable Hub command, so the same suites run against the Rust binary:
#   STEVE_HUB_CMD="$PWD/hub-rs/target/release/steve-hub" hub-rs/crates/steve-hub/tests/e2e.sh [mainline|restart|attach ...]
# Default command: node --import tsx hub/src/main.ts (the TS Hub, run from the repo root).
# A fresh STEVE_HOME (mktemp -d), a free port, the real Claude Code harness. Never touches ~/.steve or port 8440.
set -euo pipefail
self="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"; cd "$(dirname "$self")/../../../.."
hub_cmd=${STEVE_HUB_CMD:-node --import tsx hub/src/main.ts}
start() { STEVE_DEV_LOGIN=1 STEVE_REPO=/nonexistent STEVE_PUBLIC_URL="http://127.0.0.1:$STEVE_PORT" nohup sh -c "exec $hub_cmd" >>"$STEVE_HOME/hub.log" 2>&1 & echo $! >"$STEVE_HOME/hub.pid"; }
stop() { local p; p=$(cat "$STEVE_HOME/hub.pid" 2>/dev/null) || return 0; kill "$p" 2>/dev/null || true; while kill -0 "$p" 2>/dev/null; do sleep 0.2; done; }
if [ "${1:-}" = __restart ]; then stop; start; exit 0; fi

export STEVE_HOME; STEVE_HOME=$(mktemp -d -t steve-e2e)
export STEVE_PORT; STEVE_PORT=$(node -e 'const s=require("net").createServer().listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
[ "$STEVE_PORT" != 8440 ] || { echo "refusing port 8440"; exit 1; }
export STEVE_HUB_CMD="$hub_cmd"
trap 'rc=$?; stop; if [ $rc = 0 ]; then rm -rf -- "$STEVE_HOME"; else echo "failed; hub log: $STEVE_HOME/hub.log"; fi' EXIT
start
base="http://127.0.0.1:$STEVE_PORT"
for _ in $(seq 60); do curl -sf "$base/healthz" >/dev/null && break; sleep 0.5; done
echo "hub ($hub_cmd) on $base, STEVE_HOME=$STEVE_HOME"
[ $# -gt 0 ] || set -- mainline restart attach
for x in "$@"; do
  echo "── e2e-$x"
  if [ "$x" = restart ]; then node hub/test/e2e-restart.mjs "$base" "$self __restart"; else node "hub/test/e2e-$x.mjs" "$base"; fi
done
