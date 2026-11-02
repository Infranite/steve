#!/bin/zsh
# Release only the web build: no drain, no Hub restart. Same as `release.sh --web` (preflight, ff-only merge into main,
# build web/ into $STEVE_HOME/web/<ver>, switch web/current, check / and an asset, switch back and reset main on failure).
# Usage: release-web.sh <branch|main> [item-id]
exec /bin/zsh ${0:A:h}/release.sh --web "$@"
