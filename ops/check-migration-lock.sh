#!/bin/bash
# The migration LOCK of the last released version must be a prefix of this one: lines are only ever appended,
# so a released migration can't change even when its file and its LOCK line are edited together.
# Usage: ops/check-migration-lock.sh [<this version tag>]   (CI passes the tag it builds; locally: HEAD)
set -euo pipefail
cd "$(dirname "$0")/.."
LOCK=hub-rs/crates/steve-store/migrations/LOCK
this=${1:-}
# released versions, newest first, without the one being built (fetched one by one: CI checks out shallow)
prev=$(git ls-remote --tags --refs origin 'v*' | sed 's|.*refs/tags/||' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' |
  grep -vxF "${this:-none}" | sort -V | tail -1)
[ -n "$prev" ] || { echo "no released version: nothing to compare"; exit 0; }
git fetch -q --depth 1 origin "refs/tags/$prev:refs/tags/$prev" 2>/dev/null || true
if ! old=$(git show "$prev:$LOCK" 2>/dev/null); then
  echo "$prev has no $LOCK (released before the lock): nothing to compare"; exit 0
fi
new=$(cat "$LOCK")
case "$new" in
  "$old"*) echo "$LOCK keeps every line of $prev ($(grep -c . <<<"$old") lines) and only appends" ;;
  *) echo "✗ $LOCK changed lines that $prev released:"; diff <(echo "$old") <(echo "$new") || true; exit 1 ;;
esac
