#!/bin/bash
# Hub/web release split and the TS → Rust Hub cutover, end to end, without touching ~/.steve, launchd or port 8440: a temp
# STEVE_HOME, a temp git repo shaped like Steve's (hub/ and web/ workspaces whose builds take no time; later hub-rs/, a
# tiny stand-in Rust Hub from ops/test-fixtures/hub-rs built with the real cargo), Hubs on a free port (STEVE_NO_RESUME=1,
# adapters /usr/bin/false, file vault keystore, no real hosts) started like launchd does, through the release's
# ops/run.sh, which release.sh restarts through STEVE_RESTART_CMD. The fixture's TS Hub is this checkout's, via tsx.
# Cases: bootstrap, web-only (no restart), broken web build, failed web check (rollback), hub only, hub+web, --web, dirty
# main; the schema gate (ops/schema_gate.py): a Rust Hub failing to start leaves the database alone (back to TS), one
# that migrated and then fails its health check stays (degraded, TS can't read v11); the cutover, hub+web and web-only
# on the Rust Hub; --rollback refused onto TS and onto an older schema, allowed onto the same schema; history.jsonl and
# the logs; then ops/migrate-web-split.sh on a pre-split home.
# --real: instead, the real thing from this checkout's HEAD (commit first): bootstrap main (TS), release HEAD (real npm ci,
# web build and `cargo build --release -p steve-hub`), check login / API / SSE / drain on the Rust Hub, a second Rust
# release (incremental cargo), --rollback onto TS refused and onto the first Rust release allowed, forward again. Takes
# minutes.
# Usage: ops/test-release.sh [-k] [--real]   (-k keeps the temp dir)
set -euo pipefail
self="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"; ROOT=$(cd "$(dirname "$self")/.." && pwd); cd "$ROOT"
# the current release, the way launchd runs it
start() {
  STEVE_PUBLIC_URL="http://127.0.0.1:$STEVE_PORT" nohup /bin/zsh "$STEVE_HOME/current/ops/run.sh" >>"$STEVE_HOME/hub.log" 2>&1 </dev/null & echo $! >"$STEVE_HOME/hub.pid"
}
# this checkout's TS Hub on a home whose release dirs have no hub (the migration case)
start_direct() {
  local v; v=$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' "$STEVE_HOME/current/release.json")
  STEVE_VERSION=$v STEVE_WEB_DIR=$STEVE_HOME/web/current STEVE_PUBLIC_URL="http://127.0.0.1:$STEVE_PORT" \
    nohup node --import tsx "$ROOT/hub/src/main.ts" >>"$STEVE_HOME/hub.log" 2>&1 </dev/null & echo $! >"$STEVE_HOME/hub.pid"
}
stop() { local p; p=$(cat "$STEVE_HOME/hub.pid" 2>/dev/null) || return 0; kill "$p" 2>/dev/null || true; while kill -0 "$p" 2>/dev/null; do sleep 0.2; done; }
if [ "${1:-}" = __restart ]; then stop; start; exit 0; fi
KEEP=0 REAL=0
for a in "$@"; do case $a in -k) KEEP=1 ;; --real) REAL=1 ;; *) echo "usage: $0 [-k] [--real]"; exit 2 ;; esac; done

T=$(mktemp -d -t steve-test-release)
export STEVE_HOME=$T/home STEVE_REPO=$T/repo STEVE_NO_RESUME=1 TEST_ROOT=$ROOT
export STEVE_ADAPTER_CLAUDE=/usr/bin/false STEVE_ADAPTER_CODEX=/usr/bin/false CLAUDE_CODE_EXECUTABLE=/usr/bin/false
export STEVE_VAULT_KEYSTORE=file STEVE_FORBID_REAL_HOSTS=1 STEVE_DEV_LOGIN=1 STEVE_HEALTH_TRIES=12 STEVE_UNSIGNED=1
unset STEVE_CARGO_TARGET_DIR
export STEVE_PORT; STEVE_PORT=$(node -e 'const s=require("net").createServer().listen(0,"127.0.0.1",()=>{console.log(s.address().port);s.close()})')
[ "$STEVE_PORT" != 8440 ] || { echo "refusing port 8440"; exit 1; }
export STEVE_RESTART_CMD="bash $self __restart" STEVE_LABEL=io.github.infranite.steve-test-unused STEVE_WEB_KEEP=2
unset STEVE_DRAIN_TOKEN STEVE_FIRST STEVE_WEB_DIR
trap 'rc=$?; stop; if [ $rc = 0 ] && [ $KEEP = 0 ]; then rm -rf -- "$T"; else echo "kept $T (hub log: $STEVE_HOME/hub.log, release logs: $STEVE_HOME/logs/release)"; fi' EXIT
base="http://127.0.0.1:$STEVE_PORT"
REL="zsh $ROOT/ops/release.sh"
pass=0
ok() { echo "  ✓ $1"; pass=$((pass + 1)); }
no() { echo "  ✗ $1"; exit 1; }
check() { if eval "$1"; then ok "$2"; else no "$2  [$1]"; fi; }
# last.json is rewritten by the Hub once reported (JSON.stringify with spaces): allow both
field() { sed -n "s/.*\"$2\": *\"\\([^\"]*\\)\".*/\\1/p" "$1" | head -1; }
hz() { curl -s "$base/healthz"; }
hzf() { hz | sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p"; }
pid() { cat "$STEVE_HOME/hub.pid"; }
served() { curl -s "$base/$1" | sed -n 's/.*name="steve-web-version" content="\([^"]*\)".*/\1/p'; }
webcur() { basename "$(readlink "$STEVE_HOME/web/current")"; }
hubcur() { basename "$(readlink "$STEVE_HOME/current")"; }
hist() { wc -l <"$STEVE_HOME/releases/history.jsonl" | tr -d ' '; }
lastline() { tail -1 "$STEVE_HOME/releases/history.jsonl"; }
G() { git -C "$STEVE_REPO" "$@"; }
# what runs as the Hub: its command line (run.sh execs it, so the pid is the Hub's)
proc() { ps -o command= -p "$(pid)" 2>/dev/null || true; }
is_rust() { proc | grep -q 'bin/steve-hub'; }
is_ts() { proc | grep -q 'dist/main.js'; }
hfield() { lastline | sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }
schema() { sqlite3 "$STEVE_HOME/data/steve.sqlite3" "SELECT v FROM schema_version"; }
# dev login and an API call: the web app's first two requests
api_ok() { local c; c=$(curl -s -o /dev/null -D - "$base/auth/dev" | tr -d '\r' | sed -n 's/^[Ss]et-[Cc]ookie: \([^;]*\).*/\1/p' | head -1); [ -n "$c" ] && [ "$(curl -s -o /dev/null -w '%{http_code}' -H "cookie: $c" "$base/api/state")" = 200 ]; }
# a branch off main with one commit touching the given files
branch() { local b=$1; shift; G checkout -q -b "$b" main; for f in "$@"; do mkdir -p "$STEVE_REPO/$(dirname "$f")"; echo "$b $RANDOM" >>"$STEVE_REPO/$f"; done; G add -A; G commit -q -m "$b"; G checkout -q main; }
waitup() { for _ in $(seq 80); do curl -sf "$base/healthz" >/dev/null && return 0; sleep 0.25; done; no "hub did not come up"; }

if [ $REAL = 1 ]; then source "$ROOT/ops/test-release-real.sh"; exit 0; fi

# ── Steve's repo, in miniature ──
mkdir -p "$STEVE_REPO/hub" "$STEVE_REPO/web" "$STEVE_REPO/ops"
cp "$ROOT"/ops/*.sh "$STEVE_REPO/ops/"
cat >"$STEVE_REPO/package.json" <<'EOF'
{ "name": "steve-fake", "private": true, "workspaces": ["hub", "web"],
  "scripts": { "build:hub": "npm -w hub run build", "build:web": "npm -w web run build" } }
EOF
echo '{ "name": "@fake/hub", "private": true, "type": "module", "scripts": { "build": "node build.mjs" } }' >"$STEVE_REPO/hub/package.json"
cp "$ROOT/ops/test-fixtures/ts-hub-build.mjs" "$STEVE_REPO/hub/build.mjs"
echo '{ "name": "@fake/web", "private": true, "scripts": { "build": "node build.mjs" } }' >"$STEVE_REPO/web/package.json"
cat >"$STEVE_REPO/web/build.mjs" <<'EOF'
// a vite-shaped build: dist/index.html naming one hashed asset. FAIL_BUILD fails it, BROKEN_ASSET names a missing asset
import { createHash } from "node:crypto"
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
console.log("fake web build")
if (existsSync("FAIL_BUILD")) { console.error("error: the build fails on purpose"); process.exit(1) }
rmSync("dist", { recursive: true, force: true }); mkdirSync("dist/assets", { recursive: true })
const src = readFileSync("src.txt", "utf8"), h = createHash("sha1").update(src).digest("hex").slice(0, 8)
writeFileSync(`dist/assets/index-${h}.js`, `console.log(${JSON.stringify(src)})\n`)
const ref = existsSync("BROKEN_ASSET") ? `/assets/missing-${h}.js` : `/assets/index-${h}.js`
writeFileSync("dist/index.html", `<!doctype html>\n<html>\n  <head>\n    <script type="module" crossorigin src="${ref}"></script>\n  </head>\n  <body><div id="root"></div></body>\n</html>\n`)
console.log("built", ref)
EOF
echo v1 >"$STEVE_REPO/web/src.txt"; echo v1 >"$STEVE_REPO/hub/src.txt"
(cd "$STEVE_REPO" && npm install --package-lock-only --no-audit --no-fund --silent)
G init -q -b main; G config user.email test@example.invalid; G config user.name test
G add -A; G commit -q -m base

echo "── bootstrap (STEVE_FIRST): hub and web, nothing running"
STEVE_FIRST=1 $REL main >"$T/out-first.txt" 2>&1 || { cat "$T/out-first.txt"; no "first release failed"; }
check '[ -f "$STEVE_HOME/current/release.json" ] && [ -f "$STEVE_HOME/web/current/index.html" ]' "current and web/current installed"
H1=$(hubcur) W1=$(webcur)
check '[ "$H1" = "$W1" ] && [ "$(field "$STEVE_HOME/releases/last.json" kind)" = "hub+web" ]' "bootstrap is hub+web, one version ($H1)"
check '[ "$(hist)" = 2 ]' "history: a hub and a web line"
check '[ ! -e "$STEVE_HOME/current/web/dist" ]' "the hub release dir carries no web build"
start; waitup
check '[ "$(hzf version)" = "$H1" ] && [ "$(hzf web_version)" = "$W1" ]' "/healthz: version $H1, web_version $W1"
check '[ "$(served "")" = "$W1" ] && [ "$(served "items/ST-1")" = "$W1" ]' "/ and the SPA fallback serve web $W1"
check 'curl -sI "$base/" | grep -qi "^cache-control: no-store" && curl -sI "$base/items/ST-1" | grep -qi "^cache-control: no-store"' "index.html (/ and fallback) is cache-control: no-store"
asset=$(curl -s "$base/" | grep -oE '/assets/[^"]+' | head -1)
check 'curl -sI "$base$asset" | grep -qi "^cache-control: public, max-age=31536000, immutable"' "hashed asset is immutable"
check '[ "$(curl -s -o /dev/null -w "%{http_code}" "$base/release.json")" = 404 ]' "web/current/release.json is not served"
P0=$(pid)

echo "── web-only change: switch without a restart"
branch w1 web/src.txt
M0=$(G rev-parse main)
$REL w1 ST-9 >"$T/out-w1.txt" 2>&1 || { cat "$T/out-w1.txt"; no "web-only release failed"; }
W2=$(webcur)
check '[ "$W2" != "$W1" ] && [ "$(served "")" = "$W2" ]' "web switched to $W2 and served"
check '[ "$(pid)" = "$P0" ] && kill -0 "$P0" && [ "$(hzf version)" = "$H1" ]' "Hub not restarted (pid $P0, version $H1)"
check '[ "$(hzf web_version)" = "$W2" ]' "/healthz web_version follows the switch"
check '[ "$(field "$STEVE_HOME/releases/last.json" kind)" = web ] && grep -qE "\"ok\": ?true" "$STEVE_HOME/releases/last.json" && [ "$(field "$STEVE_HOME/releases/last.json" item)" = ST-9 ]' "last.json: kind web, ok, item ST-9"
check 'grep -q "\"ok\":true" "$STEVE_HOME/web/last.json" && [ "$(field "$STEVE_HOME/web/last.json" version)" = "$W2" ]' "web/last.json: ok $W2"
check '[ "$(hist)" = 3 ] && lastline | grep -q "\"kind\":\"web\".*\"prev\":\"$W1\".*\"files_changed\":\[\"web/src.txt\"\].*\"ok\":true"' "history: one web line, prev $W1, files [web/src.txt]"
for _ in $(seq 40); do grep -qE '"reported": ?true' "$STEVE_HOME/releases/last.json" && break; sleep 0.25; done
check 'grep -qE "\"reported\": ?true" "$STEVE_HOME/releases/last.json" && [ "$(pid)" = "$P0" ]' "the running Hub picked up and reported the web release"
check '! grep -q "== hub" "$STEVE_HOME/logs/release/$W2.log" && ! grep -q "== hub: drain" "$STEVE_HOME/logs/release/$W2.log"' "no hub step, no drain in the log"
check '[ "$(G rev-parse main)" = "$(G rev-parse w1)" ]' "main fast-forwarded to w1"
check 'is_ts && grep -q "run.sh: TS Hub $H1" "$STEVE_HOME/hub.log"' "run.sh started the TS Hub (no bin/steve-hub in the release)"

echo "── broken web build: fails, the old build keeps serving"
branch w2 web/src.txt web/FAIL_BUILD
M1=$(G rev-parse main) n=$(ls -d "$STEVE_HOME"/web/2* | wc -l)
if $REL w2 >"$T/out-w2.txt" 2>&1; then cat "$T/out-w2.txt"; no "a broken build released"; fi
check '[ "$(webcur)" = "$W2" ] && [ "$(served "")" = "$W2" ] && [ "$(pid)" = "$P0" ]' "still serving $W2, Hub untouched"
check '[ "$(G rev-parse main)" = "$M1" ]' "main reset to where it was"
check '[ "$(ls -d "$STEVE_HOME"/web/2* | wc -l)" = "$n" ] && ! ls -d "$STEVE_HOME"/web/.build-* >/dev/null 2>&1' "no build dir left behind"
check 'lastline | grep -q "\"kind\":\"web\".*\"ok\":false,\"error\":\"前端构建失败.*\"rolled_back\":false"' "history: web ok:false, not rolled back"
check 'grep -qE "\"ok\": ?false" "$STEVE_HOME/releases/last.json" && grep -q "前端构建失败" "$STEVE_HOME/releases/last.json"' "last.json: the failure for the Hub to report"
F=$(lastline | sed -n 's/.*"log":"\([^"]*\)".*/\1/p')
check 'grep -q "error: the build fails on purpose" "$F" && grep -q "exit 1" "$F"' "the build output is in the log ($F)"

echo "── web check fails after the switch: rolled back"
branch w3 web/src.txt web/BROKEN_ASSET
if $REL w3 >"$T/out-w3.txt" 2>&1; then cat "$T/out-w3.txt"; no "a broken asset released"; fi
check '[ "$(webcur)" = "$W2" ] && [ "$(served "")" = "$W2" ] && [ "$(pid)" = "$P0" ]' "back on $W2, Hub untouched"
check '[ "$(G rev-parse main)" = "$M1" ]' "main reset"
check 'lastline | grep -q "\"kind\":\"web\".*\"ok\":false.*\"rolled_back\":true"' "history: web ok:false, rolled_back:true"
F=$(lastline | sed -n 's/.*"log":"\([^"]*\)".*/\1/p')
check 'grep -q "GET /assets/missing-.*: 404" "$F" && grep -q "web: roll back to $W2" "$F"' "the log shows the 404 and the rollback"

echo "── hub-only change: restart, web untouched"
branch h1 hub/src.txt
$REL h1 >"$T/out-h1.txt" 2>&1 || { cat "$T/out-h1.txt"; no "hub release failed"; }
H2=$(hubcur)
check '[ "$H2" != "$H1" ] && [ "$(hzf version)" = "$H2" ] && [ "$(pid)" != "$P0" ]' "Hub restarted on $H2"
check '[ "$(webcur)" = "$W2" ] && [ "$(served "")" = "$W2" ] && [ "$(hzf web_version)" = "$W2" ]' "web still $W2 after the restart"
check 'lastline | grep -q "\"kind\":\"hub\".*\"prev\":\"$H1\".*\"files_changed\":\[\"hub/src.txt\"\].*\"ok\":true" && [ "$(field "$STEVE_HOME/releases/last.json" kind)" = hub ]' "history and last.json: kind hub"
P1=$(pid)

echo "── hub+web change: both, hub first"
branch hw hub/src.txt web/src.txt
$REL hw ST-10 >"$T/out-hw.txt" 2>&1 || { cat "$T/out-hw.txt"; no "hub+web release failed"; }
H3=$(hubcur) W3=$(webcur)
check '[ "$H3" = "$W3" ] && [ "$(hzf version)" = "$H3" ] && [ "$(hzf web_version)" = "$W3" ] && [ "$(served "")" = "$W3" ] && [ "$(pid)" != "$P1" ]' "Hub restarted and web switched, both $H3"
check '[ "$(field "$STEVE_HOME/releases/last.json" kind)" = hub+web ] && grep -qE "\"ok\": ?true" "$STEVE_HOME/releases/last.json"' "last.json: kind hub+web, ok"
check 'tail -2 "$STEVE_HOME/releases/history.jsonl" | head -1 | grep -q "\"kind\":\"hub\".*\"ok\":true" && lastline | grep -q "\"kind\":\"web\".*\"prev\":\"$W2\".*\"ok\":true"' "history: hub line then web line"
check 'L=$STEVE_HOME/logs/release/$H3.log; h=$(grep -n "== hub: health check" "$L" | cut -d: -f1); w=$(grep -n "== web: switch" "$L" | cut -d: -f1); [ -n "$h" ] && [ -n "$w" ] && [ "$h" -lt "$w" ]' "the log shows the hub before the web"
P2=$(pid)

echo "── --web with nothing changed (release-web.sh)"
zsh "$ROOT/ops/release-web.sh" main >"$T/out-wm.txt" 2>&1 || { cat "$T/out-wm.txt"; no "release-web.sh main failed"; }
W4=$(webcur)
check '[ "$W4" != "$W3" ] && [ "$(served "")" = "$W4" ] && [ "$(pid)" = "$P2" ]' "web rebuilt as $W4 without a restart"
check '[ "$(ls -d "$STEVE_HOME"/web/2* | wc -l | tr -d " ")" -le 3 ] && [ -d "$STEVE_HOME/web/$W3" ]' "old web builds pruned (keep $STEVE_WEB_KEEP + the previous)"

echo "── dirty main: refused, logged"
echo dirt >>"$STEVE_REPO/hub/src.txt"
if $REL main >"$T/out-dirty.txt" 2>&1; then no "released from a dirty main"; fi
G checkout -q -- hub/src.txt
check 'lastline | grep -q "\"kind\":\"preflight\".*\"ok\":false.*未提交"' "history: preflight failure"

# ── the Rust Hub: hub-rs/ appears in the tree ──
# a branch off main that adds or changes hub-rs/ (the stand-in Rust Hub, mode ok | bad-health | fail-start, and the
# schema it knows and migrates to), plus files
rust_branch() { local b=$1 mode=$2 sv=$3; shift 3; G checkout -q -b "$b" main
  rm -rf "$STEVE_REPO/hub-rs"; cp -R "$ROOT/ops/test-fixtures/hub-rs" "$STEVE_REPO/hub-rs"
  echo "$mode" >"$STEVE_REPO/hub-rs/crates/steve-hub/mode.txt"; echo "$sv" >"$STEVE_REPO/hub-rs/crates/steve-hub/schema.txt"
  echo "// $b $RANDOM" >>"$STEVE_REPO/hub-rs/crates/steve-hub/src/main.rs"
  for f in "$@"; do echo "$b $RANDOM" >>"$STEVE_REPO/$f"; done; G add -A; G commit -q -m "$b"; G checkout -q main; }
TSV=$(hubcur) M2=$(G rev-parse main)
check '[ "$(schema)" = 3 ] && is_ts' "before: the TS Hub $TSV on schema v3"

echo "── Rust Hub fails to start, the database untouched: back to the TS release"
rust_branch rfail fail-start 11
if $REL rfail >"$T/out-rfail.txt" 2>&1; then cat "$T/out-rfail.txt"; no "a Rust Hub that does not start released"; fi
check '[ "$(hubcur)" = "$TSV" ] && [ "$(hzf version)" = "$TSV" ] && is_ts && api_ok && [ "$(schema)" = 3 ]' "back on $TSV, TS serving schema v3, login works"
check 'grep -q "failing at start on purpose" "$STEVE_HOME/hub.log" && lastline | grep -q "\"ok\":false.*\"rolled_back\":true.*\"hub\":\"rust\"" && [ "$(G rev-parse main)" = "$M2" ]' "the binary's exit is in the hub log; history rolled back; main reset"
check '! ls "$STEVE_HOME"/releases/*/bin/steve-hub >/dev/null 2>&1' "the failed release dir removed"

echo "── Rust Hub migrates, then fails its health check: the TS release can't read v11, the new one stays (degraded)"
rust_branch rbad bad-health 11
set +e; $REL rbad >"$T/out-rbad.txt" 2>&1; rc=$?; set -e
RB=$(hubcur) F=$(lastline | sed -n 's/.*"log":"\([^"]*\)".*/\1/p')
check '[ $rc = 3 ] && [ "$RB" != "$TSV" ] && [ -x "$STEVE_HOME/current/bin/steve-hub" ] && is_rust' "exit 3, $RB stays current (the Rust Hub runs)"
check '[ "$(schema)" = 11 ] && [ "$(G rev-parse main)" = "$(G rev-parse rbad)" ]' "the database is v11; main keeps the live commit"
check 'lastline | grep -q "\"kind\":\"hub\".*\"ok\":false,\"error\":\"[^\"]*降级[^\"]*\".*\"rolled_back\":false.*\"hub\":\"rust\",\"prev_hub\":\"ts\"" && [ "$(hfield schema_before)" = 3 ] && [ "$(hfield schema_after)" = 11 ]' "history: failed, not rolled back, degraded, schema 3 → 11"
B=$(hfield backup)
check '[[ $B == */backups/pre-rust-$RB.sqlite3 ]] && [ "$(sqlite3 "$B" "SELECT v FROM schema_version")" = 3 ] && grep -q "$B" "$STEVE_HOME/releases/last.json"' "the error names the v3 backup ($(basename "$B"))"
check 'grep -q "说不清自己认识哪个 schema" "$F" && grep -q "not healthy as" "$F"' "the log: the failed check and why the TS release can't take over"
check 'grep -qE "\"degraded\": ?true" "$STEVE_HOME/releases/last.json" && lastline | grep -q "\"degraded\":true"' "last.json and history say degraded (the Hub must not report a rollback)"

PB=$(pid)
echo "── --rollback to the TS release: refused, nothing switched"
if $REL --rollback "$TSV" >"$T/out-rbts.txt" 2>&1; then cat "$T/out-rbts.txt"; no "rolled back onto a database the TS Hub can't read"; fi
check '[ "$(hubcur)" = "$RB" ] && [ "$(pid)" = "$PB" ] && [ "$(schema)" = 11 ]' "still $RB (same pid), the database untouched"
check 'lastline | grep -q "\"kind\":\"rollback\",\"version\":\"$TSV\".*\"ok\":false,\"error\":\"不能回滚到 $TSV" && ! grep -q "rollback: drain" "$T/out-rbts.txt"' "history: refused before any drain"
check '[ "$(field "$STEVE_HOME/releases/last.json" kind)" = rollback ] && grep -q "不能回滚到 $TSV" "$STEVE_HOME/releases/last.json"' "last.json: the refused rollback, for the Hub to note"
if $REL --rollback ts >"$T/out-rbts2.txt" 2>&1; then no "--rollback ts still does something"; fi
check '[ "$(hubcur)" = "$RB" ] && grep -q "no release to roll back to (ts)" "$T/out-rbts2.txt"' "--rollback ts is no longer a target"

echo "── cutover: a good Rust Hub goes live"
rust_branch r1 ok 11
$REL r1 ST-11 >"$T/out-r1.txt" 2>&1 || { cat "$T/out-r1.txt"; no "the Rust release failed"; }
R1=$(hubcur)
check '[ "$R1" != "$RB" ] && [ "$(hzf version)" = "$R1" ] && is_rust && grep -q "run.sh: Rust Hub $R1" "$STEVE_HOME/hub.log"' "run.sh started the Rust Hub $R1"
check '[ "$(field "$STEVE_HOME/current/release.json" hub)" = rust ] && [ "$(field "$STEVE_HOME/current/release.json" hub_sha256)" = "$(shasum -a 256 "$STEVE_HOME/current/bin/steve-hub" | cut -d" " -f1)" ] && grep -q "\"schema\":{\"target\":11}" "$STEVE_HOME/current/release.json" && [ ! -e "$STEVE_HOME/current/hub/dist" ]' "release.json: hub rust, its sha256 and schema 11; no TS Hub built"
check 'lastline | grep -q "\"kind\":\"hub\".*\"prev\":\"$RB\".*\"ok\":true.*\"hub\":\"rust\",\"prev_hub\":\"rust\"" && [[ $(hfield backup) == */pre-$R1.sqlite3 ]] && [ -f "$(hfield backup)" ]' "history: rust → rust with its backup"
check '[ "$(served "")" = "$W4" ] && [ "$(hzf web_version)" = "$W4" ] && [ "$(webcur)" = "$W4" ]' "the Rust Hub serves the web build $W4"
check '[ -x "$STEVE_HOME/build/cargo-target/release/steve-hub" ] && grep -q "cargo: .*(cargo " "$STEVE_HOME/logs/release/$R1.log" && grep -q "STEVE_UNSIGNED=1: no signing keychain" "$STEVE_HOME/logs/release/$R1.log"' "cargo target dir, which cargo, and the test-only unsigned build in the log"
check 'grep -qE "\"ok\": ?true" "$STEVE_HOME/releases/last.json" && [ "$(field "$STEVE_HOME/releases/last.json" item)" = ST-11 ]' "last.json: ok, item ST-11"

echo "── hub+web on the Rust Hub"
rust_branch r2 ok 11 web/src.txt
P3=$(pid)
$REL r2 >"$T/out-r2.txt" 2>&1 || { cat "$T/out-r2.txt"; no "hub+web on Rust failed"; }
R2=$(hubcur) W5=$(webcur)
check '[ "$R2" = "$W5" ] && [ "$R2" != "$R1" ] && [ "$(hzf version)" = "$R2" ] && [ "$(served "")" = "$W5" ] && is_rust && [ "$(pid)" != "$P3" ]' "Rust Hub restarted and the web switched, both $R2"
check '[ "$(field "$STEVE_HOME/releases/last.json" kind)" = hub+web ] && tail -2 "$STEVE_HOME/releases/history.jsonl" | head -1 | grep -q "\"hub\":\"rust\",\"prev_hub\":\"rust\",\"backup\":\"[^\"]*/pre-$R2.sqlite3\""' "history: rust → rust with the usual pre-<ver> backup"

echo "── web-only on the Rust Hub: no restart"
branch rw web/src.txt
P4=$(pid)
$REL rw >"$T/out-rw.txt" 2>&1 || { cat "$T/out-rw.txt"; no "web-only on Rust failed"; }
W6=$(webcur)
check '[ "$W6" != "$W5" ] && [ "$(served "")" = "$W6" ] && [ "$(pid)" = "$P4" ] && [ "$(hzf version)" = "$R2" ]' "web $W6 served by the same Rust Hub (pid $P4)"
M3=$(G rev-parse main) n=$(hist)

echo "── --rollback to the previous Rust release (same schema): allowed"
$REL --rollback >"$T/out-rb.txt" 2>&1 || { cat "$T/out-rb.txt"; no "--rollback to the previous Rust release failed"; }
check '[ "$(hubcur)" = "$R1" ] && [ "$(hzf version)" = "$R1" ] && is_rust && [ "$(served "")" = "$W6" ]' "Rust Hub $R1 serving web $W6"
check 'lastline | grep -q "\"kind\":\"rollback\",\"version\":\"$R1\",\"prev\":\"$R2\".*\"ok\":true.*\"backup\":\"[^\"]*/rollback-[0-9-]*.sqlite3\"" && [ "$(G rev-parse main)" = "$M3" ]' "history: a rollback line with its backup; main untouched"
check '[ "$(field "$STEVE_HOME/releases/last.json" kind)" = rollback ] && grep -qE "\"ok\": ?true" "$STEVE_HOME/releases/last.json" && [ "$(field "$STEVE_HOME/releases/last.json" version)" = "$R1" ]' "last.json: the rollback went through"

echo "── a Rust Hub that migrates to v12, then --rollback to v11: refused"
rust_branch r12 ok 12
$REL r12 >"$T/out-r12.txt" 2>&1 || { cat "$T/out-r12.txt"; no "the v12 release failed"; }
R12=$(hubcur) P5=$(pid)
check '[ "$(schema)" = 12 ] && [ "$(hzf version)" = "$R12" ]' "$R12 live on schema v12"
if $REL --rollback "$R1" >"$T/out-rb12.txt" 2>&1; then cat "$T/out-rb12.txt"; no "rolled back a v11 Hub onto v12"; fi
check '[ "$(hubcur)" = "$R12" ] && [ "$(pid)" = "$P5" ] && grep -q "只认识到 schema v11，数据库（v12）需要至少 v12" "$T/out-rb12.txt"' "still $R12; the reason names both schemas"
check '[ "$(ls -d "$STEVE_HOME"/releases/2* | grep -c .)" -le 4 ]' "pruning keeps the newest 3 and the previous one"

echo "── history.jsonl and logs"
node -e '
const fs = require("fs"), lines = fs.readFileSync(process.argv[1], "utf8").trim().split("\n")
const keys = ["kind","version","prev","commit","branch","item","files_changed","started","finished","duration_s","ok","error","rolled_back","log"]
const hubKeys = ["hub","prev_hub","backup","schema_before","schema_after"]
for (const l of lines) { const r = JSON.parse(l); for (const k of keys) if (!(k in r)) throw new Error(`missing ${k}: ${l}`)
  if (r.kind === "hub" || r.kind === "rollback") for (const k of hubKeys) if (!(k in r)) throw new Error(`missing ${k}: ${l}`)
  if (!fs.existsSync(r.log)) throw new Error(`log missing: ${r.log}`)
  const t = fs.readFileSync(r.log, "utf8").split("\n").filter(Boolean)
  if (!t.every((x) => /^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d /.test(x))) throw new Error(`unstamped line in ${r.log}`)
  if (!t.some((x) => / -- .*: \d+s$/.test(x))) throw new Error(`no step durations in ${r.log}`) }
console.log(`  ${lines.length} lines, every key, every log present and timestamped`)' "$STEVE_HOME/releases/history.jsonl" || no "history.jsonl/logs"
ok "history.jsonl and logs"

echo "── migrate-web-split.sh on a pre-split home"
stop
M=$T/mig; mkdir -p "$M/releases/20260101-000000-abcdef12/web/dist/assets" "$M/releases/20260101-000000-abcdef12/hub"
echo '{"version":"20260101-000000-abcdef12","commit":"abcdef1234","branch":"main"}' >"$M/releases/20260101-000000-abcdef12/release.json"
printf '<!doctype html>\n<html>\n  <head>\n    <script src="/assets/index-x.js"></script>\n  </head>\n</html>\n' >"$M/releases/20260101-000000-abcdef12/web/dist/index.html"
echo 'x' >"$M/releases/20260101-000000-abcdef12/web/dist/assets/index-x.js"
ln -s "$M/releases/20260101-000000-abcdef12" "$M/current"
printf 'STEVE_PORT=1\n' >"$M/steve.env"
STEVE_HOME=$M zsh "$ROOT/ops/migrate-web-split.sh" >"$T/out-mig1.txt" 2>&1 || { cat "$T/out-mig1.txt"; no "migration failed"; }
check '[ "$(basename "$(readlink "$M/web/current")")" = 20260101-000000-abcdef12 ] && grep -q "content=\"20260101-000000-abcdef12\"" "$M/web/current/index.html" && [ -f "$M/web/current/assets/index-x.js" ]' "web/<hub version> from current/web/dist, web/current points at it"
check '[ "$(field "$M/web/current/release.json" commit)" = abcdef1234 ]' "its release.json has the hub's commit"
check '[ "$(grep -c "^STEVE_WEB_DIR=$M/web/current$" "$M/steve.env")" = 1 ] && grep -q "^STEVE_PORT=1$" "$M/steve.env"' "steve.env gets STEVE_WEB_DIR, keeps the rest"
STEVE_HOME=$M zsh "$ROOT/ops/migrate-web-split.sh" >"$T/out-mig2.txt" 2>&1 || { cat "$T/out-mig2.txt"; no "second migration failed"; }
check 'grep -q "nothing to do" "$T/out-mig2.txt" && [ "$(grep -c "^STEVE_WEB_DIR=" "$M/steve.env")" = 1 ] && [ "$(wc -l <"$M/releases/history.jsonl" | tr -d " ")" = 1 ]' "second run changes nothing"
check '[ "$(ls "$M"/logs/release/migrate-web-split-*.log | wc -l | tr -d " ")" -ge 1 ]' "migration logs written"
# the migrated home serves: run.sh-style env against a Hub
( export STEVE_HOME=$M; start_direct ); STEVE_HOME_SAVE=$STEVE_HOME; STEVE_HOME=$M; waitup
check '[ "$(hzf web_version)" = 20260101-000000-abcdef12 ] && [ "$(served "")" = 20260101-000000-abcdef12 ]' "a Hub on the migrated home serves the migrated build"
stop; STEVE_HOME=$STEVE_HOME_SAVE

echo "all $pass checks passed"
