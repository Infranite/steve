#!/bin/bash
# Released migrations never change. The last released version is the baseline: its LOCK must be a prefix of this one
# (lines are only appended), and a version from before the LOCK gets one computed from its migration files. So a
# released migration can't change even when its file and its LOCK line are edited together.
# Usage: ops/check-migration-lock.sh [<this version tag>]   (CI passes the tag it builds; locally: HEAD)
set -euo pipefail
cd "$(dirname "$0")/.."
DIR=hub-rs/crates/steve-store/migrations
this=${1:-}
# released versions, newest first, without the one being built (fetched one by one: CI checks out shallow)
tags=$(git ls-remote --tags --refs origin 'v*') || { echo "✗ can't list the released versions"; exit 1; }
prev=$(sed 's|.*refs/tags/||' <<<"$tags" | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | grep -vxF "${this:-none}" | sort -V | tail -1 || true)
[ -n "$prev" ] || { echo "no released version: nothing to compare"; exit 0; }
git rev-parse -q --verify "refs/tags/$prev" >/dev/null ||
  git fetch -q --depth 1 origin "refs/tags/$prev:refs/tags/$prev" || { echo "✗ can't fetch $prev"; exit 1; }
if git cat-file -e "$prev:$DIR/LOCK" 2>/dev/null; then
  old=$(git show "$prev:$DIR/LOCK" | grep -v '^#' | grep .)
else
  # before the LOCK: the hash of every migration file it released
  old=$(git ls-tree --name-only "$prev" "$DIR/" | grep '\.sql$' | sort | while read -r f; do
    echo "$(basename "$f") $(git show "$prev:$f" | shasum -a 256 | cut -d' ' -f1)"; done)
fi
[ -n "$old" ] || { echo "✗ $prev released no migrations? (nothing read from it)"; exit 1; }
new=$(grep -v '^#' "$DIR/LOCK" | grep .)
case "$new" in
  "$old"*) echo "$DIR/LOCK keeps the $(grep -c . <<<"$old") migrations $prev released and only appends" ;;
  *) echo "✗ $DIR/LOCK changed migrations that $prev released:"; diff <(echo "$old") <(echo "$new") || true; exit 1 ;;
esac
