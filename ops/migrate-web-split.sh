#!/bin/zsh
# One-time move to separate web releases. Idempotent; restarts nothing, the running Hub keeps serving as it does now.
#   1. $STEVE_HOME/web/<current hub version>/ from current/web/dist (tagged with the version, plus release.json)
#   2. $STEVE_HOME/web/current -> that build
#   3. STEVE_WEB_DIR=$STEVE_HOME/web/current in steve.env (read by ops/run.sh at the next Hub start; launchd's plist only
#      runs run.sh, so steve.env is what sets the environment)
# Run it before the first release whose run.sh serves web/current. Log: logs/release/migrate-web-split-<time>.log
# Usage: migrate-web-split.sh            Env: STEVE_HOME (default ~/.steve)
set -u
source ${0:A:h}/release-lib.sh
HOME_DIR=${STEVE_HOME:-$HOME/.steve}
WEB=$HOME_DIR/web CUR=$HOME_DIR/current ENVF=$HOME_DIR/steve.env HIST=$HOME_DIR/releases/history.jsonl
mkdir -p $WEB $HOME_DIR/logs/release $HOME_DIR/releases
LOG=$HOME_DIR/logs/release/migrate-web-split-$(strftime '%Y%m%d-%H%M%S' $EPOCHSECONDS).log

main() {
  local t0=$EPOCHSECONDS ver commit branch want line changed=0
  step "migrate-web-split in $HOME_DIR"
  [[ -f $CUR/release.json ]] || { print -r -- "✗ $CUR/release.json is missing: no current release"; return 1 }
  ver=$(json_field $CUR/release.json version) commit=$(json_field $CUR/release.json commit) branch=$(json_field $CUR/release.json branch)
  [[ -n $ver ]] || { print -r -- "✗ no version in $CUR/release.json"; return 1 }
  print -r -- "   current hub release: $ver (commit ${commit:-?}, branch ${branch:-?}) at $(readlink $CUR)"

  step "web build"
  if [[ -e $WEB/current ]]; then
    print -r -- "   already done: $WEB/current -> $(readlink $WEB/current)"
  else
    if [[ -f $WEB/$ver/release.json && -f $WEB/$ver/index.html ]]; then
      print -r -- "   $WEB/$ver exists"
    else
      web_install $CUR/web/dist $WEB/$ver "$ver" "$commit" "${branch:-main}" || return 1
    fi
    web_switch $WEB $ver || return 1
    print -r -- "{\"kind\":\"web\",\"version\":$(js "$ver"),\"prev\":\"\",\"commit\":$(js "$commit"),\"branch\":\"migrate-web-split\",\"item\":\"\",\"files_changed\":[],\"started\":$(js "$(iso $t0)"),\"finished\":$(js "$(iso)"),\"duration_s\":$(( EPOCHSECONDS - t0 )),\"ok\":true,\"error\":\"\",\"rolled_back\":false,\"log\":$(js "$LOG")}" >>$HIST
    changed=1
  fi

  step "STEVE_WEB_DIR in $ENVF"
  want="STEVE_WEB_DIR=$WEB/current"
  if [[ ! -f $ENVF ]]; then
    print -r -- "   $ENVF does not exist; run.sh defaults to $WEB/current anyway"
  elif line=$(grep -E '^[[:space:]]*(export[[:space:]]+)?STEVE_WEB_DIR=' $ENVF | tail -1); [[ -n $line ]]; then
    local val=${line#*=} v
    v=$(HOME=$HOME eval "print -r -- $val" 2>/dev/null)
    if [[ ${v:h:A}/${v:t} == ${WEB:A}/current ]]; then print -r -- "   already set: $line"
    else
      print -r -- "   replacing: $line"
      cp $ENVF $ENVF.bak-websplit && print -r -- "   backup: $ENVF.bak-websplit"
      grep -vE '^[[:space:]]*(export[[:space:]]+)?STEVE_WEB_DIR=' $ENVF.bak-websplit >$ENVF
      print -r -- $want >>$ENVF; changed=1
    fi
  else
    cp $ENVF $ENVF.bak-websplit && print -r -- "   backup: $ENVF.bak-websplit"
    { print "# the web build the Hub serves; ops/release.sh switches it for web-only releases (added by migrate-web-split.sh)"; print -r -- $want } >>$ENVF
    print -r -- "   added: $want"; changed=1
  fi

  step "check"
  print -r -- "   web/current -> $(readlink $WEB/current)"
  print -r -- "   web/current/release.json: $(<$WEB/current/release.json)"
  grep -q 'name="steve-web-version"' $WEB/current/index.html && print "   index.html carries the version tag" || { print "✗ index.html has no version tag"; return 1 }
  print -r -- "   steve.env: $(grep -E '^[[:space:]]*(export[[:space:]]+)?STEVE_WEB_DIR=' $ENVF 2>/dev/null || print 'not set (run.sh default)')"
  step ""
  (( changed )) && print "✓ migrated; the Hub picks up web/current at its next start (the next Hub release)" || print "✓ nothing to do"
  print -r -- "   log $LOG"
}
exec 3>>$LOG
main 3>&- 2>&1 | stamp
exit ${pipestatus[1]}
