#!/usr/bin/env python3
"""Read-only storage preflight. Old top-level schema wrappers never grant permission.

    schema_gate.py state <db>
    schema_gate.py check <db> <release directory>
"""
import json
from contextlib import closing
import pathlib
import re
import sqlite3
import sys

DOMAINS = ('core', 'stream', 'comms')
ITEMS = ('layout', 'contract', *DOMAINS)
PHASES = ('legacy_replay', 'validation', 'install_contract', 'domain_migration')
LEGACY_MIN, LEGACY_MAX = 113, 114
READER_FENCE = 1000


def capabilities(value, *, converter=False):
    """One flat range shape, shared by gates, builders and bundle consumers."""
    if (not isinstance(value, dict) or not set(ITEMS) <= value.keys()
            or set(value) - {*ITEMS, 'legacy'} or (converter and 'legacy' not in value)):
        raise ValueError('missing, unknown or old-format storage capabilities')
    for bounds in value.values():
        if (not isinstance(bounds, dict) or set(bounds) != {'min_read', 'target'}
                or any(type(bounds[k]) is not int for k in bounds)
                or not 0 < bounds['min_read'] <= bounds['target'] <= 2**32-1):
            raise ValueError('invalid storage capability range')
    return value


def release_identity(info):
    """Compile-time binary values only; the old shell exists for published initiators."""
    storage = capabilities(info.get('storage'), converter=True)
    schema = info.get('schema')
    if type(schema) is not int or schema != storage['legacy']['target']:
        raise ValueError('legacy schema wrapper differs from compiled storage head')
    return dict(storage=storage, schema=dict(min_read=storage['legacy']['min_read'], max_read=schema, target=schema))


def matching_storage(manifest, info):
    try:
        return (type(manifest.get('format_version')) is int and manifest['format_version'] == 1
                and capabilities(manifest.get('storage')) == capabilities(info.get('storage')))
    except (ValueError, AttributeError, TypeError):
        return False


def _version(value):
    if not isinstance(value, str) or not re.fullmatch(r'\+?[0-9]+', value):
        raise ValueError('invalid storage metadata integer')
    number = int(value)
    if not 0 <= number <= 2**32-1:
        raise ValueError('storage metadata integer out of range')
    return number


def state(db):
    """Observe the same metadata as SQLite inspect; never initialize or migrate."""
    db = pathlib.Path(db)
    if not db.exists():
        return dict(kind='empty')
    with closing(sqlite3.connect(db.resolve().as_uri() + '?mode=ro', uri=True)) as c:
        objects = {name.lower(): kind for name, kind in c.execute(
            "SELECT name,type FROM sqlite_schema WHERE lower(substr(name,1,7)) != 'sqlite_'")}
        if not objects:
            return dict(kind='empty')
        if 'meta' not in objects:
            if any(t in objects for t in ('domain_schema', 'uow_applied', 'cursor')):
                raise ValueError('partial current storage metadata')
            if objects.get('schema_version') != 'table':
                raise ValueError('unrecognized nonempty storage')
            rows = c.execute('SELECT v FROM schema_version').fetchall()
            if len(rows) != 1 or type(rows[0][0]) is not int or not LEGACY_MIN <= rows[0][0] <= LEGACY_MAX:
                raise ValueError('legacy input must be schema 113..=114')
            if 'schema_compat' in objects:
                if objects['schema_compat'] != 'table':
                    raise ValueError('invalid legacy reader object')
                minimum = c.execute('SELECT min_reader FROM schema_compat').fetchall()
                if len(minimum) != 1 or type(minimum[0][0]) is not int or not 0 <= minimum[0][0] <= LEGACY_MAX:
                    raise ValueError('invalid legacy minimum reader')
            return dict(kind='legacy', schema_version=rows[0][0])
        if (any(objects.get(t) != 'table' for t in ('meta', 'domain_schema', 'uow_applied', 'cursor', 'schema_compat'))
                or 'schema_version' in objects):
            raise ValueError('partial or mixed current storage metadata')
        columns = [(r[1], r[2], r[3], r[4], r[5]) for r in c.execute('PRAGMA table_info(schema_compat)')]
        fence = c.execute('SELECT min_reader FROM schema_compat').fetchall()
        if columns != [('min_reader', 'INTEGER', 1, None, 0)] or fence != [(READER_FENCE,)]:
            raise ValueError('invalid permanent legacy reader fence')
        meta = {}
        for domain, key, value in c.execute('SELECT domain,key,value FROM meta'):
            if domain not in DOMAINS or not isinstance(key, str) or (domain, key) in meta or not isinstance(value, str):
                raise ValueError('duplicate or unknown storage metadata')
            meta[domain, key] = value
        def get(domain, key):
            if (domain, key) not in meta:
                raise ValueError('incomplete storage metadata')
            return meta[domain, key]
        generation, minimum = (_version(get('core', k)) for k in ('layout', 'min_reader'))
        if not get('core', 'store_generation'):
            raise ValueError('missing store identity')
        files = {}
        for domain in DOMAINS:
            file, ident = get(domain, 'file'), get(domain, 'file_id')
            if (not file or file in ('.', '..') or '/' in file or not ident
                    or (domain == 'core' and file != 'steve.sqlite3')
                    or (file in files and files[file] != ident)):
                raise ValueError('invalid storage file placement/identity')
            files[file] = ident
        if len(set(files.values())) != len(files):
            raise ValueError('duplicate physical file identity')
        rows = c.execute('SELECT domain,version FROM domain_schema').fetchall()
        if len(rows) != 3 or {r[0] for r in rows} != set(DOMAINS) or any(
                type(v) is not int or not 0 <= v <= 2**32-1 for _, v in rows):
            raise ValueError('missing, duplicate or unknown domain schema')
        return dict(kind='current', layout=generation, min_reader=minimum,
                    contract=_version(get('core', 'contract_schema')), **dict(rows))


def accepts(bounds, version):
    return bounds['min_read'] <= version <= bounds['target']


def check(db, release):
    """None if declared ranges accept the observed state; otherwise a refusal."""
    try:
        manifest = json.loads((pathlib.Path(release) / 'release.json').read_text())
        if type(manifest.get('format_version')) is not int or manifest['format_version'] != 1:
            raise ValueError('unsupported release manifest format')
        storage = capabilities(manifest.get('storage'))
        observed = state(db)
        if observed['kind'] == 'empty':
            return None
        if observed['kind'] == 'legacy':
            if 'legacy' not in storage or not accepts(storage['legacy'], observed['schema_version']):
                raise ValueError('candidate has no compatible legacy converter')
        elif any(not accepts(storage[k], observed[k]) for k in ITEMS) or not accepts(storage['layout'], observed['min_reader']):
            raise ValueError('candidate storage range does not accept database versions/min_reader')
        return None
    except (OSError, ValueError, TypeError, AttributeError, sqlite3.Error) as error:
        return f'storage preflight refused: {error}; nothing was changed'


def main(argv):
    try:
        if len(argv) == 3 and argv[1] == 'state':
            print(json.dumps(state(argv[2])))
            return 0
        if len(argv) == 4 and argv[1] == 'check':
            why = check(argv[2], argv[3])
            if why:
                print(why)
            return int(why is not None)
    except (OSError, ValueError, sqlite3.Error) as error:
        print(f'storage inspection refused: {error}', file=sys.stderr)
        return 1
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == '__main__':
    sys.exit(main(sys.argv))
