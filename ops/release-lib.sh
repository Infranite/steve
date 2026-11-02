#!/bin/zsh
# Shared by ops/release.sh and ops/migrate-web-split.sh (sourced, not run): log helpers, JSON helpers and the web build layout.
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
  # a new link renamed over the old one: rename is atomic, so no request sees a missing web dir (mv -h: don't follow the old link)
  ln -sfn $1/$2 $1/.current.new && mv -fh $1/.current.new $1/current || { print -r -- "✗ switching $1/current to $1/$2 failed"; return 1 }
  print -r -- "   $1/current -> $(readlink $1/current)"
}
