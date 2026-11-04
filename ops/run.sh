#!/bin/zsh
# launchd entry: run the current release with the environment from ~/.steve/steve.env
# The Hub is the release's Rust Hub (bin/steve-hub); only a release from before hub-rs/ has the TS Hub (hub/dist).
# A rollback is `current` pointing at another release dir and a restart, and only ever to a release that can open the
# database (ops/schema_gate.py, checked by the scripts that switch; the Rust Hub checks again when it opens it).
HOME_DIR=${STEVE_HOME:-$HOME/.steve}
set -a; [[ -f $HOME_DIR/steve.env ]] && source $HOME_DIR/steve.env; set +a
# the web build is released on its own (ops/release.sh, web/<ver>/ + the web/current link, switched without a restart);
# a release from before the split that still carries its own web/dist serves that until ops/migrate-web-split.sh has run
STEVE_WEB_DIR=${STEVE_WEB_DIR:-$HOME_DIR/web/current}
if [[ ! -e $STEVE_WEB_DIR && -d $HOME_DIR/current/web/dist ]]; then
  print "run.sh: $STEVE_WEB_DIR does not exist, serving $HOME_DIR/current/web/dist (run ops/migrate-web-split.sh)"
  STEVE_WEB_DIR=$HOME_DIR/current/web/dist
fi
export STEVE_HOME=$HOME_DIR STEVE_WEB_DIR
export STEVE_VERSION=$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' $HOME_DIR/current/release.json 2>/dev/null)
export PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH
WEB_V=$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' $STEVE_WEB_DIR/release.json 2>/dev/null || true)
BIN=$HOME_DIR/current/bin/steve-hub
if [[ -x $BIN ]]; then
  # the TS Hub ran .js adapters with its own node; the Rust Hub runs STEVE_NODE
  export STEVE_NODE=${STEVE_NODE:-/opt/homebrew/bin/node}
  # hub.log is a file: no ANSI colours from tracing
  export NO_COLOR=${NO_COLOR:-1}
  print "run.sh: Rust Hub ${STEVE_VERSION:-?} ($BIN), web $WEB_V from $STEVE_WEB_DIR"
  cd $HOME_DIR/current && exec $BIN
fi
print "run.sh: TS Hub ${STEVE_VERSION:-?} (node dist/main.js), web $WEB_V from $STEVE_WEB_DIR"
cd $HOME_DIR/current/hub && exec /opt/homebrew/bin/node dist/main.js
