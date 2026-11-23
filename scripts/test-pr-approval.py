#!/usr/bin/env python3
"""Operate an explicitly supplied Hub against empty storage and a fake gh, never an installed Hub.

The initial accepted items and the revocation control are synthetic database fixtures. Redelivery,
acceptance, human merge answers, PR polling and closeout use the built Hub's real MCP/HTTP handlers.
No browser, provider, live GitHub write or production installation is used.
"""
import argparse
import hashlib
import http.cookiejar
import json
import os
from pathlib import Path
import secrets
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request


FAKE_GH = r'''#!/usr/bin/env python3
import json, pathlib, sys
d = pathlib.Path(__file__).parent
a = sys.argv[1:]
with (d / 'calls.jsonl').open('a') as f: f.write(json.dumps(a)+'\n')
s = json.loads((d / 'prs.json').read_text())
if a[:2] == ['pr', 'list']:
    branch = a[a.index('--head')+1]
    p = next((p for p in s.values() if p['branch'] == branch), None)
    if p and p.get('offline'): sys.exit(1)
    print(json.dumps([p] if p else []))
elif a[:2] == ['pr', 'view']:
    p = s.get(a[2])
    if not p or p.get('offline'): sys.exit(1)
    print(json.dumps(p))
elif a[0] == 'api':
    print(json.dumps({'rebaseable': True} if '/pulls/' in a[1] else {'allow_rebase_merge': True}))
elif a[:2] == ['pr', 'merge']:
    p = s[a[2]]
    if a[a.index('--match-head-commit')+1] != p['headRefOid']: sys.exit(1)
    assert p['statusCheckRollup'][0]['conclusion'] == 'SUCCESS'
    p['state'] = 'MERGED'
    p['mergeCommit'] = {'oid': p['headRefOid']}
    tmp = d / 'prs.next'
    tmp.write_text(json.dumps(s)); tmp.replace(d / 'prs.json')
else: sys.exit(1)
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--hub-bin', required=True, type=Path)
    parser.add_argument('--output-dir', required=True, type=Path)
    args = parser.parse_args()
    binary = args.hub_bin.resolve(strict=True)
    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    repo = Path(__file__).resolve().parents[1]
    scratch = repo / '.steve'
    scratch.mkdir(exist_ok=True)
    evidence = {'scope': __doc__.strip(), 'binary_sha256': hashlib.file_digest(binary.open('rb'), 'sha256').hexdigest(), 'checks': []}
    hub = None
    with tempfile.TemporaryDirectory(prefix='pr-approval-', dir=scratch) as directory:
        home = Path(directory)
        (home / 'tmp').mkdir()
        fake = home / 'gh'
        fake.write_text(FAKE_GH)
        fake.chmod(0o700)
        state_file = home / 'prs.json'
        state_file.write_text('{}')
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            port = reserve.getsockname()[1]
        base = f'http://127.0.0.1:{port}'
        environment = {'PATH': os.environ['PATH'], 'HOME': str(home), 'TMPDIR': str(home / 'tmp'),
                       'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull,
                       'STEVE_HOME': str(home), 'STEVE_HOST': '127.0.0.1', 'STEVE_PORT': str(port),
                       'STEVE_PUBLIC_URL': base, 'STEVE_DEV_LOGIN': '1',
                       'STEVE_NO_RESUME': '1', 'STEVE_BACKUP': 'off',
                       'STEVE_VAULT_KEYSTORE': 'file', 'STEVE_FORBID_REAL_HOSTS': '1',
                       'STEVE_REPO': str(home / 'no-source-checkout'), 'STEVE_GH': str(fake),
                       'STEVE_ADAPTER_CLAUDE': '/usr/bin/false', 'STEVE_ADAPTER_CODEX': '/usr/bin/false'}
        cookies = http.cookiejar.CookieJar()
        class NoRedirect(urllib.request.HTTPRedirectHandler):
            def redirect_request(self, req, fp, code, msg, headers, newurl):
                return None
        client = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(cookies), urllib.request.ProxyHandler({}), NoRedirect())
        log = (output / 'isolated-hub.log').open('w')

        def request(path, body=None, token=None):
            headers = {'Origin': base, 'Sec-Fetch-Site': 'same-origin', 'x-steve-ws': 'default', 'Content-Type': 'application/json', 'Accept': 'application/json, text/event-stream'}
            if token:
                headers.update({'Authorization': 'Bearer ' + token, 'mcp-protocol-version': '2025-06-18'})
            req = urllib.request.Request(base + path, data=json.dumps(body).encode() if body is not None else None, headers=headers)
            try:
                with client.open(req, timeout=15) as response:
                    raw = response.read()
                    return json.loads(raw) if raw and response.headers.get_content_type() == 'application/json' else raw.decode()
            except urllib.error.HTTPError as error:
                # Development sign-in sets the real browser cookie, then redirects to the Web
                # client. This API-only exercise deliberately has no Web bundle installed.
                if path == '/auth/dev' and error.code in (302, 303):
                    return {'signed_in': True}
                raise AssertionError(f'{path}: HTTP {error.code}: {error.read().decode()}') from error

        def wait(name, predicate, timeout=80):
            until = time.monotonic() + timeout
            while time.monotonic() < until:
                assert hub.poll() is None, 'isolated Hub exited'
                try:
                    result = predicate()
                    if result:
                        return result
                except (OSError, urllib.error.URLError):
                    pass
                time.sleep(0.2)
            raise AssertionError('timed out: ' + name)

        def start():
            nonlocal hub
            hub = subprocess.Popen([str(binary)], cwd=home, env=environment, stdout=log, stderr=log)
            evidence.setdefault('started_pids', []).append(hub.pid)
            wait('Hub ready', lambda: request('/auth/hello'), 25)
            request('/auth/dev')

        def stop():
            if hub is not None and hub.poll() is None:
                hub.terminate()  # Only the exact process this script started.
                try:
                    hub.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    hub.kill()
                    hub.wait(timeout=10)

        def database():
            c = sqlite3.connect(home / 'data/steve.sqlite3', timeout=20)
            c.row_factory = sqlite3.Row
            return c

        def cards(item, kind='approve'):
            with database() as c:
                return [dict(d) for d in c.execute('SELECT * FROM decisions WHERE item_id=? AND kind=? ORDER BY created', (item, kind))]

        def merges():
            calls = home / 'calls.jsonl'
            return [a for a in map(json.loads, calls.read_text().splitlines()) if a[:2] == ['pr', 'merge']] if calls.exists() else []

        def record(name, detail):
            evidence['checks'].append({'name': name, 'passed': True, 'operated': True, 'detail': detail})
            print('PASS', name, flush=True)

        def git(cwd, *command):
            return subprocess.check_output(['git', '-C', str(cwd), *command], env=environment, stderr=subprocess.PIPE, text=True).strip()

        try:
            start()
            stop()
            # Only synthetic records in this script's own empty installation, while stopped.
            states, fixtures = {}, []
            now = int(time.time() * 1000)
            with database() as c:
                for number, case in enumerate(['same', 'other-pr', 'revoked', 'offline', 'other-sha'], 7):
                    item = f'ST-{number}'
                    cwd = home / f'work-{number}'
                    cwd.mkdir()
                    git(cwd, 'init', '-q', '-b', 'main')
                    git(cwd, 'config', 'user.email', 'acceptance@example.test')
                    git(cwd, 'config', 'user.name', 'Acceptance fixture')
                    git(cwd, 'commit', '-q', '--allow-empty', '-m', 'base')
                    bare = home / f'origin-{number}.git'
                    git(home, 'clone', '-q', '--bare', str(cwd), str(bare))
                    git(cwd, 'config', f'url.{bare}.insteadOf', 'https://github.com/acme/app.git')
                    git(cwd, 'remote', 'add', 'origin', 'https://github.com/acme/app.git')
                    git(cwd, 'fetch', '-q', 'origin')
                    branch = f'item/{number}'
                    git(cwd, 'switch', '-q', '-c', branch)
                    (cwd / 'change.txt').write_text(case + '\n')
                    git(cwd, 'add', '.')
                    git(cwd, 'commit', '-q', '-m', 'change')
                    sha = git(cwd, 'rev-parse', 'HEAD')
                    fingerprint = sha + ':' + git(cwd, 'rev-parse', 'HEAD^{tree}')
                    project = 'project-' + case
                    c.execute("INSERT INTO projects(id,workspace_id,name,path,kind,worktree,release,created,updated) VALUES(?,'default',?,?,'git',1,?, ?,?)", (project, project, str(cwd), json.dumps({'kind':'script','cmd':'','done_when':'merge'}), now, now))
                    closing = {'kind':'complete','reason':'Synthetic initial acceptance','by':'fixture','at':now,'frozen_revision':1,'policy':'human','accepted':True,'fingerprint':fingerprint}
                    c.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,project_id,cwd,branch,accept_mode,phase,cycle,revision,closing,created,updated) VALUES(?,'default',?,?,'run','closeout',?,?,?,'manual','closing',1,1,?,?,?)", (item, number, case, project, str(cwd), branch, json.dumps(closing), now, now))
                    c.execute('INSERT INTO item_goals(item_id,version,text,created) VALUES(?,1,?,?)', (item, 'Isolated merge consent acceptance', now))
                    token = secrets.token_hex(32)
                    c.execute("INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,token_hash,title,created,updated) VALUES(?,'default',?,'test',?,'idle',?,'Synthetic worker',?,?)", ('session-'+case, item, str(cwd), hashlib.sha256(token.encode()).hexdigest(), now, now))
                    ev = {'asked':True,'own':True,'pr_slug':'acme/app','pr_branch':branch,'pr':number,'pr_state':'OPEN','pr_head':sha}
                    c.execute("INSERT INTO item_operations(id,item_id,cycle,revision,kind,required,status,evidence,created,updated) VALUES(?,?,1,1,'release',1,'pending',?,?,?)", ('release-'+case, item, json.dumps(ev), now, now))
                    states[str(number)] = {'number':number,'branch':branch,'state':'OPEN','url':f'https://github.com/acme/app/pull/{number}', 'headRefOid':sha,'isDraft':False,'mergeable':'MERGEABLE','mergeStateStatus':'CLEAN', 'statusCheckRollup':[{'name':'CI gate','status':'IN_PROGRESS','conclusion':''}]}
                    fixtures.append({'item':item,'case':case,'sha':sha,'token':token,'number':number,'cwd':cwd})
            state_file.write_text(json.dumps(states))
            start()
            for f in fixtures:
                card = wait('initial card ' + f['case'], lambda: next((d for d in cards(f['item']) if d['subject_type']=='pr_awaiting_merge' and d['status']=='open'), None))
                request('/api/decisions/' + card['id'], {'choice':'yes','text':'Isolated human approval'})
                f['decision'] = card['id']
                result = json.loads(cards(f['item'])[0]['result'])
                assert result['merge_target'] == {'repo':'acme/app','pr':f['number'],'head':f['sha']}
            record('实际 HTTP 人工批准固定仓库、PR 和完整 SHA', {'items': [f['item'] for f in fixtures]})
            for f in fixtures:
                response = request('/mcp', {'jsonrpc':'2.0','id':1,'method':'tools/call','params':{'name':'steve_deliver','arguments':{'summary':'Report clarified; code and PR unchanged','outputs':[],'checks':[{'name':'Fixture result unchanged','passed':True}],'unfinished':[]}}}, f['token'])
                assert not response.get('error') and not response['result'].get('isError'), response
                accept = wait('acceptance card', lambda: next((d for d in cards(f['item'], 'accept') if d['status']=='open'), None))
                request('/api/decisions/' + accept['id'], {'choice':'pass','text':'Accept clarified report'})
                def following():
                    with database() as c:
                        row = c.execute("SELECT o.evidence FROM item_operations o JOIN items i ON i.id=o.item_id AND i.revision=o.revision WHERE i.id=? AND o.kind='release'", (f['item'],)).fetchone()
                        return row and json.loads(row[0]).get('pr_slug')
                wait('new closeout following PR', following)
            stop()
            # Controls change only this installation/fake host. The production API has no revoke UI.
            states['18'] = states.pop('8')
            states['18'].update(number=18, url='https://github.com/acme/app/pull/18')
            states['10']['offline'] = True
            # A real Git commit, with the same tree, changes the accepted worktree's exact head.
            changed = fixtures[4]
            git(changed['cwd'], 'commit', '-q', '--allow-empty', '-m', 'new exact head')
            changed['changed_sha'] = git(changed['cwd'], 'rev-parse', 'HEAD')
            assert changed['changed_sha'] != changed['sha']
            states['11']['headRefOid'] = changed['changed_sha']
            with database() as c:
                c.execute("UPDATE decisions SET status='expired' WHERE id=?", (fixtures[2]['decision'],))
            state_file.write_text(json.dumps(states))
            start()  # Empty in-memory caches: approval must come from SQLite.
            same = fixtures[0]
            def observed_all():
                details = [request('/api/items/' + f['item']) for f in fixtures]
                return details if all(d.get('closeout', {}).get('last_success_at') or d.get('closeout', {}).get('sync_error') for d in details) else None
            details = wait('redelivery observed', observed_all)
            assert len(cards(same['item'])) == 1
            assert details[0]['closeout']['status'] == 'waiting_checks', details[0].get('closeout')
            assert not merges(), merges()
            record('同 SHA 重交付并重启后没有重复批准卡，CI pending 仍等待', {'item':same['item'],'head':same['sha'],'cards':1,'closeout':details[0]['closeout']})
            for f in [fixtures[1], fixtures[2], changed]:
                assert len([d for d in cards(f['item']) if d['status']=='open' and d['subject_type']=='pr_awaiting_merge']) == 1
            assert details[3]['closeout']['status'] == 'sync_failed', details[3].get('closeout')
            record('真实换 SHA、不同 PR、撤回批准、无法核验均未合并', {'other_pr_cards':len(cards('ST-8')),'revoked_cards':len(cards('ST-9')),'changed_sha_cards':len(cards('ST-11')),'old_sha':changed['sha'],'new_sha':changed['changed_sha'],'offline_status':details[3]['closeout']['status']})
            stop()
            states = json.loads(state_file.read_text())
            states['7']['statusCheckRollup'] = [{'name':'CI gate','status':'COMPLETED','conclusion':'SUCCESS'}]
            state_file.write_text(json.dumps(states))
            start()
            wait('SHA-pinned fake merge', lambda: merges())
            detail = wait('item closed', lambda: (d if (d := request('/api/items/' + same['item'])).get('phase')=='closed' else None))
            assert merges() == [['pr','merge','7','--repo','acme/app','--rebase','--match-head-commit',same['sha']]], merges()
            assert len(cards(same['item'])) == 1
            record('CI 成功后复用原批准，仅假服务收到准确 SHA 合并且事项关闭', {'merge_calls':merges(),'phase':detail['phase'],'cards':1})
        except Exception as error:
            evidence['checks'].append({'name':'isolated Hub acceptance','passed':False,'operated':True,'error':str(error)})
            raise
        finally:
            stop()
            log.close()
            evidence['fake_gh_calls'] = (home / 'calls.jsonl').read_text().splitlines() if (home / 'calls.jsonl').exists() else []
            (output / 'pr-approval-operation.json').write_text(json.dumps(evidence, ensure_ascii=False, indent=2) + '\n')


if __name__ == '__main__':
    main()
