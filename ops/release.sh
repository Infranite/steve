#!/bin/zsh
# Release Steve from a branch. The Hub (hub/, ops/, everything outside web/) and the web build (web/) are released on
# their own:
#   hub  build a new release dir, let the running Hub drain (no new turns; wait for running ones, bounded), back up the
#        database, switch `current`, restart, health-check; roll back to the previous release on failure. The release dir
#        always has the TS Hub (hub/dist); with hub-rs/ in the tree it also has the Rust Hub (bin/steve-hub, cargo build in
#        a target dir shared by all releases), which ops/run.sh then starts instead (STEVE_HUB_IMPL=ts: TS only)
#   web  build only web/ into $STEVE_HOME/web/<ver>, switch the `web/current` link the Hub serves from (no drain, no
#        restart), check that / serves the new index.html and one of its hashed assets; switch back on failure
# What runs is decided from what changed since the deployed commits (current/release.json, web/current/release.json):
# only web/ → web; anything else → hub, and web too when web/ (or the root package files) changed. Hub first, then web.
# On a failure main is reset to where it was, so main stays what is deployed.
# Writes $STEVE_HOME/releases/last.json for the Hub, web/last.json, releases/history.jsonl, logs/release/<ver>.log.
# Usage: release.sh [--web|--hub] <branch|main> [item-id]          (ops/release-web.sh = release.sh --web)
#        release.sh --rollback [<version>|ts]   switch `current` back to an installed release (default: the one before
#        current; ts: the newest TS-only one), restart, health-check; main and the web build are left alone
# Env: STEVE_HOME, STEVE_REPO, STEVE_PORT; STEVE_DRAIN_TOKEN (given by the Hub), STEVE_DRAIN_S (default 120: the Hub resumes turns a restart cut off, once within 30 min);
#      STEVE_LABEL (launchd job) or STEVE_RESTART_CMD (restarts the Hub instead of launchctl, for tests);
#      STEVE_WEB_KEEP (web builds kept besides the previous one, default 10); STEVE_FIRST=1 (bootstrap: nothing runs yet);
#      STEVE_HUB_IMPL=ts (env or steve.env: don't build the Rust Hub); STEVE_CARGO (cargo to use), STEVE_CARGO_TARGET_DIR
#      (default $STEVE_HOME/build/cargo-target); STEVE_HEALTH_TRIES (default 40, 1.5 s apart)
set -u
SELF=${0:A} OPS=${0:A:h}
source $OPS/release-lib.sh
zmodload zsh/system

MODE=auto; ARGS=()
for a in "$@"; do
  case $a in
    --web) MODE=web ;;
    --hub) MODE=hub ;;
    --rollback) MODE=rollback ;;
    -h|--help) sed -n '2,23p' $SELF; exit 0 ;;
    *) ARGS+=$a ;;
  esac
done
BRANCH=${ARGS[1]:-main} ITEM=${ARGS[2]:-} HOME_DIR=${STEVE_HOME:-$HOME/.steve} REPO=${STEVE_REPO:-${OPS:h}}
LABEL=${STEVE_LABEL:-io.github.infranite.steve} PORT=${STEVE_PORT:-8440} DRAIN_S=${STEVE_DRAIN_S:-120} WEB_KEEP=${STEVE_WEB_KEEP:-10}
REL=$HOME_DIR/releases WEB=$HOME_DIR/web LOGS=$HOME_DIR/logs/release HIST=$HOME_DIR/releases/history.jsonl
CARGO_TARGET=${STEVE_CARGO_TARGET_DIR:-$HOME_DIR/build/cargo-target} HEALTH_TRIES=${STEVE_HEALTH_TRIES:-40} DB=$HOME_DIR/data/steve.sqlite3
mkdir -p $REL $WEB $LOGS $HOME_DIR/backups
WEB_PREV_TARGET=$(readlink $WEB/current 2>/dev/null)
PREV=$(basename "$(readlink $HOME_DIR/current 2>/dev/null)" 2>/dev/null) WEB_PREV=$(basename "$WEB_PREV_TARGET" 2>/dev/null)
[[ $PREV == . ]] && PREV=""; [[ $WEB_PREV == . ]] && WEB_PREV=""
# Legacy web/current may point into a Hub release's web/dist without release.json.
# Keep prev metadata compatible, but verify the restored page using its actual version tag.
WEB_PREV_VERSION=$(json_field $WEB/current/release.json version)
[[ -n $WEB_PREV_VERSION ]] || WEB_PREV_VERSION=$(sed -n 's/.*name="steve-web-version" content="\([^"]*\)".*/\1/p' $WEB/current/index.html 2>/dev/null | head -1)
LOG=$LOGS/$(strftime '%Y%m%d-%H%M%S' $EPOCHSECONDS)-run-$$.log
VER="" SHA="" COMMIT="" BASE="" KIND="" DO_HUB=0 DO_WEB=0 HUB_OK=0 WEB_OK=0 HUB_FILES=() WEB_FILES=()
# the hub lines of history.jsonl also say which Hub ran before and after, the database backup and the schema around it
HUB_IMPL="" PREV_IMPL="" BACKUP="" SCHEMA_BEFORE="" SCHEMA_AFTER=""

# ── results: last.json (what the Hub reports to the item), web/last.json, history.jsonl ──
last() { # ok error [pending]
  print -r -- "{\"ok\":$1,\"pending\":${3:-false},\"kind\":$(js ${KIND:-hub}),\"version\":$(js "$VER"),\"prev\":$(js "$([[ $KIND == web ]] && print -r -- $WEB_PREV || print -r -- $PREV)"),\"web_prev\":$(js "$WEB_PREV"),\"hub_ok\":$(jbool $HUB_OK),\"web_ok\":$(jbool $WEB_OK),\"item\":$(js "$ITEM"),\"branch\":$(js "$BRANCH"),\"commit\":$(js "$COMMIT"),\"error\":$(js "$2"),\"log\":$(js "$LOG"),\"at\":$EPOCHSECONDS}" >$REL/last.json
}
history() { # kind ok error rolled_back started prev files...
  local kind=$1 ok=$2 err=$3 rb=$4 t=$5 prev=$6 hx=""; shift 6
  [[ $kind == hub || $kind == rollback ]] && hx=",\"hub\":$(js "$HUB_IMPL"),\"prev_hub\":$(js "$PREV_IMPL"),\"backup\":$(js "$BACKUP"),\"schema_before\":$(js "$SCHEMA_BEFORE"),\"schema_after\":$(js "$SCHEMA_AFTER")"
  print -r -- "{\"kind\":$(js "$kind"),\"version\":$(js "$VER"),\"prev\":$(js "$prev"),\"commit\":$(js "$COMMIT"),\"branch\":$(js "$BRANCH"),\"item\":$(js "$ITEM"),\"files_changed\":$(jarr "$@"),\"started\":$(js "$(iso $t)"),\"finished\":$(js "$(iso)"),\"duration_s\":$(( EPOCHSECONDS - t )),\"ok\":$ok,\"error\":$(js "$err"),\"rolled_back\":$rb,\"log\":$(js "$LOG")$hx}" >>$HIST
  print -r -- "   history: $kind $VER ok=$ok${err:+ error=$err} rolled_back=$rb${hx:+ hub=$PREV_IMPL->$HUB_IMPL backup=${BACKUP:-none} schema=${SCHEMA_BEFORE:-?}->${SCHEMA_AFTER:-?}}"
}
web_last() { # ok error rolled_back
  print -r -- "{\"ok\":$1,\"version\":$(js "$VER"),\"prev\":$(js "$WEB_PREV"),\"commit\":$(js "$COMMIT"),\"branch\":$(js "$BRANCH"),\"item\":$(js "$ITEM"),\"error\":$(js "$2"),\"rolled_back\":$3,\"log\":$(js "$LOG"),\"at\":$EPOCHSECONDS}" >$WEB/last.json
}
reset_main() {
  [[ -n $BASE && $(git -C $REPO rev-parse HEAD 2>/dev/null) != $BASE ]] || return 0
  print -r -- "== reset main to $BASE (was $(git -C $REPO rev-parse HEAD))"; git -C $REPO reset -q --hard $BASE
}
# a failure before anything was switched (or before we know what to release)
die() { # message
  step ""
  print -r -- "✗ $1"
  [[ -z $KIND ]] && history preflight false "$1" false $T0 "$PREV"
  reset_main; last false "$1"
  finish 1
}
finish() {
  step ""
  print -r -- "== done in $(( EPOCHSECONDS - T0 ))s, exit $1; log $LOG"
  exit $1
}
# the log is named after the version once there is one
name_log() { local n=$LOGS/$1.log; [[ $LOG == $n ]] && return; mv $LOG $n && LOG=$n && print -r -- "   log: $LOG" }

# ── hub ──
restart() { if [[ -n ${STEVE_RESTART_CMD:-} ]]; then run zsh -c $STEVE_RESTART_CMD; else run launchctl kickstart -k gui/$(id -u)/$LABEL; fi }
switch_to() { ln -sfn $REL/$1 $HOME_DIR/current; print -r -- "   $HOME_DIR/current -> $(readlink $HOME_DIR/current)"; restart }
# ask the running Hub to start no new turns (new prompts queue and survive the restart) and wait until the running turns end
drain() {
  [[ -n ${STEVE_DRAIN_TOKEN:-} ]] || { print "   no drain token (not started by the Hub), restarting without waiting"; return 0 }
  local url=http://127.0.0.1:$PORT/internal/release/drain auth="authorization: Bearer $STEVE_DRAIN_TOKEN" t0=$SECONDS r last=""
  r=$(curl -sf -m 5 -X POST -H $auth $url) || { print "   the Hub did not take the drain request, restarting without waiting"; return 0 }
  print -r -- "   drain started: $r"
  while (( SECONDS - t0 < DRAIN_S )); do
    r=$(curl -s -m 5 -H $auth $url)
    [[ $r == *'"idle":true'* ]] && { print "   drained after $(( SECONDS - t0 ))s"; return 0 }
    [[ $r != $last ]] && print -r -- "   waiting: $r"; last=$r
    sleep 2
  done
  print -r -- "   still running after ${DRAIN_S}s ($r), restarting anyway"
}
healthy() { # version
  local i v r
  for i in {1..$HEALTH_TRIES}; do
    sleep 1.5
    r=$(curl -s -m 2 http://127.0.0.1:$PORT/healthz); v=$(print -r -- $r | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')
    [[ $v == $1 ]] && { print -r -- "   healthy after try $i: $r"; return 0 }
    (( i % 5 == 0 )) && print -r -- "   try $i: ${r:-no answer}"
  done
  print -r -- "   not healthy as $1 after $HEALTH_TRIES tries (last answer: ${r:-none})"; return 1
}
hub_fail() { # message — before the switch: nothing changed for the running Hub
  print -r -- "✗ $1"; history hub false "$1" false $T_HUB "$PREV" $HUB_FILES; rm -rf -- $REL/$VER; die "$1"
}
# which Hub a release dir runs (what ops/run.sh starts): rust when it has bin/steve-hub, else ts
impl_of() { # dir
  local h; h=$(json_field $1/release.json hub)
  [[ -n $h ]] && { print -r -- $h; return }
  [[ -x $1/bin/steve-hub ]] && print rust || print ts
}
# STEVE_HUB_IMPL from the environment (the Hub passes its own, read from steve.env by run.sh) or steve.env (a manual run)
impl_wanted() {
  local v=${STEVE_HUB_IMPL:-$(sed -n 's/^STEVE_HUB_IMPL=//p' $HOME_DIR/steve.env 2>/dev/null | tail -1)}
  print -r -- ${${v//\"/}//\'/}
}
schema() { [[ -f $DB ]] && sqlite3 $DB "SELECT v FROM schema_version" 2>/dev/null | head -1 }

# ── the Rust Hub ──
# cargo: launchd's PATH is short and Homebrew's links have vanished for minutes at a time, so look in every usual
# place and take the first that runs; a cargo straight from a toolchain dir gets that toolchain's rustc
find_cargo() {
  local c v cands=()
  [[ -n ${STEVE_CARGO:-} ]] && cands+=$STEVE_CARGO
  c=$(whence -p cargo 2>/dev/null) && cands+=$c
  cands+=($HOME/.cargo/bin/cargo /opt/homebrew/opt/rustup/bin/cargo /opt/homebrew/bin/cargo /usr/local/bin/cargo
    $HOME/.rustup/toolchains/stable-*/bin/cargo(N) $HOME/.rustup/toolchains/*/bin/cargo(N))
  for c in $cands; do
    [[ -x $c ]] || { [[ $c == ${STEVE_CARGO:-} ]] && print -r -- "   STEVE_CARGO=$c is not executable"; continue }
    if v=$($c --version 2>&1); then
      CARGO=$c
      # straight from a toolchain dir (not a rustup proxy): use the rustc next to it, not whatever is on PATH
      if [[ ${c:A} == */toolchains/*/bin/cargo && -x ${c:A:h}/rustc ]]; then export RUSTC=${c:A:h}/rustc PATH=${c:A:h}:$PATH; fi
      print -r -- "   cargo: $c -> ${c:A} ($v)${RUSTC:+, rustc $RUSTC ($($RUSTC --version 2>&1))}"
      return 0
    fi
    print -r -- "   $c does not run: $v"
  done
  print -r -- "   no cargo that runs among: $cands"; return 1
}
# Sign the Hub with the stable local identity (ops/sign-setup.sh), so the vault's keychain entry, whose access
# control is "identifier io.github.infranite.steve.hub and this certificate", keeps trusting every release.
# Without the signing keychain (test homes) the binary stays ad-hoc signed and a warning is logged.
SIGN_ID=${STEVE_SIGN_IDENTITY:-Steve Hub} SIGN_IDENT=${STEVE_KEYHELPER_HUB_IDENTIFIER:-io.github.infranite.steve.hub}
export STEVE_KEYHELPER_HUB_IDENTIFIER=$SIGN_IDENT
export STEVE_KEYHELPER_IDENTIFIER=${STEVE_KEYHELPER_IDENTIFIER:-io.github.infranite.steve.keyhelper}
export STEVE_KEYHELPER_SERVICE=${STEVE_KEYHELPER_SERVICE:-io.github.infranite.steve.vault}
SIGN_KC=${STEVE_SIGN_KEYCHAIN:-$HOME_DIR/signing/steve-signing.keychain-db}
sign_hub() {
  local bin=$1 req
  # code signing and the keychain ACL are macOS only; other systems keep the vault key elsewhere (accounts-vault.md §3.3)
  if [[ $(uname -s) != Darwin ]]; then
    print -r -- "   not macOS ($(uname -s)): skipping code signing"
    return 0
  fi
  if [[ ! -f $SIGN_KC || ! -f ${SIGN_KC:h}/keychain-pass ]]; then
    print -r -- "   WARN: no signing keychain at $SIGN_KC: steve-hub stays ad-hoc signed and the vault keychain entry will not trust it (run ops/sign-setup.sh)"
    return 0
  fi
  security unlock-keychain -p "$(<${SIGN_KC:h}/keychain-pass)" $SIGN_KC || { print -r -- "   cannot unlock $SIGN_KC"; return 1 }
  run codesign -f -s "$SIGN_ID" --keychain $SIGN_KC -i $SIGN_IDENT $bin || { print -r -- "   codesign failed"; return 1 }
  req=$(codesign -d -r- $bin 2>&1 | grep '^designated')
  [[ $req == *"identifier \"$SIGN_IDENT\""*"certificate leaf"* ]] || { print -r -- "   unexpected signature: $req"; return 1 }
  print -r -- "   signed: $req"
}
# steve-keyhelper (ops/KEYHELPER.md): the only process that touches the vault keys in the macOS keychain. It lives
# outside the releases ($HOME_DIR/bin) and is rebuilt only when its source changes, so its code signature (and the
# keychain partition of the items it created) stays the same across Hub releases. An upgrade migrates the items
# from the old helper to the new one before the new one takes its place; a failed migration fails the release.
keyhelper_install() { # release dir
  local dir=$1 bin=$HOME_DIR/bin/steve-keyhelper src want have
  [[ $(uname -s) == Darwin ]] || return 0
  # A neutral source default must not silently replace an existing distribution identity.
  if [[ -x $bin ]]; then
    local installed_ident=$(codesign -dv $bin 2>&1 | sed -n 's/^Identifier=//p')
    [[ $installed_ident == $STEVE_KEYHELPER_IDENTIFIER ]] || {
      print "   existing helper identity differs; set STEVE_KEYHELPER_SERVICE, STEVE_KEYHELPER_HUB_IDENTIFIER and STEVE_KEYHELPER_IDENTIFIER to the original build values before upgrading"
      return 1
    }
  fi
  src=$dir/hub-rs/crates/steve-keyhelper
  [[ -d $src ]] || return 0
  if [[ ! -f $SIGN_KC || ! -f ${SIGN_KC:h}/keychain-pass ]]; then
    print -r -- "   WARN: no signing keychain: steve-keyhelper not installed (run ops/sign-setup.sh)"; return 0
  fi
  want=$( (cd $src && find Cargo.toml src -type f | LC_ALL=C sort | xargs shasum -a 256) | shasum -a 256 | cut -d' ' -f1)
  want=$(print -r -- "$want|$STEVE_KEYHELPER_SERVICE|$STEVE_KEYHELPER_HUB_IDENTIFIER|$STEVE_KEYHELPER_IDENTIFIER" | shasum -a 256 | cut -d' ' -f1)
  have=$([[ -f $bin.srchash ]] && <$bin.srchash)
  if [[ -x $bin && $want == $have ]]; then print -r -- "   steve-keyhelper unchanged ($want)"; return 0; fi
  ( cd $dir/hub-rs && export CARGO_TERM_COLOR=never CARGO_TERM_PROGRESS_WHEN=never && run $CARGO build --release --locked -p steve-keyhelper --target-dir $CARGO_TARGET ) || return 1
  mkdir -p $HOME_DIR/bin && cp -p $CARGO_TARGET/release/steve-keyhelper $bin.new || return 1
  security unlock-keychain -p "$(<${SIGN_KC:h}/keychain-pass)" $SIGN_KC || return 1
  run codesign -f -s "$SIGN_ID" --keychain $SIGN_KC -i $STEVE_KEYHELPER_IDENTIFIER $bin.new || return 1
  if [[ -x $bin ]]; then
    print -r -- "   migrating the vault keys from the installed steve-keyhelper"
    $bin.new migrate-from $bin || { print -r -- "   migration failed: keeping the installed helper"; rm -f $bin.new; return 1 }
  fi
  mv -f $bin.new $bin && print -r -- $want >$bin.srchash || return 1
  print -r -- "   installed $bin ($(codesign -dvvv $bin 2>&1 | grep '^CDHash=' ))"
}
rust_build() { # release dir → <dir>/bin/steve-hub
  local dir=$1 bin=$CARGO_TARGET/release/steve-hub
  find_cargo || return 1
  mkdir -p $CARGO_TARGET; print -r -- "   target dir $CARGO_TARGET (shared by releases: only what changed is rebuilt)"
  ( cd $dir/hub-rs && export CARGO_TERM_COLOR=never CARGO_TERM_PROGRESS_WHEN=never && run $CARGO build --release --locked -p steve-hub --target-dir $CARGO_TARGET ) || return 1
  [[ -x $bin ]] || { print -r -- "   $bin is missing after the build"; return 1 }
  mkdir -p $dir/bin && cp -p $bin $dir/bin/steve-hub.tmp && mv -f $dir/bin/steve-hub.tmp $dir/bin/steve-hub || return 1
  sign_hub $dir/bin/steve-hub || return 1
  RUST_SHA=$(shasum -a 256 $dir/bin/steve-hub | cut -d' ' -f1)
  print -r -- "   installed $dir/bin/steve-hub ($(du -h $dir/bin/steve-hub | cut -f1), sha256 $RUST_SHA)"
}

hub_release() {
  local DIR=$REL/$VER want; T_HUB=$EPOCHSECONDS RUST_SHA=""
  PREV_IMPL=$([[ -n $PREV && -d $REL/$PREV ]] && impl_of $REL/$PREV)
  step "hub: export $COMMIT to $DIR"
  mkdir -p $DIR && git -C $REPO archive HEAD | tar -x -C $DIR || hub_fail "导出代码失败"
  cd $DIR
  step "hub: npm ci"
  run npm ci --no-audit --no-fund || hub_fail "安装依赖失败，见 $LOG"
  # the TS Hub is built in every release: rollback target, and what STEVE_HUB_IMPL=ts runs from a Rust release
  step "hub: build"
  run npm run build:hub || hub_fail "Hub 构建失败，见 $LOG"
  HUB_IMPL=ts want=$(impl_wanted)
  if [[ -f $DIR/hub-rs/Cargo.toml && $want != ts ]]; then
    step "hub: cargo build (Rust Hub)"
    HUB_IMPL=rust
    rust_build $DIR || hub_fail "Rust Hub 构建失败，见 $LOG"
    step "hub: steve-keyhelper"
    keyhelper_install $DIR || hub_fail "steve-keyhelper 安装或密钥迁移失败，见 $LOG"
  elif [[ -f $DIR/hub-rs/Cargo.toml ]]; then
    print "   STEVE_HUB_IMPL=ts: not building the Rust Hub"
  fi
  print -r -- "   hub: ${PREV_IMPL:-none} -> $HUB_IMPL"
  print -r -- "{\"version\":$(js "$VER"),\"commit\":$(js "$COMMIT"),\"branch\":$(js "$BRANCH"),\"built_at\":$(js "$(iso)"),\"hub\":$(js $HUB_IMPL)${RUST_SHA:+,\"hub_sha256\":$(js $RUST_SHA)}}" >$DIR/release.json
  if [[ -n ${STEVE_FIRST:-} ]]; then
    step "hub: first release, nothing to restart"; ln -sfn $DIR $HOME_DIR/current; HUB_OK=1; history hub true "" false $T_HUB "" $HUB_FILES; return 0
  fi
  step "hub: drain"
  drain
  step "hub: backup the database"
  SCHEMA_BEFORE=$(schema)
  if [[ -f $DB ]]; then
    # the first switch to the Rust Hub migrates the schema forward (3 → 11+): its backup is kept apart from the rotation
    if [[ $HUB_IMPL == rust && $PREV_IMPL != rust ]]; then BACKUP=$HOME_DIR/backups/pre-rust-$VER.sqlite3; else BACKUP=$HOME_DIR/backups/pre-$VER.sqlite3; fi
    print -r -- "   schema v${SCHEMA_BEFORE:-?}, backup to $BACKUP"
    if run sqlite3 $DB ".backup '$BACKUP'" && [[ $(sqlite3 $BACKUP "PRAGMA quick_check" 2>&1) == ok ]]; then
      print -r -- "   backup ok ($(du -h $BACKUP | cut -f1))"
    else
      [[ $BACKUP == */pre-rust-* ]] && hub_fail "切换到 Rust Hub 前数据库备份失败，没有切换"
      print "   backup failed, releasing anyway"; BACKUP=""
    fi
  else print "   no database yet"; fi
  # keep the newest 10 pre-release backups (pre-rust-* stay)
  for old in $HOME_DIR/backups/pre-2*.sqlite3(N.om[11,-1]); do print -r -- "   remove old backup $old"; rm -f -- $old; done
  step "hub: switch to $VER and restart"
  last false "发布进行中" true
  switch_to $VER
  step "hub: health check"
  if healthy $VER; then
    SCHEMA_AFTER=$(schema)
    HUB_OK=1; print -r -- "✓ hub $VER is live ($HUB_IMPL, schema v${SCHEMA_BEFORE:-?} -> v${SCHEMA_AFTER:-?})"; history hub true "" false $T_HUB "$PREV" $HUB_FILES
    step "hub: prune old releases"
    # keep the newest 3, the previous one and the newest TS-only one (the rollback target from a Rust Hub)
    local ts_keep="" old
    for old in $REL/2*(N/On); do [[ $(impl_of $old) == ts ]] && { ts_keep=${old:t}; break }; done
    for old in $REL/2*(N/om[4,-1]); do [[ ${old:t} != $PREV && ${old:t} != $ts_keep ]] && { print -r -- "   remove $old"; rm -rf -- $old }; done
    return 0
  fi
  SCHEMA_AFTER=$(schema)
  local err="新版本启动后健康检查没有通过"
  step "hub: roll back to ${PREV:-nothing}"
  if [[ -n $PREV ]]; then
    switch_to $PREV
    healthy $PREV || err="新版本健康检查没有通过，回滚后旧版本也没有起来"
  fi
  [[ $SCHEMA_AFTER != $SCHEMA_BEFORE ]] && print -r -- "   the database stays at schema v$SCHEMA_AFTER (was v$SCHEMA_BEFORE; backup $BACKUP)"
  rm -rf -- $DIR
  history hub false "$err" true $T_HUB "$PREV" $HUB_FILES
  reset_main; last false "$err"
  finish $([[ $err == *旧版本也没有起来* ]] && print 2 || print 1)
}

# ── rollback: back to an installed release by hand (no build, no git, web untouched) ──
rollback_main() { # [version|ts]
  local target=${1:-} cur=$PREV d err="" ok=true
  T0=$EPOCHSECONDS T_HUB=$EPOCHSECONDS KIND=rollback
  step "rollback ${target:-to the previous release}"
  print -r -- "   home=$HOME_DIR port=$PORT current=${cur:-none}; restart: ${STEVE_RESTART_CMD:-launchctl kickstart -k gui/$(id -u)/$LABEL}"
  touch $REL/.release.lock; zsystem flock -t 0 $REL/.release.lock 2>/dev/null || { print "✗ 另一个发布正在进行"; finish 1 }
  print "   installed:"; for d in $REL/2*(N/On); do print -r -- "     ${d:t} ($(impl_of $d))$([[ ${d:t} == $cur ]] && print ' <- current')"; done
  if [[ $target == ts ]]; then
    target=""; for d in $REL/2*(N/On); do [[ ${d:t} != $cur && $(impl_of $d) == ts ]] && { target=${d:t}; break }; done
  elif [[ -z $target ]]; then
    for d in $REL/2*(N/On); do [[ ${d:t} < $cur ]] && { target=${d:t}; break }; done
  fi
  [[ -n $target && -f $REL/$target/release.json ]] || { print -r -- "✗ no release to roll back to (${target:-none found})"; finish 1 }
  [[ $target != $cur ]] || { print -r -- "✗ $target is already current"; finish 1 }
  VER=$target COMMIT=$(json_field $REL/$target/release.json commit) BRANCH=$(json_field $REL/$target/release.json branch)
  HUB_IMPL=$(impl_of $REL/$target) PREV_IMPL=$([[ -n $cur ]] && impl_of $REL/$cur)
  name_log rollback-$target-$(strftime '%H%M%S' $EPOCHSECONDS)
  print -r -- "   ${cur:-none} ($PREV_IMPL) -> $target ($HUB_IMPL)"
  step "rollback: drain"; drain
  step "rollback: backup the database"
  SCHEMA_BEFORE=$(schema)
  if [[ -f $DB ]]; then
    BACKUP=$HOME_DIR/backups/rollback-$(strftime '%Y%m%d-%H%M%S' $EPOCHSECONDS).sqlite3
    run sqlite3 $DB ".backup '$BACKUP'" || { print "   backup failed, rolling back anyway"; BACKUP="" }
  fi
  step "rollback: switch to $target and restart"
  switch_to $target
  step "rollback: health check"
  if healthy $target; then
    print -r -- "✓ rolled back to $target ($HUB_IMPL)"
    [[ $HUB_IMPL == ts && $PREV_IMPL == rust ]] && print "   note: main still has hub-rs/, so the next release builds the Rust Hub again unless STEVE_HUB_IMPL=ts is in steve.env"
  else
    ok=false err="回滚到 $target 后健康检查没有通过"
    if [[ -n $cur && -d $REL/$cur ]]; then
      step "rollback: back to $cur"; switch_to $cur; healthy $cur || err="回滚到 $target 和切回 $cur 后健康检查都没有通过"
    fi
  fi
  SCHEMA_AFTER=$(schema)
  history rollback $ok "$err" false $T_HUB "$cur"
  finish $([[ $ok == true ]] && print 0 || print 1)
}

# ── web ──
# / must be the new index.html (its version tag) and one of its hashed assets must load
web_verify() { # version
  local base=http://127.0.0.1:$PORT body="" asset code hdr i
  for i in {1..10}; do
    body=$(curl -s -m 5 $base/)
    [[ $body == *"name=\"steve-web-version\" content=\"$1\""* ]] && break
    sleep 1
  done
  [[ $body == *"name=\"steve-web-version\" content=\"$1\""* ]] || {
    print -r -- "   GET / does not serve $1 (it has: $(print -r -- $body | grep -o 'steve-web-version" content="[^"]*' | head -1 || true)${body:+, ${#body} bytes}${body:-no answer})"; return 1 }
  hdr=$(curl -s -m 5 -o /dev/null -D - $base/ | tr -d '\r' | grep -i '^cache-control:' || true)
  print -r -- "   GET / serves $1 (${#body} bytes, ${hdr:-no cache-control header})"
  [[ $hdr == *no-store* ]] || print "   warning: / is not served with cache-control: no-store"
  asset=$(print -r -- $body | grep -oE '/assets/[^"'"'"' >]+' | head -1)
  [[ -n $asset ]] || { print "   index.html names no /assets/ file"; return 1 }
  code=$(curl -s -m 10 -o /dev/null -w '%{http_code} %{content_type}' $base$asset)
  print -r -- "   GET $asset: $code"
  [[ $code == 200* && $code != *text/html* ]] || return 1
  print -r -- "   /healthz: $(curl -s -m 5 $base/healthz)"
}
web_release() {
  local B=$WEB/.build-$VER D=$WEB/$VER err="" rb=false; T_WEB=$EPOCHSECONDS
  step "web: export $COMMIT to $B"
  rm -rf -- $B; mkdir -p $B && git -C $REPO archive HEAD | tar -x -C $B || err="导出代码失败"
  if [[ -z $err ]]; then
    cd $B
    step "web: npm ci (web workspace only)"
    run npm ci -w web --include-workspace-root --no-audit --no-fund || err="前端安装依赖失败，见 $LOG"
  fi
  if [[ -z $err ]]; then
    step "web: build"
    run npm run build:web || err="前端构建失败，见 $LOG"
  fi
  if [[ -z $err ]]; then
    step "web: install as $D"
    web_install $B/web/dist $D "$VER" "$COMMIT" "$BRANCH" || err="前端构建结果不完整，见 $LOG"
  fi
  cd $HOME_DIR; rm -rf -- $B
  if [[ -n $err ]]; then # nothing was switched: the previous build keeps serving
    print -r -- "✗ $err; still serving ${WEB_PREV:-nothing}"; rm -rf -- $D $D.tmp
    history web false "$err" false $T_WEB "$WEB_PREV" $WEB_FILES; web_last false "$err" false; WEB_ERR=$err; return 1
  fi
  step "web: switch to $VER"
  web_switch $WEB $VER
  if [[ -n ${STEVE_FIRST:-} ]]; then
    print "   first release: no Hub running to check against"
  else
    step "web: check what the Hub serves"
    if ! web_verify $VER; then
      err="新前端切换后检查没有通过"; rb=true
      step "web: roll back to ${WEB_PREV:-nothing}"
      if web_restore "$WEB" "$WEB_PREV_TARGET"; then
        web_verify "${WEB_PREV_VERSION:-$WEB_PREV}" || { err="新前端检查没有通过，切回 $WEB_PREV 后检查也没有通过"; print "   the previous build does not check out either" }
      elif [[ -n $WEB_PREV_TARGET ]]; then
        err="新前端检查没有通过，恢复旧前端链接失败"
        print -r -- "   could not restore $WEB/current -> $WEB_PREV_TARGET"
        # Keep the failed build while current still points at it.
        history web false "$err" false $T_WEB "$WEB_PREV" $WEB_FILES; web_last false "$err" false; WEB_ERR=$err; return 1
      else
        rm -f -- $WEB/current; print "   no previous build: web/current removed"
      fi
      rm -rf -- $D
      print -r -- "✗ $err"
      history web false "$err" $rb $T_WEB "$WEB_PREV" $WEB_FILES; web_last false "$err" $rb; WEB_ERR=$err; return 1
    fi
  fi
  WEB_OK=1; print -r -- "✓ web $VER is live (no restart)"
  history web true "" false $T_WEB "$WEB_PREV" $WEB_FILES; web_last true "" false
  step "web: prune old builds (keep $WEB_KEEP and $WEB_PREV)"
  local n=0 d
  for d in $WEB/2*(N/om); do
    n=$(( n + 1 )); [[ ${d:t} == $VER || ${d:t} == $WEB_PREV ]] && continue
    (( n > WEB_KEEP )) && { print -r -- "   remove $d"; rm -rf -- $d }
  done
  return 0
}

# web/ and the root package files count for the web build
is_web() { [[ $1 == web/* || $1 == package.json || $1 == package-lock.json ]] }
is_hub() { [[ $1 != web/* ]] }

main() {
  [[ $MODE == rollback ]] && rollback_main $ARGS
  T0=$EPOCHSECONDS
  step "release ${(j: :)argv}"
  print -r -- "   script $SELF, pid $$, user $(id -un), host $(hostname -s)"
  print -r -- "   branch=$BRANCH item=${ITEM:--} mode=$MODE home=$HOME_DIR repo=$REPO port=$PORT"
  print -r -- "   hub now: ${PREV:-none}; web now: ${WEB_PREV:-none}"
  print -r -- "   drain token: $( [[ -n ${STEVE_DRAIN_TOKEN:-} ]] && print set || print unset ); restart: ${STEVE_RESTART_CMD:-launchctl kickstart -k gui/$(id -u)/$LABEL}${STEVE_FIRST:+; first release}"
  print -r -- "   node $(node --version 2>&1), npm $(npm --version 2>&1), $(git --version)"
  print -r -- "   log $LOG"
  touch $REL/.release.lock; zsystem flock -t 0 $REL/.release.lock 2>/dev/null || die "另一个发布正在进行（$REL/.release.lock 被占用）"

  step "preflight: main in $REPO"
  cd $REPO 2>/dev/null || die "仓库不存在：$REPO"
  [[ -z $(git status --porcelain --untracked-files=no) ]] || { git status --short --untracked-files=no; die "main 的工作区有未提交的改动" }
  run git checkout -q main || die "切不到 main"
  BASE=$(git rev-parse HEAD); print -r -- "   main is at $BASE"
  if [[ $BRANCH != main ]]; then
    run git merge --ff-only $BRANCH || die "$BRANCH 不能快进合并到 main，需要先 rebase"
  fi
  COMMIT=$(git rev-parse HEAD) SHA=$(git rev-parse --short=8 HEAD) VER=$(strftime '%Y%m%d-%H%M%S' $EPOCHSECONDS)-$SHA
  name_log $VER
  print -r -- "   releasing $COMMIT as $VER"
  [[ $BASE != $COMMIT ]] && { print "   new on main:"; git log --format='     %h %s' $BASE..HEAD }

  step "decide what to release"
  local hub_at web_at all
  hub_at=$(json_field $HOME_DIR/current/release.json commit) web_at=$(json_field $WEB/current/release.json commit)
  print -r -- "   hub deployed from ${hub_at:-unknown}, web deployed from ${web_at:-unknown}"
  all=$(changed_since "$hub_at"); [[ $all == "?" ]] && { print "   hub commit unknown here: comparing with main before the merge"; all=$(git diff --name-only $BASE HEAD) }
  for f in ${(f)all}; do is_hub $f && HUB_FILES+=$f; done
  all=$(changed_since "$web_at"); [[ $all == "?" ]] && { print "   web commit unknown here: comparing with main before the merge"; all=$(git diff --name-only $BASE HEAD) }
  for f in ${(f)all}; do is_web $f && WEB_FILES+=$f; done
  print -r -- "   hub files changed (${#HUB_FILES}):"; for f in $HUB_FILES; do print -r -- "     $f"; done
  print -r -- "   web files changed (${#WEB_FILES}):"; for f in $WEB_FILES; do print -r -- "     $f"; done
  case $MODE in
    web) DO_WEB=1; print "   --web: web only" ;;
    hub) DO_HUB=1; print "   --hub: hub only" ;;
    *)
      if (( ${#HUB_FILES} )); then DO_HUB=1; print "   hub files changed: hub release"
      elif (( ! ${#WEB_FILES} )); then DO_HUB=1; print "   nothing changed since the deployed commits: hub release as before"
      fi
      if (( ${#WEB_FILES} )); then DO_WEB=1; print "   web files changed: web release$( (( DO_HUB )) && print ' after the hub')"
      elif [[ ! -e $WEB/current ]]; then DO_WEB=1; print "   no web build installed yet: web release"
      fi ;;
  esac
  (( DO_HUB && DO_WEB )) && KIND=hub+web || { (( DO_HUB )) && KIND=hub || KIND=web }
  print -r -- "   => $KIND"

  (( DO_HUB )) && hub_release
  WEB_ERR=""
  if (( DO_WEB )); then
    if ! web_release; then
      if (( DO_HUB )); then
        # the new Hub is live and stays: main keeps the commit, the web is retried with --web
        last false "Hub 已发布 $VER，前端发布失败：$WEB_ERR；前端仍是 ${WEB_PREV:-旧版本}，可以用 release.sh --web main 重试"
      else
        reset_main; last false "$WEB_ERR"
      fi
      finish 1
    fi
  fi
  last true ""
  print -r -- "✓ released $KIND $VER"
  finish 0
}

# fd 3 is the log, opened before main can rename it (the open file follows the rename); main itself doesn't get it
exec 3>>$LOG
main "$@" 3>&- 2>&1 | stamp
exit ${pipestatus[1]}
