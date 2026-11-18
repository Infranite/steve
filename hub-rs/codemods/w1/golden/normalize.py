#!/usr/bin/env python3
"""Normalize identifiers and volatile metadata, retaining workspace as separate evidence."""
import argparse
from collections import defaultdict
import json
from pathlib import Path

HUB = {'device', 'release.hold'}
USER = {'node', 'client'}
LIVE = {'event', 'steve.thinking', 'steve.draft', 'steve.tool'}
WIRE = set('item decision session event message steve.thinking steve.draft steve.tool thread.run release.hold bot project tag workspace settings device node client party mm changeset account connection vault signal flow run space chan_message persona digest calendar channel yolo yolo_action stats kb gh mail'.split())
VOLATILE = {'rev', 'epoch', 'cursor', 'created', 'updated', 'started_at', 'at', 'timestamp', 'ts'}


class Normalizer:
    def __init__(self, manifest):
        self.ids, self.counts = {}, defaultdict(int)
        # The manifest records first HTTP observation, before cross-connection scheduling can reorder IDs.
        for ws in manifest['workspaces'].values():
            self.ident(ws, 'workspace')
        for entity, meta in manifest['entities'].items():
            self.ident(entity, meta['kind'])

    def ident(self, value, kind):
        if value not in self.ids:
            self.counts[kind] += 1
            self.ids[value] = f'<{kind}#{self.counts[kind]}>'
        return self.ids[value]

    def value(self, value, key='', ty=''):
        if isinstance(value, dict):
            return {k: self.value(v, k, value.get('type', ty)) for k, v in value.items()
                    if k not in VOLATILE}
        if isinstance(value, list):
            return [self.value(v, key, ty) for v in value]
        if isinstance(value, str):
            if key == 'day':
                return '<day>'
            if value in self.ids:
                return self.ids[value]
            kind = {'id': ty, 'ws': 'workspace', 'session_id': 'session', 'item_id': 'item', 'mid': 'message',
                    'user': 'user', 'flow_id': 'flow', 'space_id': 'space', 'run': 'run'}.get(key)
            if kind and value not in ('', '*', 'steve', 'hub'):
                return self.ident(value, kind)
            # Routes contain observed entity IDs; text and other semantic strings stay intact.
            if key == 'route':
                for entity, placeholder in self.ids.items():
                    value = value.replace(entity, placeholder)
            return value
        return value


def read_run(path):
    path = Path(path)
    manifest = json.loads((path / 'manifest.json').read_text())
    streams = {name: [json.loads(line) for line in (path / f'{name}.jsonl').read_text().splitlines()]
               for name in manifest['workspaces']}
    return manifest, streams


def owner(data, row, manifest):
    ty = data['type']
    if ty in HUB or ty in USER or ty.startswith('stream.') or ty == 'ping':
        return None
    if ty == 'stats':
        # Stats identifies a workspace aggregate, not an entity ID. It may be emitted by periodic jobs.
        if data.get('ws') not in manifest['workspaces'].values():
            raise ValueError('stats names an unknown workspace')
        return data['ws']
    key = data.get('session_id') if ty == 'event' else data.get('id', data.get('mid'))
    if key in manifest['entities']:
        return manifest['entities'][key]['ws']
    steps = {s['name']: s['ws'] for s in manifest['steps']}
    if row['step'] not in steps:
        raise ValueError(f"no independent entity/step ownership: {row['step']} {ty}")
    return steps[row['step']]


def normalized(path, scoped=False, project=False):
    manifest, streams = read_run(path)
    normal = Normalizer(manifest)
    result = {}
    for name, rows in streams.items():
        grouped = defaultdict(list)
        for row in rows:
            if row['event'] == 'ping':
                continue
            data = json.loads(row['data'])
            ty = data.get('type')
            if ty == 'ping':
                continue  # Transport liveness depends on run duration, never on mutations.
            if ty not in WIRE and ty not in {'stream.reset', 'stream.gap'}:
                raise ValueError(f'unknown wire type: {ty}')
            expected = owner(data, row, manifest)
            ws = manifest['workspaces'][name]
            if expected and (scoped or 'ws' in data):
                if data.get('ws') != expected or ws != expected:
                    raise ValueError(f"workspace mismatch/leak: {name} {row['step']} {data}")
            if project and expected and ws != expected:
                continue
            frame = {'event': row['event'], 'id': '<cursor>' if row['id'] else None,
                     'data': normal.value({k: v for k, v in data.items() if k != 'ws'}),
                     'ws': normal.ident(data['ws'], 'workspace') if 'ws' in data else None}
            grouped[row['step']].append(frame)
        result[name] = dict(grouped)
    return normal.value(manifest['steps']), result


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('run')
    args = parser.parse_args()
    print(json.dumps(normalized(args.run), ensure_ascii=False, indent=2))
