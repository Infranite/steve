#!/bin/bash
# Verify only retained legacy 113/114 and every domain LOCK. First archive publication
# must continue the last published old chain; afterwards that archive is frozen.
set -euo pipefail
cd "$(dirname "$0")/.."
this=${1:-}
tags=$(git ls-remote --tags --refs origin 'v*') || { echo "cannot list released versions"; exit 1; }
versions=$(sed 's|.*refs/tags/||' <<<"$tags" | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | grep -vxF "${this:-none}" || true)
if [[ $this =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  prev=$(printf '%s\n' $versions "$this" | sort -V | grep -B1 -xF "$this" | grep -vxF "$this" || true)
else
  prev=$(printf '%s\n' $versions | sort -V | tail -1 || true)
fi
[ -n "$prev" ] || { echo "cannot establish released legacy migration provenance"; exit 1; }
git rev-parse -q --verify "refs/tags/$prev" >/dev/null || git fetch -q --depth 1 origin "refs/tags/$prev:refs/tags/$prev"
python3 -I - "$prev" <<'PY'
import hashlib
from pathlib import Path
import re
import subprocess
import sys

previous=sys.argv[1]
base=Path('hub-rs/crates/steve-store-sqlite')

def fail(message): raise SystemExit('migration LOCK refused: '+message)
def exists(path):
    return subprocess.run(['git','cat-file','-e',f'{previous}:{path}'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL).returncode==0
def old(path):
    if not exists(path): fail(f'missing published path {previous}:{path}')
    return subprocess.check_output(['git','show',f'{previous}:{path}'])
def names(path,published):
    if published:
        return [Path(p).name for p in subprocess.check_output(['git','ls-tree','--name-only',previous,str(path)+'/']).decode().splitlines()]
    if not path.is_dir() or path.is_symlink(): fail(f'missing directory {path}')
    for p in path.iterdir():
        if not p.is_file() or p.is_symlink(): fail(f'not an ordinary migration file: {p}')
    return [p.name for p in path.iterdir()]
def historical(name):
    match=re.fullmatch(r'([0-9]{3})(?:_[A-Za-z0-9_]+)?\.sql',name)
    return match and int(match[1])<113

def verify(path,*,legacy=False,published=False):
    files=names(path,published)
    if 'LOCK' not in files: fail(f'missing LOCK: {path}')
    read=old if published else lambda p:p.read_bytes()
    data=read(path/'LOCK')
    entries=[]
    for line in data.decode().splitlines():
        fields=line.split()
        if not fields or line.startswith('#'): continue
        if legacy and historical(fields[0]): continue
        if len(fields)!=2 or Path(fields[0]).name!=fields[0] or not re.fullmatch('[0-9a-f]{64}',fields[1]):
            fail(f'invalid LOCK entry in {path}: {line}')
        filename,digest=fields
        if filename not in files or hashlib.sha256(read(path/filename)).hexdigest()!=digest:
            fail(f'missing SQL or hash mismatch: {path/filename}')
        entries.append((filename,digest))
    listed=[e[0] for e in entries]
    actual={f for f in files if f!='LOCK' and not (legacy and historical(f))}
    if not listed or len(set(listed))!=len(listed) or set(listed)!=actual:
        fail(f'missing/extra/duplicate migration in {path}')
    if legacy:
        if listed not in [['113.sql'],['113.sql','114.sql']]: fail('legacy must retain exactly 113 and optional 114')
    else:
        for number,name in enumerate(listed,1):
            if not re.fullmatch(rf'{number:04}(?:_[A-Za-z0-9_]+)?\.sql',name): fail(f'noncontiguous domain LOCK {path}')
    return entries

legacy=base/'legacy/migrations'
current=verify(legacy,legacy=True)
if exists(legacy):
    released=verify(legacy,legacy=True,published=True)
    if (current!=released or (legacy/"LOCK").read_bytes()!=old(legacy/"LOCK")
            or any((legacy/name).read_bytes()!=old(legacy/name) for name,_ in released)):
        fail('published legacy 113/114 archive changed')
else:
    predecessor=Path('hub-rs/crates/steve-store/migrations')
    released=verify(predecessor,legacy=True,published=True)
    if current[:len(released)]!=released or any((legacy/name).read_bytes()!=old(predecessor/name) for name,_ in released):
        fail('first archive does not continue the published legacy LOCK/SQL chain')
print(f'{legacy}: retained 113/114 hashes and published provenance verified')
for domain in ('core','stream','comms','contract'):
    path=base/'schema'/domain
    entries=verify(path)
    if exists(path):
        released=verify(path,published=True)
        if entries[:len(released)]!=released: fail(f'published domain LOCK prefix changed: {path}')
    print(f'{path}: all hashes and append-only published prefix verified')
PY
