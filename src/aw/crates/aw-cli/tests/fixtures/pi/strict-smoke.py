#!/usr/bin/env python3
"""Strict tool output through the real Pi SDK, HTTP model and sec-core Provider."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import threading
import time


def call(argv, cwd, env, timeout=45):
    with subprocess.Popen(list(map(str, argv)), cwd=cwd, env=env, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          start_new_session=True) as process:
        try:
            out, err = process.communicate(timeout=timeout)
            return process.returncode, out, err
        finally:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--aw', type=Path, required=True)
    parser.add_argument('--root', type=Path, required=True, help='locked npm installation')
    parser.add_argument('--sec-core-provider', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent
    results = []
    for scenario in ['clean', 'risk', 'error', 'invalid', 'timeout', 'metadata', 'extra',
                     'tool-error', 'native-error', 'cancel', 'after-metadata', 'after-extra',
                     'mutation-content', 'mutation-metadata', 'mutation-extra', 'mutation-after']:
        with tempfile.TemporaryDirectory(prefix='aw-pi-strict-') as temporary:
            root = Path(temporary)
            (root / 'node_modules').symlink_to(args.root.resolve() / 'node_modules')
            for filename in ['submission.mjs', 'submission-native.mjs', 'strict-native.mjs', 'strict-scanner.py']:
                shutil.copy(fixture / filename, root / filename)
            requests, forbidden = [], []
            class Model(BaseHTTPRequestHandler):
                def log_message(self, *_args):
                    pass

                def do_POST(self):
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    requests.append(body)
                    encoded = json.dumps(body)
                    if any(text in encoded for text in ['PRIVATE_OUTPUT_SECRET']):
                        forbidden.append(body)
                        self.send_error(500, 'rejected input reached model')
                        return
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.end_headers()
                    assert self.path == '/v1/responses', self.path
                    tool = body['input'][-1].get('role') == 'user'
                    item = ({'type': 'function_call', 'id': 'fc_fixture', 'call_id': 'reused-tool-id',
                             'name': 'test', 'arguments': '{"command":"printf safe"}', 'status': 'completed'}
                            if tool else {'type': 'message', 'id': 'msg_fixture', 'role': 'assistant',
                                          'status': 'completed', 'content': [{'type': 'output_text', 'text': 'OK', 'annotations': []}]})
                    events = [
                        {'type': 'response.created', 'response': {'id': 'resp_fixture', 'status': 'in_progress'}},
                        {'type': 'response.output_item.added', 'output_index': 0, 'item': item},
                        {'type': 'response.output_item.done', 'output_index': 0, 'item': item},
                        {'type': 'response.completed', 'response': {'id': 'resp_fixture', 'status': 'completed',
                            'output': [item], 'usage': {'input_tokens': 1, 'output_tokens': 1, 'total_tokens': 2}}},
                    ]
                    for event in events:
                        self.wfile.write(('event: ' + event['type'] + '\ndata: ' + json.dumps(event) + '\n\n').encode())
                    self.wfile.flush()

            server = ThreadingHTTPServer(('127.0.0.1', 0), Model)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            env = {key: value for key, value in os.environ.items() if not key.startswith('AW_')}
            env.update(FAKE_ROOT=str(root), HOME=str(root), PI_TEST_ROOT=str(root),
                       PI_STRICT_CASE=scenario, PI_MODEL_URL=f'http://127.0.0.1:{server.server_port}/v1')
            config = {'apiVersion': 'aw/v1alpha1', 'kind': 'AWConfiguration', 'metadata': {'name': 'pi-strict'}, 'spec': {
                'daemon': {'startup': 'on_demand', 'endpoint': 'auto', 'state_dir': str(root / 'state')},
                'execution': {'guarantee': 'native_hook', 'default_event_budget_ms': 8000},
                'audit': {'enabled': True, 'payload': 'metadata_only'},
                'agents': {'pi': {'adapter': 'pi', 'argv': [shutil.which('node'), str(root / 'strict-native.mjs')]}},
                'providers': {'security': {'protocol': 'aw-provider/v1alpha1',
                    'transport': {'type': 'stdio', 'location': 'agent', 'argv': [str(args.sec_core_provider.resolve()),
                        '--cli', str(root / 'strict-scanner.py'), '--socket', str(root / 'scanner.sock')]},
                    'timeout_ms': 1500, 'max_output_bytes': 65536,
                    'config': {'version': 1, 'mode': 'block', 'pii': {'input_mode': 'observe', 'output_mode': 'replace_result'}}}},
                'events': {'tool.after': {'enabled': True, 'required': True, 'steps': [
                    {'id': 'pii', 'provider': 'security', 'operation': 'scan_tool_output',
                     'effects': ['observe', 'replace_result'], 'on_error': 'withhold_result'}]}}}}
            path = root / 'aw.json'
            path.write_text(json.dumps(config))
            aw = str(args.aw.resolve())
            try:
                code, out, err = call([aw, 'run', '--config', path, '--agent', 'pi'], root, env)
                (output / (scenario + '.stderr')).write_text(err)
                (output / (scenario + '-model.json')).write_text(json.dumps(requests, indent=2))
                assert not forbidden, ('unchecked output reached HTTP model', scenario, forbidden)
                assert code == 0, (scenario, out, err)
                shutil.copy(root / 'events.json', output / (scenario + '-events.json'))
                status = json.loads(call([aw, 'status', '--config', path], root, env)[1])['result']
                assert status['bindings'] == 0 and status['events'] == 0, status
                assert status['resources']['usage']['invocations'] == 0, status
                results.append({**json.loads(out), 'http_requests': len(requests), 'status': 'passed'})
            finally:
                try:
                    call([aw, 'stop', '--config', path], root, env, timeout=15)
                    deadline = time.monotonic() + 5
                    while (root / 'state/aw.sock').exists() and time.monotonic() < deadline:
                        time.sleep(.05)
                    assert not (root / 'state/aw.sock').exists(), 'AW socket survived stop'
                finally:
                    server.shutdown()
                    server.server_close()
                    thread.join(timeout=5)
                    assert not thread.is_alive()
    (output / 'results.json').write_text(json.dumps(results, indent=2))
    print(json.dumps({'passed': True, 'cases': len(results)}))


if __name__ == '__main__':
    main()
