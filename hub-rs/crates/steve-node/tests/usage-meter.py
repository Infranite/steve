#!/usr/bin/env python3
"""Synthetic ACP meter: real processes/files, deterministic counts, no model or credentials."""
import datetime
import json
import os
from pathlib import Path
import sys
import threading
import time

lock = threading.Lock()
sid = 'thread-' + str(os.getpid())
path = Path(os.environ['CODEX_HOME']) / 'sessions/2026/01/01' / ('rollout-test-' + sid + '.jsonl')
total = [10000, 4000, 1000, 200]
cancelled = threading.Event()
fields = ['input_tokens', 'cached_input_tokens', 'output_tokens', 'reasoning_output_tokens']


def send(value):
    with lock:
        print(json.dumps(value), flush=True)


def tick(delta, duplicate=False, reset=False):
    global total
    total = list(delta) if reset else [a + b for a, b in zip(total, delta)]
    row = {'timestamp': datetime.datetime.now(datetime.timezone.utc).isoformat(timespec='milliseconds').replace('+00:00', 'Z'),
           'type': 'event_msg', 'payload': {'type': 'token_count', 'info': {
               'total_token_usage': dict(zip(fields, total)), 'last_token_usage': dict(zip(fields, delta))}}}
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('a') as f:
        line = json.dumps(row) + '\n'
        f.write(line)
        if duplicate:
            f.write(line)


def prompt(msg):
    text = msg['params']['prompt'][0]['text']
    if text == 'stop-stubborn':
        import subprocess
        child = path.with_suffix('.child')
        subprocess.Popen([sys.executable, '-c', "import os,signal,time;from pathlib import Path;signal.signal(signal.SIGTERM,signal.SIG_IGN);Path(" + repr(str(child)) + ").write_text(str(os.getpid()));time.sleep(60)"])
        while not child.exists():
            time.sleep(0.002)
    if text != 'missing':
        tick([0, 0, 0, 0] if text == 'zero' else [100, 60, 20, 5], duplicate=True)
    send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': 'unrelated-session', 'update': {
        'sessionUpdate': 'usage_update', 'used': 777, 'size': 1000000, 'cost': {'amount': 999, 'currency': 'USD'}}}})
    for _ in range(2):
        send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': sid, 'update': {
            'sessionUpdate': 'usage_update', 'used': 999999, 'size': 1000000}}})
    if text.startswith('stop-'):
        # The test persists its tail with sync_all, then asks this live process to trigger a
        # stop condition before the next sampler tick. No UsageReport/bill is injected.
        control = path.with_suffix('.control')
        heartbeat = time.monotonic()
        while not control.exists():
            if time.monotonic() - heartbeat > 0.05:
                send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': sid,
                      'update': {'sessionUpdate': 'available_commands_update', 'availableCommands': []}}})
                heartbeat = time.monotonic()
            time.sleep(0.002)
        action = control.read_text()
        if action in ('output', 'turns'):
            # output: one message past the test's 256 KiB budget, under the 1 MiB frame limit
            chunks = [('one', 2000), ('two', 2000)] if action == 'turns' else [('one', 270000)]
            for message, size in chunks:
                send({'jsonrpc': '2.0', 'method': 'session/update', 'params': {'sessionId': sid,
                      'update': {'sessionUpdate': 'agent_message_chunk', 'messageId': message,
                                 'content': {'type': 'text', 'text': 'x' * size}}}})
        elif action in ('tools', 'policy'):
            for tool in (['first', 'second'] if action == 'tools' else ['first']):
                send({'jsonrpc': '2.0', 'id': 'stop-permission-' + tool, 'method': 'session/request_permission', 'params': {
                    'sessionId': sid, 'toolCall': {'toolCallId': 'new-tool-' + tool, 'title': 'Read' if action == 'tools' else 'Write',
                    'kind': 'read' if action == 'tools' else 'edit', 'status': 'pending',
                    'rawInput': {'file_path': str(Path.cwd() / 'file.txt')}},
                    'options': [{'optionId': 'yes', 'kind': 'allow_once', 'name': 'Allow'}]}})
        # A stop must never execute a later permission or start a second prompt.
        cancelled.wait(60)
        return
    if text in ('live', 'cancel', 'hang', 'native'):
        cancelled.wait(60 if text == 'hang' else 1.1)
    if text == 'reset':
        tick([50, 20, 10, 2], duplicate=True, reset=True)
        tick([30, 10, 5, 1], duplicate=True)
    response = {'stopReason': 'cancelled' if cancelled.is_set() else 'end_turn'}
    if text == 'native':
        response['usage'] = {'inputTokens': 40, 'cachedReadTokens': 60, 'cachedWriteTokens': 7,
                             'outputTokens': 20, 'thoughtTokens': 5}
        response['_meta'] = {'quota': {'model_usage': [{'model': 'meter-model', 'token_count': {
            'inputTokens': 40, 'cachedInputTokens': 60, 'cacheWriteInputTokens': 7,
            'outputTokens': 20, 'reasoningOutputTokens': 5}}]}}
    if text in ('late', 'error', 'cancel'):
        def tail():
            time.sleep(0.3)
            tick([50, 20, 10, 2], duplicate=True)
        threading.Thread(target=tail, daemon=True).start()
    if text == 'error':
        send({'jsonrpc': '2.0', 'id': msg['id'], 'error': {'code': -32603, 'message': 'synthetic provider error'}})
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
        path = path.with_name('rollout-test-' + sid + '.jsonl')
        tick([0, 0, 0, 0])  # a large lifetime total exists before the attempt's baseline
        result = {'sessionId': sid, 'modes': {'currentModeId': 'default', 'availableModes': [{'id': 'default'}, {'id': 'plan'}]}}
    elif method == 'session/prompt':
        if msg['params']['prompt'][0]['text'] == 'stop-stubborn':
            import signal
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
        cancelled.clear()
        threading.Thread(target=prompt, args=(msg,), daemon=True).start()
        continue
    elif method == 'session/cancel':
        path.with_suffix('.cancelled').write_text('cancel received')
        cancelled.set()
    if 'id' in msg:
        send({'jsonrpc': '2.0', 'id': msg['id'], 'result': result})
