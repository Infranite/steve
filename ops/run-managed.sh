#!/bin/sh
# Run as the service manager's long-lived process. No shell code or target URL comes from the API.
set -eu
STEVE_HOME=${STEVE_HOME:-"$HOME/.steve"}
export STEVE_HOME
if [ -f "$STEVE_HOME/steve.env" ]; then
  set -a
  . "$STEVE_HOME/steve.env"
  set +a
fi
exec "$STEVE_HOME/current/bin/steve-hub" --supervise
