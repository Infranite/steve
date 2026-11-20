#!/usr/bin/env python3
"""Isolated actual-Hub upgrade and recovery test with three independent release builds.

Usage: python3 ops/test-upgrade-runtime.py OLD_HUB NEW_HUB CANDIDATE_HUB
Build each executable separately with STEVE_RELEASE_VERSION and STEVE_RELEASE_COMMIT
(the actual source SHA), and copy it aside before building the next identity. All
three versions must differ. Runtime STEVE_VERSION cannot replace a compiled identity.
Uses a disposable home/key directory and loopback port; never touches an installation.
"""
import argparse, copy, fcntl, hashlib, json, os, pathlib, re, shutil, signal, socket, sqlite3, subprocess, sys, tempfile, time, urllib.error, urllib.request, uuid

parser = argparse.ArgumentParser(description=__doc__)
for name in ('old', 'new', 'candidate'):
    parser.add_argument(name, type=pathlib.Path, help='independently compiled identity-bearing Hub')
args = parser.parse_args()
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import schema_gate

def release(binary):
    binary = binary.resolve(strict=True)
    identity = json.loads(subprocess.check_output([str(binary), '--build-info'], timeout=15))
    assert isinstance(identity.get('version'), str) and re.fullmatch(r'(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)', identity['version']), identity
    assert isinstance(identity.get('commit'), str) and re.fullmatch(r'[0-9a-fA-F]{40}', identity['commit']), identity
    assert identity.get('target'), identity
    schema_gate.release_identity(identity)
    with binary.open('rb') as source:
        digest = hashlib.file_digest(source, 'sha256').hexdigest()
    return dict(binary=binary, identity=identity, digest=digest)

old_release, new_release, candidate_release = (release(getattr(args, name)) for name in ('old', 'new', 'candidate'))
releases = (old_release, new_release, candidate_release)
assert len({r['identity']['version'] for r in releases}) == 3, 'three distinct compiled versions required'
versions = [tuple(map(int,r['identity']['version'].split('.'))) for r in releases]
assert versions == sorted(versions), 'release versions must increase'
assert len({r['digest'] for r in releases}) == 3, 'the same binary cannot stand in for multiple releases'
assert len({r['identity']['target'] for r in releases}) == 1, 'all builds must target this test host'
for r in releases:
    print('BUILD', json.dumps(dict(**r['identity'], sha256=r['digest'])), flush=True)

def atomic(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp=path.with_suffix('.tmp'); tmp.write_text(json.dumps(value)); tmp.replace(path)

def eventually(fn, label, timeout=90):
    end=time.monotonic()+timeout
    while time.monotonic()<end:
        try:
            result=fn()
            if result: return result
        except (OSError, ValueError, KeyError): pass
        time.sleep(.05)
    raise AssertionError('timeout: '+label)

with tempfile.TemporaryDirectory(prefix='steve-upgrade-smoke-') as temp:
    root=pathlib.Path(temp); home=root/'home'; home.mkdir(); keys=root/'keys'; keys.mkdir(mode=0o700); account=root/'account'; account.mkdir()
    sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close()
    env={**{k: os.environ[k] for k in ('PATH', 'TMPDIR', 'SYSTEMROOT') if k in os.environ},'HOME':str(account),'STEVE_HOME':str(home),'STEVE_HOST':'127.0.0.1','STEVE_PORT':str(port),'STEVE_PUBLIC_URL':f'http://127.0.0.1:{port}', 'STEVE_VAULT_KEYSTORE':'dir','STEVE_VAULT_KEY_DIR':str(keys),'STEVE_NO_RESUME':'1','STEVE_BACKUP':'off','STEVE_ADAPTER_UPDATES':'off','STEVE_NODE_MODE':'off','STEVE_ADAPTER_CLAUDE':'/nonexistent','STEVE_ADAPTER_CODEX':'/nonexistent','CODEX_HOME':str(root/'codex'),'STEVE_REPO':str(root/'repo'),'STEVE_DEV_LOGIN':'1'}
    # Only tool lookup and temporary-file configuration are inherited; auth, channels,
    # web overrides and external integrations cannot leak into this disposable installation.
    assert home.resolve().is_relative_to(root.resolve()) and port != 8440
    def bundle(release, label=None, storage=None):
        identity=release['identity'];version=identity['version']
        payload=copy.deepcopy(schema_gate.release_identity(identity))
        if storage is not None: payload["storage"]=storage
        path=home/'releases'/(label or version);(path/'bin').mkdir(parents=True);(path/'web/dist').mkdir(parents=True)
        shutil.copy2(release['binary'],path/'bin/steve-hub')
        atomic(path/'release.json',dict(format_version=1,version=version,commit=identity['commit'],hub_version=version,web_version=version,api_protocol=1,**payload,assets=[]))
        atomic(path/'web/dist/release.json',{'version':version});(path/'web/dist/index.html').write_text('<html>upgrade fixture</html>')
        return path
    old=bundle(old_release);(home/'current').symlink_to(old)
    log=open(root/'runtime.log','w+')
    process=None
    def start():
        return subprocess.Popen([str(old_release['binary']),'--supervise'],env=env,cwd=root,stdout=log,stderr=log,start_new_session=True)
    def health(release):
        assert process.poll() is None, f'supervisor exited before health: {process.returncode}'
        identity=release['identity'];version=identity['version']
        with urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz',timeout=1) as r: data=json.load(r)
        return data if data.get('version')==version and data.get('web_version')==version and data.get('build_commit')==identity['commit'] and not data.get('maintenance') else None
    def operation(version,target):
        ident=str(uuid.uuid4());now=int(time.time()*1000)
        op=dict(id=ident,idempotency_key='smoke:'+ident,version=version,phase='ready',created_at=now,updated_at=now,requested_by='smoke-owner',error=None,previous=None,target=str(target))
        path=home/'updates/operations'/f'{ident}.json';atomic(path,op);atomic(home/'updates/active.json',{'id':ident});return path
    class NoRedirect(urllib.request.HTTPRedirectHandler):
        def redirect_request(self,*args): return None
    cookie=None
    def settings():
        # what Settings · 关于 reads, as the instance owner (the first local login owns the instance)
        jar=cookie
        if not jar:
            try: urllib.request.build_opener(NoRedirect).open(f'http://127.0.0.1:{port}/auth/dev',timeout=5)
            except urllib.error.HTTPError as e: jar=e.headers['Set-Cookie'].split(';')[0]
        req=urllib.request.Request(f'http://127.0.0.1:{port}/api/updates',headers={'Cookie':jar})
        with urllib.request.urlopen(req,timeout=5) as r: return jar,json.load(r)
    def phase(path,wanted):
        value=json.loads(path.read_text());return value if value['phase'] in wanted else None
    def kill_group():
        if process and process.poll() is None:
            os.killpg(process.pid,signal.SIGKILL);process.wait(timeout=10)
    try:
        process=start();eventually(lambda:health(old_release),'old actual Hub ready')
        db=home/'data/steve.sqlite3'
        with sqlite3.connect(db) as conn:
            observed=schema_gate.state(db)
            assert observed['kind']=='current',observed
            conn.execute('CREATE TABLE upgrade_smoke_evidence(value TEXT)');conn.execute("INSERT INTO upgrade_smoke_evidence VALUES('preserve')")
        new=bundle(new_release);job=operation(new_release['identity']['version'],new)
        result=eventually(lambda:phase(job,['succeeded','failed','rolled_back']),'successful upgrade')
        assert result['phase']=='succeeded',result
        assert health(new_release);assert (home/'current').resolve()==new.resolve()
        cookie,seen=settings()
        assert seen['install']['kind']=='managed' and seen['install']['home']==str(home),seen['install']
        assert seen['current']==dict.fromkeys(('hub','web'),new_release['identity']['version']) and seen['operation']['phase']=='succeeded',seen
        assert seen['backup']['database'] and pathlib.Path(seen['backup']['database']).exists(),seen['backup']
        print('PASS actual Hub paired upgrade and post-commit health; settings read back managed install, both versions and the verified backup')
        mismatched=copy.deepcopy(candidate_release['identity']['storage']);mismatched['core']['target']+=100
        bad=bundle(candidate_release,label='storage-mismatch',storage=mismatched);job=operation(candidate_release['identity']['version'],bad)
        result=eventually(lambda:phase(job,['rolled_back','failed']),'schema gate rollback')
        assert result['phase']=='rolled_back',result
        eventually(lambda:health(new_release),'rollback actual Hub ready')
        with sqlite3.connect(db) as conn: assert conn.execute('SELECT value FROM upgrade_smoke_evidence').fetchone()[0]=='preserve'
        cookie,seen=settings()
        assert seen['operation']['phase']=='rolled_back' and seen['current']==dict.fromkeys(('hub','web'),new_release['identity']['version']),seen
        assert pathlib.Path(seen['backup']['database']).name==result['id']+'.sqlite3',seen['backup']
        assert pathlib.Path(seen['backup']['database']).resolve()==pathlib.Path(result['backup']).resolve() and pathlib.Path(result['backup']).is_file(),seen['backup']
        log.flush()
        assert 'candidate storage incompatible with manifest' in pathlib.Path(log.name).read_text(),'storage gate was not reached'
        assert list((home/'updates').glob(result['id']+'.failed.*.sqlite3')),'the failed database is kept as evidence'
        print('PASS actual Hub storage identity mismatch rejects; old database and paired bundle restored; settings name the backup and the kept failed database')
        # Crash a genuinely matching candidate, rather than relying on identity/storage
        # rejection to keep the gate open. Verify that the crash really preceded commit.
        recovery=bundle(candidate_release)
        job=operation(candidate_release['identity']['version'],recovery)
        eventually(lambda:phase(job,['verifying']),'matching candidate pre-commit crash window')
        kill_group()
        interrupted=json.loads(job.read_text())
        assert interrupted['phase']=='verifying',interrupted
        assert (home/'current').resolve()==recovery.resolve()
        assert pathlib.Path(interrupted['backup']).is_file(),interrupted
        def instance_available():
            with open(home/'updates/instance.lock','a') as f:
                fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB);fcntl.flock(f,fcntl.LOCK_UN);return True
        eventually(instance_available,'orphan candidate releases instance lock',20)
        process=start();eventually(lambda:health(new_release),'supervisor restart recovery')
        result=eventually(lambda:phase(job,['rolled_back','failed']),'rollback journal reaches terminal state')
        assert result['phase']=='rolled_back' and 'before health commit' in result['error'],result
        assert (home/'current').resolve()==new.resolve()
        with sqlite3.connect(db) as conn: assert conn.execute('SELECT value FROM upgrade_smoke_evidence').fetchone()[0]=='preserve'
        cookie,seen=settings()
        assert seen['operation']['phase']=='rolled_back' and seen['current']==dict.fromkeys(('hub','web'),new_release['identity']['version']),seen
        assert pathlib.Path(seen['backup']['database']).resolve()==pathlib.Path(interrupted['backup']).resolve() and pathlib.Path(interrupted['backup']).is_file(),seen['backup']
        assert list((home/'updates').glob(result['id']+'.failed.*.sqlite3')),'crash recovery must keep the interrupted database'
        print('PASS matching candidate loses supervisor before commit and self-stops; restart restores backup and original version')
        # Kill only the parent while a fully booted Hub is serving; the managed child must self-stop.
        os.kill(process.pid,signal.SIGKILL);process.wait(timeout=10)
        eventually(instance_available,'normal managed child follows lost supervisor',20)
        process=start();eventually(lambda:health(new_release),'restart after normal parent-only crash')
        result=eventually(lambda:phase(job,['rolled_back','failed']),'terminal receipt after normal parent-only crash')
        assert result['phase']=='rolled_back',result
        # GET ready=false after DELETE means maintenance has been released by the restarted supervisor.
        token=json.loads((home/'updates/control-token.json').read_text())
        def maintenance_released():
            req=urllib.request.Request(f'http://127.0.0.1:{port}/internal/upgrade/drain',headers={'Authorization':'Bearer '+token})
            with urllib.request.urlopen(req,timeout=2) as r: return json.load(r).get('maintenance') is False
        eventually(maintenance_released,'maintenance released after parent-only crash')
        print('PASS normal managed Hub exits after supervisor-only crash; new supervisor starts safely')
        print('Temporary STEVE_HOME only; no GitHub publication, production restart, or agent task execution. Storage:',observed)
    except Exception:
        log.flush();log.seek(0);print(log.read()[-14000:],file=sys.stderr);raise
    finally:
        if process and process.poll() is None:
            process.terminate()
            try:process.wait(timeout=20)
            except subprocess.TimeoutExpired:kill_group()
        log.close()
