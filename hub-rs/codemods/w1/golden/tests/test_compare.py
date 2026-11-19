"""Negative checks: a comparator must reject loss, duplicates, semantic changes and leaks."""
import copy
import json
from pathlib import Path
import sys
import tempfile
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from compare import compare


class Comparison(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def fixture(self, name, scoped=False):
        path = self.root / name
        path.mkdir()
        ws = {'a': 'workspace_' + name, 'b': 'other_' + name}
        item = 'item_' + name
        manifest = {'workspaces': ws, 'entities': {
            item: {'kind': 'item', 'ws': ws['a']}},
            'steps': [{'name': 'item.create', 'ws': ws['a'], 'route': '/api/items', 'status': 200}]}
        (path / 'manifest.json').write_text(json.dumps(manifest))
        data = {'type': 'item', 'id': item}
        if scoped:
            data.update(ws=ws['a'], rev=123)
        row = {'step': 'item.create', 'id': name + ':99', 'event': 'message', 'data': json.dumps(data)}
        for stream in ws:
            (path / (stream + '.jsonl')).write_text(
                json.dumps(row) + '\n' if stream == 'a' or not scoped else '')
        return path

    def mutate(self, path, transform, stream='a'):
        file = path / (stream + '.jsonl')
        rows = [json.loads(line) for line in file.read_text().splitlines()]
        transform(rows)
        file.write_text(''.join(json.dumps(row) + '\n' for row in rows))

    def test_baseline_and_scoped_projection(self):
        before = self.fixture('before')
        compare(before, self.fixture('again'))
        compare(before, self.fixture('after', scoped=True), scoped=True)

    def test_missing_duplicate_and_changed_frame_fail(self):
        before = self.fixture('before')
        for name, mutate in [('missing', lambda rows: rows.clear()),
                             ('duplicate', lambda rows: rows.append(copy.deepcopy(rows[0]))),
                             ('semantic', lambda rows: rows[0].update(event='changed'))]:
            after = self.fixture(name)
            self.mutate(after, mutate)
            with self.assertRaises(ValueError):
                compare(before, after)

    def test_wrong_ws_and_cross_connection_leak_fail(self):
        before = self.fixture('before')
        after = self.fixture('wrong', scoped=True)
        def wrong(rows):
            data = json.loads(rows[0]['data'])
            data['ws'] = 'other_wrong'
            rows[0]['data'] = json.dumps(data)
        self.mutate(after, wrong)
        with self.assertRaisesRegex(ValueError, 'workspace mismatch'):
            compare(before, after, scoped=True)
        leaked = self.fixture('leak', scoped=True)
        (leaked / 'b.jsonl').write_text((leaked / 'a.jsonl').read_text())
        with self.assertRaisesRegex(ValueError, 'workspace mismatch'):
            compare(before, leaked, scoped=True)

    def test_ping_is_transport_only_but_transcript_sequence_is_semantic(self):
        before = self.fixture('before')
        after = self.fixture('after')
        self.mutate(after, lambda rows: rows.append(
            {'step': 'item.create', 'id': None, 'event': 'ping', 'data': ''}))
        compare(before, after)
        def transcript(rows):
            rows[0]['data'] = json.dumps({'type': 'event', 'session_id': 'session',
                                          'seq': 5, 'kind': 'text', 'data': {'text': 'hello'}})
        self.mutate(before, transcript)
        self.mutate(after, transcript)
        compare(before, after)
        def next_sequence(rows):
            data = json.loads(rows[0]['data'])
            data['seq'] = 6
            rows[0]['data'] = json.dumps(data)
        self.mutate(after, next_sequence)
        with self.assertRaises(ValueError):
            compare(before, after)

    def stats(self, path, stream='a', step='background', workspace=None):
        manifest = json.loads((path / 'manifest.json').read_text())
        ws = workspace if workspace is not None else manifest['workspaces'][stream]
        row = {'step': step, 'id': 'epoch:101', 'event': 'message',
               'data': json.dumps({'type': 'stats', 'ws': ws, 'day': '2026-01-01', 'rev': 7})}
        with (path / (stream + '.jsonl')).open('a') as output:
            output.write(json.dumps(row) + '\n')

    def test_stats_count_and_step_timing_are_not_a_business_sequence(self):
        before = self.fixture('before')
        after = self.fixture('after', scoped=True)
        self.stats(before, step='item.create')
        self.stats(before, stream='b', step='background')
        self.stats(after, step='coalescing-window-end')
        self.stats(after, step='later-window')
        compare(before, after, scoped=True)

    def test_stats_still_require_known_workspace_and_correct_audience(self):
        before = self.fixture('before')
        unknown = self.fixture('unknown', scoped=True)
        self.stats(unknown, workspace='unknown-workspace')
        with self.assertRaisesRegex(ValueError, 'stats names an unknown workspace'):
            compare(before, unknown, scoped=True)
        leaked = self.fixture('leaked-stats', scoped=True)
        ws = json.loads((leaked / 'manifest.json').read_text())['workspaces']['a']
        self.stats(leaked, stream='b', workspace=ws)
        with self.assertRaisesRegex(ValueError, 'workspace mismatch'):
            compare(before, leaked, scoped=True)
        missing = self.fixture('missing-stats-ws', scoped=True)
        self.stats(missing)
        def remove_ws(rows):
            data = json.loads(rows[-1]['data'])
            data.pop('ws')
            rows[-1]['data'] = json.dumps(data)
        self.mutate(missing, remove_ws)
        with self.assertRaisesRegex(ValueError, 'stats names an unknown workspace'):
            compare(before, missing, scoped=True)

    def test_ignoring_stats_never_hides_a_missing_business_frame(self):
        before = self.fixture('before')
        after = self.fixture('after', scoped=True)
        self.mutate(after, lambda rows: rows.clear())
        self.stats(after)
        with self.assertRaises(ValueError):
            compare(before, after, scoped=True)


if __name__ == '__main__':
    unittest.main()
