#!/usr/bin/env python3
"""Shared SQLite/candidate matrix used by the Python gate, native installer and packager."""
import copy
import hashlib
import json
from contextlib import closing
import pathlib
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import schema_gate

MATRIX = pathlib.Path(__file__).resolve().parents[1] / 'hub-rs/crates/steve-install/tests/storage_matrix.json'

def fixtures():
    data = json.loads(MATRIX.read_text())
    for case in data['cases']:
        manifest = copy.deepcopy(data['manifest'])
        for path, value in case.get('patch', {}).items():
            keys = path.strip('/').split('/')
            obj = manifest
            for key in keys[:-1]: obj = obj[key]
            if value is None: obj.pop(keys[-1])
            else: obj[keys[-1]] = value
        yield case, data.get(case['base'] + '_sql', '') + case['sql'], manifest

class Gate(unittest.TestCase):
    def test_shared_matrix_readonly_and_old_wrapper_never_grants_permission(self):
        for case, sql, manifest in fixtures():
            with self.subTest(case=case['name']), tempfile.TemporaryDirectory() as temp:
                root=pathlib.Path(temp); db=root/'odd ?#% 数据.sqlite3'
                if not case.get('missing'):
                    with closing(sqlite3.connect(db)) as c: c.executescript(sql)
                (root/'release.json').write_text(json.dumps(manifest))
                before=db.read_bytes() if db.exists() else None
                why=schema_gate.check(db,root)
                self.assertEqual(why is None,case['allowed'],why)
                if 'state' in case: self.assertEqual(schema_gate.state(db),case['state'])
                self.assertEqual(db.read_bytes() if db.exists() else None,before)

    def test_cli_refuses_partial_metadata_without_traceback_or_writes(self):
        with tempfile.TemporaryDirectory() as temp:
            root=pathlib.Path(temp); db=root/'db'
            with closing(sqlite3.connect(db)) as c: c.execute('CREATE TABLE meta(v INTEGER)')
            before=hashlib.sha256(db.read_bytes()).hexdigest()
            result=subprocess.run([sys.executable,str(pathlib.Path(schema_gate.__file__)), 'state',str(db)],capture_output=True,text=True)
            self.assertEqual(result.returncode,1)
            self.assertNotIn('Traceback',result.stderr)
            self.assertEqual(hashlib.sha256(db.read_bytes()).hexdigest(),before)

    def test_every_range_uses_same_inclusive_predicate(self):
        for version in (0,1,2,3,4):
            self.assertEqual(schema_gate.accepts(dict(min_read=1,target=3),version),1<=version<=3)

    def test_source_release_manifest_writer_uses_binary_storage_and_full_old_shell(self):
        source = pathlib.Path(__file__).with_name('release.sh').read_text()
        writer = source.split("<<'IDENTITY'\n", 1)[1].split('\nIDENTITY', 1)[0]
        data = json.loads(MATRIX.read_text())
        for valid in (True, False):
            with self.subTest(valid=valid), tempfile.TemporaryDirectory() as temp:
                root = pathlib.Path(temp)
                (root/'ops').mkdir()
                shutil.copyfile(schema_gate.__file__, root/'ops/schema_gate.py')
                info = dict(version='0.2.0', commit='a'*40, schema=114)
                if valid: info['storage'] = data['manifest']['storage']
                command = [sys.executable, '-I', '-', str(root), '0.2.0', 'a'*40,
                           'release-fixture', '2026-01-01', 'rust', 'b'*64, json.dumps(info)]
                result = subprocess.run(command, input=writer, capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, valid, result.stderr)
                if valid:
                    manifest = json.loads((root/'release.json').read_text())
                    self.assertEqual(manifest['storage'], info['storage'])
                    self.assertEqual(manifest['schema'], dict(min_read=113, max_read=114, target=114))
                    self.assertEqual((manifest['format_version'], manifest['api_protocol'], manifest['assets']), (1, 1, []))
                else:
                    self.assertFalse((root/'release.json').exists())

class MigrationLocks(unittest.TestCase):
    legacy='hub-rs/crates/steve-store-sqlite/legacy/migrations'
    old='hub-rs/crates/steve-store/migrations'
    schema='hub-rs/crates/steve-store-sqlite/schema'
    def git(self,root,*args):
        return subprocess.run(['git','-c','user.name=Fixture','-c','user.email=fixture@example.invalid',
                               '-c','commit.gpgSign=false',*args],cwd=root,check=True,capture_output=True,text=True)
    def directory(self,root,name,files):
        path=root/name;path.mkdir(parents=True,exist_ok=True)
        lines=[]
        for file,sql in files.items():
            (path/file).write_text(sql);lines.append(f'{file} {hashlib.sha256(sql.encode()).hexdigest()}')
        (path/'LOCK').write_text('\n'.join(lines)+'\n')
    def fixture(self,archived=False,previous114=False):
        temp=tempfile.TemporaryDirectory();self.addCleanup(temp.cleanup);root=pathlib.Path(temp.name)
        self.git(root,'init','-q')
        files={'113.sql':'-- published 113\n'}
        if previous114:files['114.sql']='-- published 114\n'
        self.directory(root,self.legacy if archived else self.old,files)
        if archived:
            for domain in ('core','stream','comms','contract'):
                self.directory(root,self.schema+'/'+domain,{'0001.sql':'-- baseline '+domain+'\n'})
        self.git(root,'add','.');self.git(root,'commit','-qm','published migration chain');self.git(root,'tag','v0.1.0')
        if not archived:shutil.rmtree(root/self.old)
        self.directory(root,self.legacy,{'113.sql':'-- published 113\n'})
        for domain in ('core','stream','comms','contract'):
            self.directory(root,self.schema+'/'+domain,{'0001.sql':'-- baseline '+domain+'\n'})
        (root/'ops').mkdir();shutil.copyfile(pathlib.Path(__file__).with_name('check-migration-lock.sh'),root/'ops/check-migration-lock.sh')
        self.git(root,'remote','add','origin','.')
        return root
    def check(self,root,allowed):
        result=subprocess.run(['bash','ops/check-migration-lock.sh'],cwd=root,capture_output=True,text=True)
        self.assertEqual(result.returncode==0,allowed,result.stdout+result.stderr)
    def test_first_archive_is_the_published_old_chain_and_old_copy_is_not_required(self):
        root=self.fixture();self.check(root,True)
        self.directory(root,self.legacy,{'113.sql':'-- published 113\n','114.sql':'-- pending 114\n'})
        self.check(root,True)
    def test_first_archive_cannot_rehash_changed_sql_or_omit_published_114(self):
        root=self.fixture();self.directory(root,self.legacy,{'113.sql':'-- forged\n'});self.check(root,False)
        root=self.fixture(previous114=True);self.check(root,False)
    def test_published_archive_and_lock_are_byte_frozen(self):
        for kind in ('sql','lock','append'):
            with self.subTest(kind=kind):
                root=self.fixture(archived=True)
                if kind=='sql':self.directory(root,self.legacy,{'113.sql':'-- rehashed edit\n'})
                elif kind=='lock':
                    p=root/self.legacy/'LOCK';p.write_text('# changed archive\n'+p.read_text())
                else:self.directory(root,self.legacy,{'113.sql':'-- published 113\n','114.sql':'-- late\n'})
                self.check(root,False)
    def test_missing_extra_duplicate_and_hash_changed_are_refused_in_every_directory(self):
        for directory in (self.legacy,*(self.schema+'/'+d for d in ('core','stream','comms','contract'))):
            for kind in ('missing-lock','missing-sql','extra','duplicate','hash'):
                with self.subTest(directory=directory,kind=kind):
                    root=self.fixture();path=root/directory;file='113.sql' if directory==self.legacy else '0001.sql'
                    if kind=='missing-lock':(path/'LOCK').unlink()
                    elif kind=='missing-sql':(path/file).unlink()
                    elif kind=='extra':(path/'9999.sql').write_text('-- extra')
                    elif kind=='duplicate':
                        p=path/'LOCK';p.write_text(p.read_text()*2)
                    else:(path/file).write_text('-- changed without LOCK')
                    self.check(root,False)
    def test_domains_append_but_cannot_rehash_a_released_prefix(self):
        root=self.fixture(archived=True)
        self.directory(root,self.schema+'/core',{'0001.sql':'-- baseline core\n','0002_next.sql':'-- next\n'})
        self.check(root,True)
        self.directory(root,self.schema+'/core',{'0001.sql':'-- forged baseline\n'})
        self.check(root,False)
    def test_001_through_112_are_outside_the_retained_guard(self):
        root=self.fixture();path=root/self.legacy
        (path/'001.sql').write_text('-- no longer guarded')
        p=path/'LOCK';p.write_text('001.sql ignored-history\n'+p.read_text())
        self.check(root,True)
    def test_missing_provenance_or_unversioned_directory_cannot_be_skipped(self):
        root=self.fixture();self.git(root,'tag','-d','v0.1.0');self.check(root,False)
        root=self.fixture();shutil.rmtree(root/self.legacy);self.check(root,False)

if __name__=='__main__': unittest.main()
