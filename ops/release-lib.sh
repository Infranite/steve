#!/bin/zsh
# Shared by ops/release.sh (sourced, not run): log helpers, JSON helpers and the web build layout.
#   $STEVE_HOME/web/<ver>/       one web build: the web/dist contents plus release.json {version, commit, branch, built_at}
#   $STEVE_HOME/web/current      symlink to the build the Hub serves (STEVE_WEB_DIR); switched by an atomic rename, no restart
#   $STEVE_HOME/logs/release/    one full, timestamped log per release run
#   $STEVE_HOME/releases/history.jsonl   one JSON line per hub or web release (ok or not)
zmodload zsh/datetime

# every line of a run gets a timestamp (no fork per line: strftime is a zsh builtin); to stdout, and to fd 3 (the log) if open
stamp() {
  local l t log=0; { : >&3 } 2>/dev/null && log=1
  while IFS= read -r l || [[ -n $l ]]; do
    strftime -s t '%Y-%m-%d %H:%M:%S' $EPOCHSECONDS; print -r -- "$t $l"; (( log )) && print -r -- "$t $l" >&3
  done
}
iso() { strftime '%Y-%m-%dT%H:%M:%S%z' ${1:-$EPOCHSECONDS} }

# steps: "== name", and how long the previous one took
STEP="" STEP_T=0
step() {
  [[ -n $STEP ]] && print -r -- "-- $STEP: $(( EPOCHSECONDS - STEP_T ))s"
  STEP=$1 STEP_T=$EPOCHSECONDS
  [[ -n $1 ]] && print -r -- "== $1"
  return 0
}
# show the command, then run it with all its output in the log
run() { print -r -- "\$ ${(j: :)${(q-)@}}"; "$@"; local rc=$?; (( rc )) && print -r -- "   exit $rc: $1"; return $rc }

# JSON string / array of strings
js() { local s=${1-}; s=${s//\\/\\\\}; s=${s//\"/\\\"}; s=${s//$'\n'/\\n}; s=${s//$'\t'/\\t}; s=${s//$'\r'/}; print -rn -- "\"$s\"" }
jarr() { local out="" x; for x in "$@"; do out+="${out:+,}$(js "$x")"; done; print -rn -- "[$out]" }
jbool() { (( $1 )) && print -n true || print -n false }
# a field of a flat release.json
json_field() { sed -n "s/.*\"$2\":\"\\([^\"]*\\)\".*/\\1/p" $1 2>/dev/null | head -1 }
# This machine's build identity, $STEVE_HOME/signing/identity.json (written once per machine, also read by
# ops/local-release.py): an existing deployment keeps its launchd label, signing identifiers and keychain service
# across releases instead of the public defaults. What the environment sets wins; a missing file or field leaves
# the defaults. Only these four names are read, nothing else in the file.
machine_identity() { # home dir
  local f=$1/signing/identity.json v k var
  [[ -f $f ]] || return 0
  for k var in label STEVE_LABEL hub_identifier STEVE_KEYHELPER_HUB_IDENTIFIER keyhelper_identifier STEVE_KEYHELPER_IDENTIFIER vault_service STEVE_KEYHELPER_SERVICE; do
    [[ -n ${(P)var:-} ]] && continue
    v=$(python3 -c 'import json,sys; v=json.load(open(sys.argv[1])).get(sys.argv[2]); print(v if isinstance(v, str) else "")' $f $k 2>/dev/null) || v=""
    [[ $v =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]] && export $var=$v
  done
  return 0
}

# Empty/unknown deployed commits are normal for legacy web builds.
changed_since() { # commit → paths, or "?" when the commit is unknown
  local commit=${1:-}
  if [[ -n $commit ]] && git -C $REPO cat-file -e "$commit^{commit}" 2>/dev/null; then
    git -C $REPO diff --name-only "$commit" HEAD
  else
    print "?"
  fi
}

# install a built web/dist as web build <ver>: copy, tag index.html with the version, write release.json
web_install() { # src dest ver commit branch
  local src=$1 dest=$2 ver=$3 commit=$4 branch=$5 f c
  [[ -f $src/index.html ]] || { print -r -- "✗ $src/index.html is missing"; return 1 }
  rm -rf -- $dest.tmp && mkdir -p $dest.tmp && cp -R $src/. $dest.tmp/ || { print -r -- "✗ copying $src failed"; return 1 }
  f=$dest.tmp/index.html; c=$(<$f)
  [[ $c == *'<head>'* ]] || { print -r -- "✗ $f has no <head> to put the version in"; rm -rf -- $dest.tmp; return 1 }
  # what the release check (and the web UI) reads back: <meta name="steve-web-version" content="<ver>">
  c=${c/"<head>"/<head>
    <meta name=\"steve-web-version\" content=\"$ver\" />}
  print -r -- $c >$f
  print -r -- "{\"version\":$(js "$ver"),\"commit\":$(js "$commit"),\"branch\":$(js "$branch"),\"built_at\":$(js "$(iso)")}" >$dest.tmp/release.json
  rm -rf -- $dest && mv $dest.tmp $dest || { print -r -- "✗ moving the build to $dest failed"; return 1 }
  print -r -- "   installed $dest ($(find $dest -type f | wc -l | tr -d ' ') files, $(du -sh $dest | cut -f1))"
}
# point web/current at a build; ln -n replaces the link itself (BSD mv onto a symlinked directory would move into it)
web_switch() { # webroot ver
  web_switch_target "$1" "$1/$2"
}
web_switch_target() { # webroot symlink-target (preserve relative and legacy external targets)
  # a new link renamed over the old one: rename is atomic, so no request sees a missing web dir (mv -h: don't follow the old link)
  ln -sfn "$2" "$1/.current.new" && mv -fh "$1/.current.new" "$1/current" || { print -r -- "✗ switching $1/current to $2 failed"; return 1 }
  print -r -- "   $1/current -> $(readlink $1/current)"
}
web_restore() { # webroot previous-raw-symlink-target
  local root=$1 target=${2:-} resolved
  [[ -n $target ]] || return 1
  [[ $target == /* ]] && resolved=$target || resolved=$root/$target
  [[ -d $resolved ]] || return 1
  web_switch_target "$root" "$target"
}

# ── the signing keychain (ops/sign-setup.sh) ──
# Its password never goes on a command line: `ps` shows those to every process of the user, agents included.
# `security -i` reads the command from stdin instead (print is a builtin: no process carries it either). The keychain
# is unlocked only around the codesign that needs it and locked again after, failed or not: left unlocked, it lets
# any process sign a binary as the Hub, and steve-keyhelper hands the vault's keys to that binary. It also locks
# itself after 5 idle minutes and on sleep, should a run die in between.
SIGN_PASS_ACCOUNT=steve/signing/keychain-pass
kc_security() { # security -i command line (with a password in it) → security's status
  print -r -- "$1" | security -i
}
kc_quotable() { # password keychain: what a `security -i` line can carry (no quoting inside it)
  [[ -n $1 && $1 != *[[:space:]\"\'\\]* && $2 != *[\"\\$'\n']* ]] && return 0
  print -u2 -r -- "   the signing keychain's password or path has a space, quote or backslash: not passed to security"
  return 1
}
kc_unlock() { # keychain password
  kc_quotable "$2" "$1" || return 1
  kc_security "unlock-keychain -p $2 \"$1\"" || return 1
  security set-keychain-settings -l -u -t 300 "$1"
}
kc_lock() { # keychain
  security lock-keychain "$1" || print -u2 -r -- "   could not lock $1 again"
}
kc_sign() { # keychain password command…: runs the command with the keychain unlocked, then locks it whatever happened
  local kc=$1 pass=$2 rc=0
  shift 2
  kc_unlock $kc "$pass" || { print -u2 -r -- "   cannot unlock $kc"; return 1 }
  "$@" || rc=$?
  kc_lock $kc
  return $rc
}
# The keychain's password: signing/keychain-pass until the Hub moves it into the login keychain (through
# steve-keyhelper, hub-rs upgrades/signing.rs), then that entry. Only the signed Hub reads the entry unasked; anyone
# else meets a macOS prompt, so only a run from a terminal asks for it (the Hub's one-click upgrade gets it from the
# Hub instead). "Always Allow" on that prompt would let every process read it unasked: allow it once.
sign_pass() { # keychain → the password on stdout
  local f=${1:h}/keychain-pass b64
  if [[ -f $f ]]; then print -r -- "$(<$f)"; return 0; fi
  if [[ ! -t 0 ]]; then
    print -u2 -r -- "   the signing keychain's password is in the login keychain, and only a run from a terminal asks macOS for it: upgrade from Steve's settings, or run this in Terminal"
    return 1
  fi
  print -u2 -r -- "   macOS asks for the login keychain item $SIGN_PASS_ACCOUNT: click 允许 (Allow), never 始终允许 (Always Allow), which would let every process read it unasked"
  b64=$(security find-generic-password -s ${STEVE_KEYHELPER_SERVICE:-io.github.infranite.steve.vault} -a $SIGN_PASS_ACCOUNT -w 2>/dev/null) || {
    print -u2 -r -- "   the signing keychain's password did not open (the prompt was refused)"
    return 1
  }
  print -r -- $b64 | base64 -D | od -An -v -tx1 | tr -d ' \n'
}
