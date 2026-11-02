# Sourced by ops/test-release.sh --real (its helpers, temp home, port and safety env are set): the TS → Rust cutover with
# the real builds. Steve's repo is a clone of this checkout: main = this repo's main (TS only), cut = this checkout's HEAD.
# Bootstrap main (TS Hub + web), release cut (npm ci, TS build, cargo build --release -p steve-hub, web build), then
# check the Rust Hub (login, API, SSE, drain refusal, data from TS), a second Rust release (incremental cargo),
# --rollback ts onto the migrated database (the TS Hub reads and writes it), and forward again.
[ -z "$(git -C "$ROOT" status --porcelain --untracked-files=no)" ] || echo "warning: $ROOT has uncommitted changes; --real releases HEAD ($(git -C "$ROOT" rev-parse --short HEAD))"
git clone -q "$ROOT" "$STEVE_REPO"
G config user.email test@example.invalid; G config user.name test
G checkout -q -B main "$(git -C "$ROOT" rev-parse main)"; G branch -q -f cut "$(git -C "$ROOT" rev-parse HEAD)"
G merge-base --is-ancestor main cut || no "HEAD does not contain main: it would not fast-forward"
G cat-file -e main:hub-rs/Cargo.toml 2>/dev/null && no "main already has hub-rs/: nothing to cut over from"
item() { # goal → id, through the API with a dev login
  local c; c=$(curl -s -o /dev/null -D - "$base/auth/dev" | tr -d '\r' | sed -n 's/^[Ss]et-[Cc]ookie: \([^;]*\).*/\1/p' | head -1)
  curl -s -H "cookie: $c" -H 'content-type: application/json' -d "{\"goal\":\"$1\",\"title\":\"$1\",\"bot\":\"dev\",\"project\":null,\"mode\":\"propose\"}" "$base/api/items" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p' | head -1
}
items() { local c; c=$(curl -s -o /dev/null -D - "$base/auth/dev" | tr -d '\r' | sed -n 's/^[Ss]et-[Cc]ookie: \([^;]*\).*/\1/p' | head -1); curl -s -H "cookie: $c" "$base/api/state"; }
sse_ok() { # /api/stream answers as an event stream
  local c h; c=$(curl -s -o /dev/null -D - "$base/auth/dev" | tr -d '\r' | sed -n 's/^[Ss]et-[Cc]ookie: \([^;]*\).*/\1/p' | head -1)
  h=$(curl -s -m 3 -D - -o /dev/null -H "cookie: $c" "$base/api/stream" | tr -d '\r' || true)
  grep -qi '^content-type: text/event-stream' <<<"$h"
}
drain_refused() { # no release running: wrong token and no token are a 404, GET and POST
  [ "$(curl -s -o /dev/null -w '%{http_code}' -X POST -H 'authorization: Bearer nope' "$base/internal/release/drain")" = 404 ] &&
  [ "$(curl -s -o /dev/null -w '%{http_code}' -H 'authorization: Bearer nope' "$base/internal/release/drain")" = 404 ] &&
  [ "$(curl -s -o /dev/null -w '%{http_code}' -X POST "$base/internal/release/drain")" = 404 ]
}

echo "── bootstrap main (TS): real npm ci and builds"
STEVE_FIRST=1 $REL main >"$T/out-first.txt" 2>&1 || { cat "$T/out-first.txt"; no "first release failed"; }
TSV=$(hubcur)
start; waitup
check 'is_ts && [ "$(hzf version)" = "$TSV" ] && api_ok && sse_ok && drain_refused' "TS Hub $TSV: login, API, SSE; drain refuses a wrong token"
I1=$(item "before the cutover")
check '[ -n "$I1" ] && [ "$(schema)" = 3 ]' "an item $I1 made on TS, schema v3"

echo "── release cut: the Rust Hub (real cargo build)"
$REL cut >"$T/out-cut.txt" 2>&1 || { cat "$T/out-cut.txt"; no "the cutover release failed"; }
RV=$(hubcur) F=$STEVE_HOME/logs/release/$RV.log
WV=$(webcur)
check '[ "$RV" != "$TSV" ] && [ "$(hzf version)" = "$RV" ] && is_rust && [ "$(hzf web_version)" = "$WV" ] && [ "$(served "")" = "$WV" ] && [ "$(served "items/$I1")" = "$WV" ]' "Rust Hub $RV live, serving web $WV (/ and the SPA fallback)"
check '[ "$(schema)" -ge 11 ] && B=$(hfield backup) && [[ $B == */pre-rust-$RV.sqlite3 ]] && [ "$(sqlite3 "$B" "SELECT v FROM schema_version")" = 3 ]' "schema v$(schema); the pre-rust backup in history is v3"
check 'api_ok && sse_ok && drain_refused && items | grep -q "\"$I1\""' "Rust: login, API, SSE, drain refusal; the TS item $I1 is there"
check 'grep -q "cargo: " "$F" && grep -q "Finished .release. profile" "$F"' "cargo output in $F ($(grep -c ' Compiling ' "$F") crates compiled)"
N1=$(grep -c ' Compiling ' "$F")

echo "── a second Rust release: incremental"
G checkout -q -b cut2 main; echo "# $RANDOM" >>"$STEVE_REPO/ops/steve.env.example"; G commit -qam cut2; G checkout -q main
$REL cut2 >"$T/out-cut2.txt" 2>&1 || { cat "$T/out-cut2.txt"; no "second Rust release failed"; }
RV2=$(hubcur) F2=$STEVE_HOME/logs/release/$RV2.log N2=$(grep -c ' Compiling ' "$F2")
check '[ "$(hzf version)" = "$RV2" ] && is_rust && [ "$N2" -lt "$N1" ] && [ "$N2" -lt 40 ]' "Rust Hub $RV2: $N2 crates compiled (first: $N1), $(sed -n 's/.*Finished .release. profile.* in \(.*\)/\1/p' "$F2")"

echo "── --rollback ts onto the migrated database"
$REL --rollback ts >"$T/out-rb.txt" 2>&1 || { cat "$T/out-rb.txt"; no "--rollback ts failed"; }
check '[ "$(hubcur)" = "$TSV" ] && is_ts && [ "$(hzf version)" = "$TSV" ] && grep -q "is newer than this Hub" "$STEVE_HOME/hub.log"' "TS Hub $TSV on schema v$(schema), with the warning"
I2=$(item "after the rollback")
check 'api_ok && sse_ok && [ -n "$I2" ] && items | grep -q "\"$I1\""' "TS: login, API, SSE, a new item $I2, the old one listed"

echo "── forward again"
$REL --hub main >"$T/out-fw.txt" 2>&1 || { cat "$T/out-fw.txt"; no "forward release failed"; }
check 'is_rust && [ "$(hzf version)" = "$(hubcur)" ] && items | grep -q "\"$I2\""' "Rust Hub $(hubcur) again; the item TS made after the rollback is there"
echo "all $pass checks passed (real)"
