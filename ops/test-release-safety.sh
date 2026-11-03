#!/bin/zsh
# Isolated release regressions: no network, services, credentials or production paths.
set -eu
ROOT=${0:A:h:h}
source "$ROOT/ops/release-lib.sh"
T=$(mktemp -d "${TMPDIR:-/tmp}/steve-release-safety.XXXXXX")
trap 'rm -rf -- "$T"' EXIT
fail() { print -u2 -- "FAIL: $1"; exit 1 }
pass() { print -r -- "✓ $1" }

REPO=$T/repo
mkdir -p "$REPO/web"
git -C "$REPO" init -q -b main
git -C "$REPO" config user.email test@example.invalid
git -C "$REPO" config user.name test
print base >"$REPO/web/source"
git -C "$REPO" add .
git -C "$REPO" commit -qm base
BASE=$(git -C "$REPO" rev-parse HEAD)
print changed >>"$REPO/web/source"
git -C "$REPO" commit -qam changed
[[ $(changed_since) == '?' ]] || fail "missing deployed commit"
[[ $(changed_since "") == '?' ]] || fail "empty deployed commit"
[[ $(changed_since unknown) == '?' ]] || fail "unknown deployed commit"
[[ $(changed_since "$BASE") == web/source ]] || fail "known deployed commit diff"
pass "deployed commit lookup tolerates missing, empty and unknown commits under set -u"

# Run the production web_release function, replacing only build/archive and HTTP.
# Its rollback must restore the actual symlink and verify the old page's version.
eval "$(sed -n '/^web_release() {/,/^}/p' "$ROOT/ops/release.sh")"
FIXTURE=$T/fixture
mkdir -p "$FIXTURE/web/dist/assets"
print '<html><head></head><body>new build</body></html>' >"$FIXTURE/web/dist/index.html"
print asset >"$FIXTURE/web/dist/assets/index.js"
git() { command tar -cf - -C "$FIXTURE" . }
run() { return 0 }
history() { HISTORY_ARGS=("$@") }
web_last() { LAST_ARGS=("$@") }
web_verify() {
  VERIFIED+=("$1")
  [[ $1 == old-version && $(<"$WEB/current/index.html") == *old-version* ]]
}
COMMIT=new-commit BRANCH=test WEB_FILES=(web/source) STEVE_FIRST="" WEB_KEEP=10

rollback_case() {
  local name=$1 target=$2 expected=$3
  WEB=$T/$name/web HOME_DIR=$T/$name
  VER=new-version WEB_PREV=${target:t} WEB_PREV_TARGET=$target WEB_PREV_VERSION=old-version
  WEB_OK=0 WEB_ERR="" VERIFIED=() HISTORY_ARGS=() LAST_ARGS=()
  mkdir -p "$WEB" "$expected"
  print '<html><head><meta name="steve-web-version" content="old-version"></head></html>' >"$expected/index.html"
  ln -s "$target" "$WEB/current"
  if web_release; then fail "$name accepted a failed verification"; fi
  [[ $(readlink "$WEB/current") == "$target" ]] || fail "$name changed the previous symlink target"
  [[ -f "$WEB/current/index.html" && $(<"$WEB/current/index.html") == *old-version* ]] || fail "$name lost old page"
  [[ ! -e "$WEB/$VER" ]] || fail "$name retained failed build after successful rollback"
  [[ ${(j:,:)VERIFIED} == new-version,old-version ]] || fail "$name did not verify the restored version"
  [[ $HISTORY_ARGS[4] == true && $LAST_ARGS[3] == true ]] || fail "$name did not record rollback"
  pass "$name restores original symlink, verifies old version and records rollback"
}
rollback_case legacy-absolute "$T/legacy release/web/dist" "$T/legacy release/web/dist"
rollback_case legacy-relative ../releases/old/web/dist "$T/legacy-relative/releases/old/web/dist"
rollback_case normal "$T/normal/web/old-version" "$T/normal/web/old-version"

WEB=$T/missing-previous/web HOME_DIR=$T/missing-previous
VER=new-version WEB_PREV=old-version WEB_PREV_TARGET=../releases/vanished/web/dist WEB_PREV_VERSION=old-version
WEB_OK=0 WEB_ERR="" VERIFIED=() HISTORY_ARGS=() LAST_ARGS=()
mkdir -p "$WEB"
ln -s "$WEB_PREV_TARGET" "$WEB/current"
if web_release; then fail "missing previous target accepted a failed verification"; fi
[[ $(readlink "$WEB/current") == "$WEB/$VER" && -f "$WEB/current/index.html" ]] || fail "failed restore left current dangling"
[[ -f "$WEB/$VER/index.html" ]] || fail "failed restore deleted the current build"
[[ $HISTORY_ARGS[2] == false && $HISTORY_ARGS[4] == false && $LAST_ARGS[1] == false && $LAST_ARGS[3] == false ]] || fail "failed restore falsely recorded rollback"
[[ $WEB_ERR == *恢复旧前端链接失败* && ${(j:,:)VERIFIED} == new-version ]] || fail "failed restore did not report the recovery failure"
pass "missing previous target preserves current build and records rollback=false"

WEB=$T/no-previous/web HOME_DIR=$T/no-previous
VER=new-version WEB_PREV="" WEB_PREV_TARGET="" WEB_PREV_VERSION=""
WEB_OK=0 WEB_ERR="" VERIFIED=() HISTORY_ARGS=() LAST_ARGS=()
mkdir -p "$WEB"
if web_release; then fail "no previous version accepted a failed verification"; fi
[[ ! -e "$WEB/current" && ! -L "$WEB/current" && ! -e "$WEB/$VER" ]] || fail "no previous version retained failed current or build"
[[ $HISTORY_ARGS[2] == false && $HISTORY_ARGS[4] == true && $LAST_ARGS[1] == false && $LAST_ARGS[3] == true ]] || fail "rollback to no previous version was not recorded"
[[ $HISTORY_ARGS[6] == "" && ${(j:,:)VERIFIED} == new-version ]] || fail "no previous version invented a rollback target"
pass "no previous version removes failed current/build and records rollback to empty state"
print "All release safety checks passed"
