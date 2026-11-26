#!/bin/bash
# Released migrations never change. The last released version is the baseline: its LOCK must be a prefix of this one
# (lines are only appended), and a version from before the LOCK gets one computed from its migration files. So a
# released migration can't change even when its file and its LOCK line are edited together.
# Usage: ops/check-migration-lock.sh [--offline] [<this version tag>]   (CI passes the tag it builds; locally: --offline)
set -euo pipefail
cd "$(dirname "$0")/.."
DIR=hub-rs/crates/steve-store/migrations
offline=0
this=
for arg in "$@"; do
  case "$arg" in
    --offline) offline=1 ;;
    -*) echo "✗ unknown option: $arg" >&2; exit 2 ;;
    *) this="$arg" ;;
  esac
done
# the newest released version before the one being built (fetched one by one: CI checks out shallow); a
# version built again later (a re-run) still compares with the one before it, not with what came after
if [ "$offline" -eq 1 ]; then
  versions=$(git tag -l 'v*' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | grep -vxF "${this:-none}" || true)
else
  tags=$(git ls-remote --tags --refs origin 'v*') || { echo "✗ can't list the released versions"; exit 1; }
  versions=$(sed 's|.*refs/tags/||' <<<"$tags" | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | grep -vxF "${this:-none}" || true)
fi
if [[ $this =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  prev=$(printf '%s\n' $versions "$this" | sort -V | grep -B1 -xF "$this" | grep -vxF "$this" || true)
else
  prev=$(printf '%s\n' $versions | sort -V | tail -1 || true)
fi
if [ -n "$prev" ]; then
  if [ "$offline" -eq 1 ]; then
    git rev-parse -q --verify "refs/tags/$prev" >/dev/null || { echo "✗ local tag $prev not found in --offline mode"; exit 1; }
  else
    git rev-parse -q --verify "refs/tags/$prev" >/dev/null ||
      git fetch -q --depth 1 origin "refs/tags/$prev:refs/tags/$prev" || { echo "✗ can't fetch $prev"; exit 1; }
  fi
  HISTORY_DIR=$DIR
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
# Every file listed in LOCK hashes as listed, every SQL file is listed exactly once, and numbered
# migrations are strictly contiguous (001.sql, 002.sql, ..., 999.sql, 1000.sql) with only 027_items.sql allowlisted.
python3 - "$DIR" <<'PY'
import hashlib, pathlib, re, sys
ALLOWED_HELPERS = {"027_items.sql"}
d = pathlib.Path(sys.argv[1])
names = []
for line in (d / 'LOCK').read_text().splitlines():
    if not line or line.startswith('#'):
        continue
    parts = line.split()
    if len(parts) != 2:
        sys.exit(f'✗ malformed LOCK line: {line!r}')
    name, digest = parts
    sql_path = d / name
    if not sql_path.is_file() or sql_path.is_symlink():
        sys.exit(f'✗ missing or non-regular migration file: {sql_path}')
    if hashlib.sha256(sql_path.read_bytes()).hexdigest() != digest:
        sys.exit(f'✗ hash mismatch: {sql_path}')
    names.append(name)
if len(set(names)) != len(names) or set(names) != {p.name for p in d.glob('*.sql')}:
    sys.exit(f'✗ {d}/LOCK must list every SQL file exactly once')
numbered = []
for name in names:
    if name in ALLOWED_HELPERS:
        continue
    if not re.fullmatch(r'[0-9]{3,}\.sql', name):
        sys.exit(f'✗ disallowed migration filename (must be contiguous NNN.sql): {name}')
    numbered.append(name)
for idx, name in enumerate(numbered, start=1):
    expected = f'{idx:03d}.sql'
    if name != expected:
        sys.exit(f'✗ migration numbering gap or order mismatch: expected {expected}, found {name}')
print(f'{d}/LOCK: all {len(numbered)} contiguous migrations and hashes verified')
PY
