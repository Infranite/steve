#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
keys = ['GIT_DIR','GIT_CONFIG_GLOBAL','GIT_CONFIG_PARAMETERS','GIT_CONFIG_COUNT',
        'GIT_ALLOW_PROTOCOL','GIT_TERMINAL_PROMPT','GIT_ASKPASS','SSH_ASKPASS_REQUIRE',
        'GIT_TRACE','GIT_CURL_VERBOSE','GIT_TRACE2','GIT_TRACE2_EVENT','GIT_WORK_TREE']
pathlib.Path(os.environ['SG34_CLONE_LOG']).write_text(json.dumps({'args': args, 'env': {k:os.environ.get(k) for k in keys}}))
case = os.environ['SG34_CLONE_CHILD']
sys.stderr.write('Receiving objects: 25% (1/4)\rReceiving objects: 25% (1/4)\rResolving deltas: 100%\nUpdating files: 100%\n')
if case in ['auth','network']:
    sys.stderr.write('fatal: '+('Authentication failed' if case=='auth' else 'Could not resolve host')+' SG34 raw diagnostic\n')
    sys.exit(128)
repo = pathlib.Path(args[-1])
(repo/'.git').mkdir(parents=True)
branch = args[args.index('--branch')+1] if '--branch' in args else 'main'
(repo/'.git'/'HEAD').write_text('detached\n' if case=='detached' else 'ref: refs/heads/'+branch+'\n')
(repo/'kept').write_text('complete')
if case=='broken':
    sys.stderr.write('warning: remote HEAD refers to nonexistent ref, unable to checkout\n')
