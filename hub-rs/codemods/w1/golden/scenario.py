#!/usr/bin/env python3
"""Record real HTTP mutations against a fresh, checkout-local Hub only."""
import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import time
import urllib.error
import urllib.request

from record import Recorder

ROOT = Path(__file__).resolve().parents[4]


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class Api:
    def __init__(self, port):
        self.base = f'http://127.0.0.1:{port}'
        self.cookies = http.cookiejar.CookieJar()
        self.http = urllib.request.build_opener(urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(self.cookies), NoRedirect())
        self.ws = None

    def call(self, method, path, body=None, expected=200):
        headers = {'Accept-Language': 'en', 'Origin': self.base}
        if self.ws:
            headers['x-steve-ws'] = self.ws
        if body is not None:
            headers['Content-Type'] = 'application/json'
        request = urllib.request.Request(self.base + path,
            data=json.dumps(body).encode() if body is not None else None,
            headers=headers, method=method)
        try:
            with self.http.open(request, timeout=20) as response:
                self.cursor = response.headers.get('x-steve-cursor', getattr(self, 'cursor', ''))
                raw = response.read()
                if response.status != expected:
                    raise RuntimeError(f'{method} {path}: expected {expected}, got {response.status}')
                return response.status, json.loads(raw) if raw.startswith((b'{', b'[')) else None
        except urllib.error.HTTPError as error:
            if path == '/auth/dev' and error.code == 302:
                error.close()
                return 302, None
            raw = error.read()
            if error.code == expected:
                return error.code, json.loads(raw)
            raise RuntimeError(f'{method} {path}: {error.code} {raw.decode()}') from error

    def cookie(self):
        return '; '.join(f'{c.name}={c.value}' for c in self.cookies)


def run(api, output, port):
    _, state = api.call('GET', '/api/state')
    first = state['workspace']['id']
    api.ws = first
    _, second = api.call('POST', '/api/workspaces', {'name': 'Golden B', 'item_prefix': 'GB'})
    workspaces = {'a': first, 'b': second['id']}
    api.call('GET', '/api/state')
    entities = {ws: {'kind': 'workspace', 'ws': ws} for ws in workspaces.values()}
    steps = []
    recorder = Recorder(port, api.cookie(), workspaces, output, api.cursor)
    try:
        recorder.quiet(6)

        def step(name, method, path, body=None, kind=None, wait=5.6, expected=200):
            recorder.label(name)
            status, result = api.call(method, path, body, expected)
            if kind and isinstance(result, dict) and result.get('id'):
                entities[result['id']] = {'kind': kind, 'ws': api.ws}
            steps.append({'name': name, 'ws': api.ws, 'method': method,
                          'route': path.split('?')[0], 'status': status})
            recorder.quiet(wait)
            # Independent HTTP readback pins ownership of asynchronously created sessions/decisions/messages.
            _, current = api.call('GET', '/api/state')
            for field, entity_kind in [('items', 'item'), ('decisions', 'decision'),
                ('recent', 'decision'), ('sessions', 'session'), ('thread', 'message')]:
                for entity in current.get(field, []):
                    if entity.get('id'):
                        entities[entity['id']] = {'kind': entity_kind,
                            'ws': entity.get('workspace_id', api.ws)}
            recorder.quiet()
            print(name, flush=True)
            return result

        item = step('item.create', 'POST', '/api/items',
                    {'goal': 'Golden proposed work', 'title': 'Golden work', 'mode': 'propose'}, 'item')
        iid = item['id']
        step('item.title', 'PATCH', f'/api/items/{iid}', {'title': 'Golden renamed'})
        # A supplement is the public API for an item's human comment.
        step('item.comment', 'POST', f'/api/items/{iid}/supplement',
             {'text': 'Golden comment', 'mode': 'auto', 'files': []}, wait=6, expected=500)
        step('item.drop', 'POST', f'/api/items/{iid}/actions', {'action': 'drop'}, wait=6)
        step('item.archive', 'POST', f'/api/items/{iid}/actions', {'action': 'archive'})
        step('item.delete', 'DELETE', f'/api/items/{iid}')
        tag = step('tag.create', 'POST', '/api/tags',
                   {'name': 'Golden tag', 'cat': '主题', 'description': 'Golden'}, 'tag')
        step('tag.rename', 'PATCH', '/api/tags/' + tag['id'], {'name': 'Golden renamed tag'})
        step('tag.delete', 'DELETE', '/api/tags/' + tag['id'])
        project = step('project.create', 'POST', '/api/projects',
                       {'name': 'Golden project', 'path': str(ROOT / 'scratch' / 'golden-project')}, 'project')
        step('project.rename', 'PATCH', '/api/projects/' + project['id'], {'name': 'Golden renamed project'})
        bot = step('bot.create', 'POST', '/api/bots', {'name': 'Golden bot', 'harness': 'codex'}, 'bot')
        step('bot.patch', 'PATCH', '/api/bots/' + bot['id'], {'role': 'Golden role'})
        step('bot.memory', 'POST', '/api/bots/' + bot['id'] + '/memories', {'text': 'Golden memory'})
        step('settings.patch', 'PATCH', '/api/settings', {'archDays': '0'})
        contact = step('party.create', 'POST', '/api/contacts', {'name': 'Golden contact'}, 'party')
        _, systems = api.call('GET', '/api/systems')
        system = next(s for s in systems if s['family'] == 'email')
        step('account.record', 'POST', '/api/accounts', {'system_id': system['id'],
             'party_id': contact['id'], 'kind': 'personal', 'handle': 'golden@example.com'}, 'account')
        step('party.memory', 'POST', '/api/parties/' + contact['id'] + '/memories',
             {'text': 'Golden contact memory'}, 'mm', wait=6)
        step('persona.create', 'POST', '/api/personas',
             {'name': 'Golden persona', 'bot_id': bot['id']}, 'persona')
        flow = step('flow.create', 'POST', '/api/flows', {'name': 'Golden disabled flow',
            'graph': {'nodes': [{'id': 't', 'kind': 'trigger', 'config': {'source': 'manual'}}],
                      'edges': []}}, 'flow')
        step('flow.rename', 'PATCH', '/api/flows/' + flow['id'], {'name': 'Golden renamed flow'})
        flow_run = step('flow.run', 'POST', '/api/flows/' + flow['id'] + '/run',
                        {'input': {'golden': True}})
        entities[flow_run['run_id']] = {'kind': 'run', 'ws': api.ws}
        step('yolo.project.off', 'PUT', '/api/yolo/scopes/project/' + project['id'], {'mode': 'off'})
        step('yolo.off', 'PUT', '/api/yolo', {'on': False})
        step('thread.say', 'POST', '/api/thread', {'text': 'Golden hello', 'files': []}, wait=6)
        api.ws = workspaces['b']
        step('b.item.create', 'POST', '/api/items',
             {'goal': 'Golden B work', 'title': 'Golden B', 'mode': 'propose'}, 'item')
        step('b.party.create', 'POST', '/api/contacts', {'name': 'Golden B contact'}, 'party')
        step('b.workspace.rename', 'PATCH', '/api/workspaces/' + api.ws, {'name': 'Golden B renamed'})
        recorder.label('settle')
        recorder.quiet(6)
    finally:
        recorder.close()
    manifest = {'schema': 1, 'baseline': '3b69ca887', 'workspaces': workspaces,
                'entities': entities, 'steps': steps}
    (output / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--port', type=int, default=18720)
    parser.add_argument('--binary', type=Path, default=ROOT / 'scratch/target/debug/steve-hub')
    args = parser.parse_args()
    if not 18720 <= args.port < 65000:
        parser.error('isolated port must be >=18720 and <65000')
    binary = args.binary.resolve()
    if not binary.is_relative_to(ROOT) or not binary.is_file():
        parser.error('binary must be built inside this checkout')
    args.output.mkdir(parents=True, exist_ok=False)
    scratch = ROOT / 'scratch'
    scratch.mkdir(exist_ok=True)
    if not scratch.resolve().is_relative_to(ROOT):
        parser.error('scratch must stay inside this checkout')
    import tempfile
    home = Path(tempfile.mkdtemp(prefix='golden-', dir=scratch))
    (scratch / 'golden-project').mkdir(exist_ok=True)
    for port in (args.port, args.port + 3):
        with socket.socket() as check:
            check.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            check.bind(('127.0.0.1', port))
    env = {key: os.environ[key] for key in ('HOME', 'USER', 'TMPDIR') if key in os.environ}
    env.update(PATH='/usr/bin:/bin:/usr/sbin:/sbin', STEVE_HOME=str(home),
        STEVE_HOST='127.0.0.1', STEVE_PORT=str(args.port), STEVE_LOCAL_NODE_PORT=str(args.port + 3),
        STEVE_PUBLIC_URL=f'http://127.0.0.1:{args.port}', STEVE_DEV_LOGIN='1', STEVE_NO_RESUME='1',
        STEVE_VAULT_KEYSTORE='file', STEVE_BACKUP='off', STEVE_ADAPTER_UPDATES='off',
        STEVE_ADAPTER_CLAUDE='/usr/bin/false', STEVE_ADAPTER_CODEX='/usr/bin/false',
        STEVE_ADAPTER_PI='/usr/bin/false', STEVE_WEB_DIR=str(ROOT / 'web/dist'),
        NO_COLOR='1', RUST_LOG='warn')
    with (home / 'hub.log').open('w') as log:
        process = subprocess.Popen([str(binary)], cwd=ROOT, env=env, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            api = Api(args.port)
            for _ in range(200):
                if process.poll() is not None:
                    raise RuntimeError(f'Hub exited; see {home}/hub.log')
                try:
                    api.call('GET', '/auth/dev')
                    break
                except (OSError, RuntimeError):
                    time.sleep(0.1)
            else:
                raise RuntimeError('Hub readiness timed out')
            run(api, args.output, args.port)
            manifest_path = args.output / 'manifest.json'
            manifest = json.loads(manifest_path.read_text())
            manifest['binary_sha256'] = hashlib.sha256(binary.read_bytes()).hexdigest()
            manifest_path.write_text(json.dumps(manifest, indent=2) + '\n')
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait()
            # The process group is ours, including any local Node children.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    print(f'closed isolated Hub; recording: {args.output}')


if __name__ == '__main__':
    main()
