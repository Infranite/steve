#!/bin/zsh
# launchd entry: run the current release with the environment from ~/.steve/steve.env
# A rollback is `current` pointing at another release dir and a restart, and only ever to a release that can open the
# database (ops/schema_gate.py, checked by the scripts that switch; the Rust Hub checks again when it opens it).
HOME_DIR=${STEVE_HOME:-$HOME/.steve}
set -a; [[ -f $HOME_DIR/steve.env ]] && source $HOME_DIR/steve.env; set +a
# the web build is released on its own (ops/release.sh, web/<ver>/ + the web/current link, switched without a restart)
STEVE_WEB_DIR=${STEVE_WEB_DIR:-$HOME_DIR/web/current}
export STEVE_HOME=$HOME_DIR STEVE_WEB_DIR
export STEVE_VERSION=$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' $HOME_DIR/current/release.json 2>/dev/null)
export PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:$PATH
WEB_V=$(sed -n 's/.*"version":"\([^"]*\)".*/\1/p' $STEVE_WEB_DIR/release.json 2>/dev/null || true)
BIN=$HOME_DIR/current/bin/steve-hub
if [[ ! -x $BIN ]]; then
  print "run.sh: $BIN is missing or not executable; $HOME_DIR/current is not a release this script can run"
  exit 1
fi
# the Node.js that runs .js harness adapters
export STEVE_NODE=${STEVE_NODE:-/opt/homebrew/bin/node}
# hub.log is a file: no ANSI colours from tracing
export NO_COLOR=${NO_COLOR:-1}
print "run.sh: Hub ${STEVE_VERSION:-?} ($BIN), web $WEB_V from $STEVE_WEB_DIR"
cd $HOME_DIR/current && exec $BIN
