"""Two independent, lossless SSE readers; no reconnect can hide a recording gap."""
import http.client
import json
import socket
import threading
import time
from urllib.parse import urlencode


class Recorder:
    def __init__(self, port, cookie, workspaces, output, cursor):
        self.lock = threading.Lock()
        self.step = 'connect'
        self.last = time.monotonic()
        self.errors, self.readers = [], []
        self.stopping = False
        for name, ws in workspaces.items():
            conn = http.client.HTTPConnection('127.0.0.1', port, timeout=60)
            conn.request('GET', '/api/stream?' + urlencode({'lang': 'en', 'ws': ws, 'after': cursor}),
                         headers={'Cookie': cookie, 'Accept': 'text/event-stream'})
            response = conn.getresponse()
            if response.status != 200:
                raise RuntimeError(f'SSE {name}: HTTP {response.status}')
            target = (output / f'{name}.jsonl').open('x', encoding='utf-8')
            thread = threading.Thread(target=self.read, args=(response, target), daemon=True)
            self.readers.append((conn, response, target, thread))
            thread.start()

    def read(self, response, target):
        try:
            fields, data = {}, []
            while raw := response.readline():
                line = raw.decode('utf-8').rstrip('\r\n')
                if not line:
                    if data:
                        with self.lock:
                            row = {'step': self.step, 'id': fields.get('id'),
                                   'event': fields.get('event', 'message'), 'data': '\n'.join(data)}
                            target.write(json.dumps(row, ensure_ascii=False) + '\n')
                            target.flush()
                            self.last = time.monotonic()
                    fields, data = {}, []
                elif not line.startswith(':'):
                    key, _, value = line.partition(':')
                    value = value.removeprefix(' ')
                    if key == 'data':
                        data.append(value)
                    else:
                        fields[key] = value
            if not self.stopping:
                self.errors.append('SSE ended before scenario completed')
        except Exception as error:
            if not self.stopping:
                self.errors.append(str(error))

    def quiet(self, minimum=0.6):
        start = time.monotonic()
        while True:
            with self.lock:
                if self.errors:
                    raise RuntimeError(self.errors)
                now = time.monotonic()
                if now - start >= minimum and now - self.last >= 0.4:
                    return
            if now - start > 30:
                raise RuntimeError('SSE never became quiet')
            time.sleep(0.03)

    def label(self, name):
        with self.lock:
            self.step = name

    def close(self):
        self.stopping = True
        for conn, response, target, thread in self.readers:
            try:
                response.fp.raw._sock.shutdown(socket.SHUT_RDWR)
            except (AttributeError, OSError):
                pass
            conn.close()
            thread.join(3)
            target.close()
            if thread.is_alive():
                raise RuntimeError('SSE reader did not stop')
