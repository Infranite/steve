#!/usr/bin/env python3
"""Isolated actual-Hub smoke test. Usage: python3 ops/test-upgrade-runtime.py /absolute/steve-hub
Uses a disposable home/key directory and loopback port; never touches a running installation.
"""
import fcntl, json, os, pathlib, shutil, signal, socket, sqlite3, subprocess, sys, tempfile, time, urllib.request, uuid

binary = pathlib.Path(sys.argv[1]).resolve(strict=True)

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
    root=pathlib.Path(temp); home=root/'home'; home.mkdir(); keys=root/'keys'; keys.mkdir(mode=0o700)
    sock=socket.socket();sock.bind(('127.0.0.1',0));port=sock.getsockname()[1];sock.close()
    env={**os.environ,'STEVE_HOME':str(home),'STEVE_HOST':'127.0.0.1','STEVE_PORT':str(port),'STEVE_PUBLIC_URL':f'http://127.0.0.1:{port}', 'STEVE_VAULT_KEYSTORE':'dir','STEVE_VAULT_KEY_DIR':str(keys),'STEVE_NO_RESUME':'1','STEVE_ADAPTER_CLAUDE':'/nonexistent','STEVE_ADAPTER_CODEX':'/nonexistent','CODEX_HOME':str(root/'codex'),'STEVE_REPO':str(root/'repo')}
    # Avoid inherited production auth/channels/external integrations in this isolated fixture.
    for key in list(env):
        if key.startswith(('STEVE_OIDC_', 'LARK_', 'FEISHU_', 'GITHUB_', 'STEVE_UPGRADE_', 'STEVE_KEYHELPER')): env.pop(key)
    def bundle(version,schema):
        path=home/'releases'/version;(path/'bin').mkdir(parents=True);(path/'web/dist').mkdir(parents=True)
        shutil.copy2(binary,path/'bin/steve-hub')
        atomic(path/'release.json',dict(format_version=1,version=version,commit='a'*40,hub_version=version,web_version=version,api_protocol=1,schema=dict(min_read=schema,max_read=schema,target=schema),assets=[]))
        atomic(path/'web/dist/release.json',{'version':version});(path/'web/dist/index.html').write_text('<html>upgrade fixture</html>')
        return path
    old=bundle('0.1.0',0);(home/'current').symlink_to(old)
    log=open(root/'runtime.log','w+')
    process=None
    def start():
        return subprocess.Popen([str(binary),'--supervise'],env=env,stdout=log,stderr=log,start_new_session=True)
    def health(version):
        with urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz',timeout=1) as r: data=json.load(r)
        return data if data.get('version')==version and data.get('web_version')==version and not data.get('maintenance') else None
    def operation(version,target):
        ident=str(uuid.uuid4());now=int(time.time()*1000)
        op=dict(id=ident,idempotency_key='smoke:'+ident,version=version,phase='ready',created_at=now,updated_at=now,requested_by='smoke-owner',error=None,previous=None,target=str(target))
        path=home/'updates/operations'/f'{ident}.json';atomic(path,op);atomic(home/'updates/active.json',{'id':ident});return path
    def phase(path,wanted):
        value=json.loads(path.read_text());return value if value['phase'] in wanted else None
    def kill_group():
        if process and process.poll() is None:
            os.killpg(process.pid,signal.SIGKILL);process.wait(timeout=10)
    try:
        process=start();eventually(lambda:health('0.1.0'),'old actual Hub ready')
        db=home/'data/steve.sqlite3'
        with sqlite3.connect(db) as conn:
            schema=conn.execute('SELECT v FROM schema_version').fetchone()[0]
            conn.execute('CREATE TABLE upgrade_smoke_evidence(value TEXT)');conn.execute("INSERT INTO upgrade_smoke_evidence VALUES('preserve')")
        new=bundle('0.2.0',schema);job=operation('0.2.0',new)
        result=eventually(lambda:phase(job,['succeeded','failed','rolled_back']),'successful upgrade')
        assert result['phase']=='succeeded',result
        assert health('0.2.0');assert (home/'current').resolve()==new.resolve()
        print('PASS actual Hub paired upgrade and post-commit health')
        bad=bundle('0.3.0',schema+100);job=operation('0.3.0',bad)
        result=eventually(lambda:phase(job,['rolled_back','failed']),'schema gate rollback')
        assert result['phase']=='rolled_back',result
        eventually(lambda:health('0.2.0'),'rollback actual Hub ready')
        with sqlite3.connect(db) as conn: assert conn.execute('SELECT value FROM upgrade_smoke_evidence').fetchone()[0]=='preserve'
        print('PASS actual Hub incompatible-schema gate rejects; old database and paired bundle restored')
        # Kill the complete isolated process group inside the persisted pre-commit window.
        job=operation('0.3.0',bad);eventually(lambda:phase(job,['verifying']),'pre-commit crash window',10);kill_group()
        def instance_available():
            with open(home/'updates/instance.lock','a') as f:
                fcntl.flock(f,fcntl.LOCK_EX|fcntl.LOCK_NB);fcntl.flock(f,fcntl.LOCK_UN);return True
        eventually(instance_available,'orphan candidate releases instance lock',20)
        process=start();eventually(lambda:health('0.2.0'),'supervisor restart recovery')
        result=eventually(lambda:phase(job,['rolled_back','failed']),'rollback journal reaches terminal state')
        assert result['phase']=='rolled_back',result
        with sqlite3.connect(db) as conn: assert conn.execute('SELECT value FROM upgrade_smoke_evidence').fetchone()[0]=='preserve'
        print('PASS supervisor and child crash before commit; restart restores backup and original version')
        # Kill only the parent while a fully booted Hub is serving; the managed child must self-stop.
        os.kill(process.pid,signal.SIGKILL);process.wait(timeout=10)
        eventually(instance_available,'normal managed child follows lost supervisor',20)
        process=start();eventually(lambda:health('0.2.0'),'restart after normal parent-only crash')
        result=eventually(lambda:phase(job,['rolled_back','failed']),'terminal receipt after normal parent-only crash')
        assert result['phase']=='rolled_back',result
        # GET ready=false after DELETE means maintenance has been released by the restarted supervisor.
        token=json.loads((home/'updates/control-token.json').read_text())
        def maintenance_released():
            req=urllib.request.Request(f'http://127.0.0.1:{port}/internal/upgrade/drain',headers={'Authorization':'Bearer '+token})
            with urllib.request.urlopen(req,timeout=2) as r: return json.load(r).get('maintenance') is False
        eventually(maintenance_released,'maintenance released after parent-only crash')
        print('PASS normal managed Hub exits after supervisor-only crash; new supervisor starts safely')
        print('Temporary STEVE_HOME only; no GitHub publication, production restart, or agent task execution. Schema:',schema)
    except Exception:
        log.flush();log.seek(0);print(log.read()[-14000:],file=sys.stderr);raise
    finally:
        if process and process.poll() is None:
            process.terminate()
            try:process.wait(timeout=20)
            except subprocess.TimeoutExpired:kill_group()
        log.close()
