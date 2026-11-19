#!/bin/bash
# Released migrations never change. The last released version is the baseline: its LOCK must be a prefix of this one
# (lines are only appended), and a version from before the LOCK gets one computed from its migration files. So a
# released migration can't change even when its file and its LOCK line are edited together.
# Usage: ops/check-migration-lock.sh [<this version tag>]   (CI passes the tag it builds; locally: HEAD)
set -euo pipefail
cd "$(dirname "$0")/.."
DIR=hub-rs/crates/steve-store/migrations
[ -d "$DIR" ] || DIR=hub-rs/crates/steve-store-sqlite/legacy/migrations
this=${1:-}
# the newest released version before the one being built (fetched one by one: CI checks out shallow); a
# version built again later (a re-run) still compares with the one before it, not with what came after
tags=$(git ls-remote --tags --refs origin 'v*') || { echo "✗ can't list the released versions"; exit 1; }
versions=$(sed 's|.*refs/tags/||' <<<"$tags" | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | grep -vxF "${this:-none}" || true)
if [[ $this =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  prev=$(printf '%s\n' $versions "$this" | sort -V | grep -B1 -xF "$this" | grep -vxF "$this" || true)
else
  prev=$(printf '%s\n' $versions | sort -V | tail -1 || true)
fi
if [ -n "$prev" ]; then
  git rev-parse -q --verify "refs/tags/$prev" >/dev/null ||
    git fetch -q --depth 1 origin "refs/tags/$prev:refs/tags/$prev" || { echo "✗ can't fetch $prev"; exit 1; }
  HISTORY_DIR=$DIR
  if ! git cat-file -e "$prev:$HISTORY_DIR" 2>/dev/null; then
    HISTORY_DIR=hub-rs/crates/steve-store/migrations
  fi
  if git cat-file -e "$prev:$HISTORY_DIR/LOCK" 2>/dev/null; then
    old=$(git show "$prev:$HISTORY_DIR/LOCK" | grep -v '^#' | grep .)
  else
    # before the LOCK: the hash of every migration file it released
    old=$(git ls-tree --name-only "$prev" "$HISTORY_DIR/" | grep '\.sql$' | sort | while read -r f; do
      echo "$(basename "$f") $(git show "$prev:$f" | shasum -a 256 | cut -d' ' -f1)"; done)
  fi
  [ -n "$old" ] || { echo "✗ $prev released no migrations? (nothing read from it)"; exit 1; }
  new=$(grep -v '^#' "$DIR/LOCK" | grep .)
  case "$new" in
    "$old"*) echo "$DIR/LOCK keeps the $(grep -c . <<<"$old") migrations $prev released and only appends" ;;
    *) echo "✗ $DIR/LOCK changed migrations that $prev released:"; diff <(echo "$old") <(echo "$new") || true; exit 1 ;;
  esac

fi
# Every layout keeps its own append-only history. First appearances still verify every hash.
python3 - "$prev" <<'PY'
import hashlib
import pathlib
import subprocess
import sys

base = pathlib.Path('hub-rs/crates/steve-store-sqlite')
legacy = base / 'legacy/migrations'
previous = sys.argv[1]

def fail(message):
    sys.exit('✗ ' + message)

def released(path):
    if not previous:
        return None
    result = subprocess.run(['git', 'cat-file', '-e', f'{previous}:{path}'], capture_output=True)
    if result.returncode:
        return None
    return subprocess.check_output(['git', 'show', f'{previous}:{path}'])

def verify(directory):
    if not directory.is_dir():
        fail(f'missing migration directory: {directory}')
    lock = directory / 'LOCK'
    entries = []
    for line in lock.read_text().splitlines():
        if not line or line.startswith('#'):
            continue
        fields = line.split()
        if len(fields) != 2 or pathlib.Path(fields[0]).name != fields[0]:
            fail(f'invalid LOCK line in {lock}: {line}')
        filename, digest = fields
        if hashlib.sha256((directory / filename).read_bytes()).hexdigest() != digest:
            fail(f'hash mismatch: {directory / filename}')
        entries.append(filename)
    if not entries or len(set(entries)) != len(entries) or set(entries) != {p.name for p in directory.glob('*.sql')}:
        fail(f'{lock} must list every SQL file exactly once')
    old = released(lock)
    if old is not None:
        old_lines = [s for s in old.decode().splitlines() if s and not s.startswith('#')]
        new_lines = [s for s in lock.read_text().splitlines() if s and not s.startswith('#')]
        if new_lines[:len(old_lines)] != old_lines:
            fail(f'{lock} changed the prefix released in {previous}')
    elif released(directory) is not None:
        fail(f'{previous}:{directory} exists without a LOCK')
    print(f'{lock}: all hashes and released prefix verified')

verify(legacy)
old_directory = pathlib.Path('hub-rs/crates/steve-store/migrations')
if old_directory.exists():
    files = {p.name: p.read_bytes() for p in old_directory.iterdir() if p.is_file()}
    archived = {p.name: p.read_bytes() for p in legacy.iterdir() if p.is_file()}
    if archived != files:
        fail('legacy archive differs from steve-store/migrations (including LOCK)')
    print('legacy archive equals the old migration directory byte for byte')
# Once an archive was released it is frozen, not merely append-only.
if previous and released(legacy) is not None:
    paths = subprocess.check_output(['git', 'ls-tree', '-r', '--name-only', previous, str(legacy)]).decode().splitlines()
    expected = {pathlib.Path(p).name: released(p) for p in paths}
    actual = {p.name: p.read_bytes() for p in legacy.iterdir() if p.is_file()}
    if actual != expected:
        fail(f'legacy archive changed since {previous}')
for domain in ('core', 'stream', 'comms', 'contract'):
    verify(base / 'schema' / domain)
PY
