#!/bin/zsh
# Installs (or updates) the OpenViking server Steve's knowledge is stored in, as a user launchd job
# next to the Hub, on loopback only:
#   $STEVE_HOME/plugins/openviking/{venv,data,models,home}   the server, its data and model cache
#   $STEVE_HOME/plugins/openviking/ov.conf                    its configuration (owner-only)
#   $STEVE_HOME/secrets/openviking.key                        the root key Steve sends (owner-only)
# then waits for /ready. OpenViking is AGPL-3.0 and runs as its own process; nothing of it is part of
# Steve's source. Vectors only (no model calls), CPU embedding (bge-small-zh, downloaded once).
#
# Afterwards, in $STEVE_HOME/steve.env:
#   STEVE_KB_STORE=openviking
#   STEVE_OPENVIKING_KEY_FILE=$STEVE_HOME/secrets/openviking.key
# and restart the Hub: it mirrors its index on first start (log: "mirroring the index").
setopt err_exit pipe_fail
STEVE_HOME=${STEVE_HOME:-$HOME/.steve}
LABEL=${STEVE_LABEL:-io.github.infranite.steve}-openviking
PORT=${STEVE_OPENVIKING_PORT:-18933}
VERSION=${OPENVIKING_VERSION:-0.5.3}
PYTHON=${STEVE_PYTHON:-python3}
DIR=$STEVE_HOME/plugins/openviking
umask 077
mkdir -p $DIR/data $DIR/models $DIR/home $STEVE_HOME/secrets $STEVE_HOME/logs

[[ -x $DIR/venv/bin/python ]] || $PYTHON -m venv $DIR/venv
$DIR/venv/bin/pip install -q "openviking==$VERSION" "llama-cpp-python==0.3.36"

KEY=$STEVE_HOME/secrets/openviking.key
[[ -s $KEY ]] || $DIR/venv/bin/python -c 'import secrets; print(secrets.token_urlsafe(32))' > $KEY

$DIR/venv/bin/python - "$DIR" "$PORT" "$KEY" <<'PY'
import json, sys
d, port, key = sys.argv[1], int(sys.argv[2]), open(sys.argv[3]).read().strip()
conf = {
    "storage": {"workspace": f"{d}/data"},
    "log": {"level": "INFO", "output": f"{d}/server.log"},
    "embedding": {"dense": {"provider": "local", "model": "bge-small-zh-v1.5-f16", "cache_dir": f"{d}/models"}},
    "auto_generate_l0": False,
    "auto_generate_l1": False,
    "default_search_mode": "fast",
    "server": {"host": "127.0.0.1", "port": port, "auth_mode": "trusted", "root_api_key": key,
               "observability": {"usage_audit": {"enabled": False}}},
}
open(f"{d}/ov.conf", "w").write(json.dumps(conf, indent=2))
PY

PLIST=$HOME/Library/LaunchAgents/$LABEL.plist
cat > $PLIST <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$DIR/venv/bin/openviking-server</string><string>--config</string><string>$DIR/ov.conf</string></array>
  <key>EnvironmentVariables</key><dict><key>HOME</key><string>$DIR/home</string></dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>ThrottleInterval</key><integer>10</integer>
  <key>StandardOutPath</key><string>$STEVE_HOME/logs/openviking.log</string>
  <key>StandardErrorPath</key><string>$STEVE_HOME/logs/openviking.log</string>
</dict></plist>
PL
chmod 644 $PLIST
launchctl bootout gui/$(id -u)/$LABEL 2>/dev/null || true
launchctl bootstrap gui/$(id -u) $PLIST

for i in {1..120}; do
  if [[ $(curl -s -m 2 http://127.0.0.1:$PORT/ready 2>/dev/null) == *'"ready"'* ]]; then
    print "OpenViking $VERSION ready on 127.0.0.1:$PORT ($LABEL)"
    exit 0
  fi
  sleep 2
done
print -u2 "OpenViking did not become ready; see $STEVE_HOME/logs/openviking.log and $DIR/server.log"
exit 1
