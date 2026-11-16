#!/usr/bin/env python3
"""Protocol fixture for persisted Claude-shaped usage; no provider calls or credentials."""
import datetime
import json
import os
from pathlib import Path
import sys
import threading
import time

lock = threading.Lock()
cancelled = threading.Event()
sid = 'session-' + str(os.getpid())
home = Path(os.environ['CLAUDE_CONFIG_DIR'])
turn = 0


def send(value):
    with lock:
        print(json.dumps(value), flush=True)


def path():
    return home / 'projects' / 'fixture-project' / (sid + '.jsonl')


def record(mid, input=6, output=1112, write=30544, read=23457, model='source-model', child=False, source_stamp=None):
    row = {'timestamp': source_stamp or datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='milliseconds').replace('+00:00', 'Z'),
           'type': 'assistant', 'sessionId': sid, 'isSidechain': child,
           'message': {'id': mid, 'role': 'assistant', 'model': model, 'usage': {
               'input_tokens': input, 'cache_creation_input_tokens': write, 'cache_read_input_tokens': read,
               'output_tokens': output, 'output_tokens_details': {'thinking_tokens': min(19, output)}}}}
    p = path() if not child else path().with_suffix('') / 'subagents' / 'agent-one.jsonl'
    p.parent.mkdir(parents=True, exist_ok=True)
    with p.open('a') as f:
        f.write(json.dumps(row) + '\n')


def prompt(msg):
    global turn
    turn += 1
    mid = str(turn)
    text = msg['params']['prompt'][0]['text']
    if text != 'missing':
        for _ in range(3):
            record(mid, **({'input': 0, 'output': 0, 'write': 0, 'read': 0} if text == 'zero' else {}))
    if text == 'children':
        record(mid + '-child', input=2, output=7, write=0, read=0, child=True)
    if text == 'models':
        record(mid + '-other', input=2, output=7, write=0, read=0, model='other-model')
    if text in ('live', 'native', 'zero-final', 'children', 'models', 'cancel', 'hang'):
        cancelled.wait(60 if text in ('cancel', 'hang') else 1.2)
    response = {'stopReason': 'cancelled' if cancelled.is_set() else 'end_turn'}
    if text in ('late', 'error', 'cancel'):
        source_stamp = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='milliseconds').replace('+00:00', 'Z')
        def tail():
            time.sleep(0.3)
            record(mid + '-tail', input=2, output=7, write=0, read=0, source_stamp=source_stamp)
            record(mid + '-tail', input=2, output=7, write=0, read=0, source_stamp=source_stamp)
            record(mid + '-after-end', input=99999, output=99999)
            record(mid + '-synthetic', input=0, output=0, write=0, read=0, model='<synthetic>')
        threading.Thread(target=tail, daemon=True).start()
    if text in ('zero-final', 'native', 'cancel'):
        zero = text != 'native'
        response['usage'] = {'inputTokens': 0 if zero else 6, 'cachedReadTokens': 0 if zero else 23457,
                             'cachedWriteTokens': 0 if zero else 30544, 'outputTokens': 0 if zero else 1112}
        response['_meta'] = {'quota': {'model_usage': [{'model': 'source-model', 'token_count': response['usage']}]}}
    if text == 'error':
        send({'jsonrpc': '2.0', 'id': msg['id'], 'error': {'code': -32603, 'message': 'fixture provider error'}})
    else:
        send({'jsonrpc': '2.0', 'id': msg['id'], 'result': response})


for line in sys.stdin:
    msg = json.loads(line)
    method = msg.get('method')
    result = {}
    if method == 'initialize':
        result = {'protocolVersion': 1, 'agentCapabilities': {}}
    elif method in ('session/new', 'session/load'):
        sid = msg.get('params', {}).get('sessionId', sid)
        record('baseline', input=999999, output=999999)
        result = {'sessionId': sid, 'modes': {'currentModeId': 'default', 'availableModes': [{'id': 'default'}, {'id': 'plan'}]}}
    elif method == 'session/prompt':
        cancelled.clear()
        threading.Thread(target=prompt, args=(msg,), daemon=True).start()
        continue
    elif method == 'session/cancel':
        cancelled.set()
    if 'id' in msg:
        send({'jsonrpc': '2.0', 'id': msg['id'], 'result': result})
