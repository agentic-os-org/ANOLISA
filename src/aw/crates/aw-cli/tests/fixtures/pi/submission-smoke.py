#!/usr/bin/env python3
"""Real Pi SDK, AW and sec-core Provider with controlled HTTP model and CLI scanner."""
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
    parser.add_argument('--capacity', action='store_true', help='check 1024 denials and recovery in one session')
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent
    (output / 'results.json').unlink(missing_ok=True)
    results = []
    scenarios = [('normal', 'block'), ('error', 'block'), ('error', 'report'),
                             ('invalid', 'block'), ('invalid', 'report'),
                             ('timeout', 'block'), ('timeout', 'report'), ('abort', 'block'), ('dispose', 'block'),
                             ('tools-report', 'block'), ('transforms', 'block')]
    for scenario, action in ([('capacity', 'block')] if args.capacity else scenarios):
        name = scenario + '-' + action
        with tempfile.TemporaryDirectory(prefix='aw-pi-submit-') as temporary:
            root = Path(temporary)
            (root / 'node_modules').symlink_to(args.root.resolve() / 'node_modules')
            for filename in ['submission.mjs', 'submission-native.mjs', 'submission-scanner.py']:
                shutil.copy(fixture / filename, root / filename)
            requests, forbidden = [], []

            class Model(BaseHTTPRequestHandler):
                def log_message(self, *_args):
                    pass

                def do_POST(self):
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    requests.append(body)
                    encoded = json.dumps(body)
                    if any(text in encoded for text in ['api_key=synthetic-secret', '"risk"', '"cancel"']):
                        forbidden.append(body)
                        self.send_error(500, 'rejected input reached model')
                        return
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.end_headers()
                    assert self.path == '/v1/responses', self.path
                    tool = scenario == 'tools-report' and body['input'][-1].get('role') == 'user'
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
            env.update(FAKE_ROOT=str(root), HOME=str(root), PI_SUBMISSION_CASE=scenario,
                       PI_SUBMISSION_REPORT='1' if action == 'report' else '0',
                       PI_MODEL_URL=f'http://127.0.0.1:{server.server_port}/v1')
            config = {'apiVersion': 'aw/v1alpha1', 'kind': 'AWConfiguration', 'metadata': {'name': 'pi-submit'}, 'spec': {
                'daemon': {'startup': 'on_demand', 'endpoint': 'auto', 'state_dir': str(root / 'state')},
                'execution': {'guarantee': 'native_hook', 'default_event_budget_ms': 8000},
                'audit': {'enabled': True, 'payload': 'metadata_only'},
                'agents': {'pi': {'adapter': 'pi', 'argv': [shutil.which('node'), str(root / 'submission-native.mjs')]}},
                'providers': {'security': {'protocol': 'aw-provider/v1alpha1',
                    'transport': {'type': 'stdio', 'location': 'agent', 'argv': [str(args.sec_core_provider.resolve()), '--cli', str(root / 'submission-scanner.py'), '--socket', str(root / 'scanner.sock')]},
                    'timeout_ms': 5000 if scenario in ('abort', 'dispose') else 1500,
                    'max_output_bytes': 65536, 'config': {'version': 1, 'mode': 'block',
                        'prompt': {'mode': 'block', 'scan_mode': 'fast'},
                        'pii': {'input_mode': 'block', 'output_mode': 'observe'}}},
                    'record': {'protocol': 'aw-provider/v1alpha1',
                        'transport': {'type': 'stdio', 'location': 'agent',
                            'argv': [shutil.which('python3'), str(fixture / 'submission-policy.py'), str(root)]},
                        'timeout_ms': 500, 'max_output_bytes': 4096, 'config': {}}},
                'events': {'input.submit': {'enabled': True, 'required': True, 'steps': [
                    {'id': 'record', 'provider': 'record', 'operation': 'submit', 'effects': ['observe'], 'on_error': 'report'},
                    *[{'id': label, 'provider': 'security', 'operation': operation,
                       'effects': ['observe', 'block'], 'on_error': action}
                      for label, operation in [('prompt', 'scan_prompt'), ('pii', 'scan_user_input')]]]}}}}
            # The recorder only observes: scanner effects and faults must decide the outcome.
            recorder = root / 'record.py'
            recorder.write_text((fixture / 'submission-policy.py').read_text().replace(
                'text = event["input"]["text"]', 'text = "record-only"'))
            config['spec']['providers']['record']['transport']['argv'][1] = str(recorder)
            if scenario == 'tools-report':
                config['spec']['providers']['security']['config']['tools'] = {
                    'test': {'language': 'bash', 'input_pointer': '/command'}}
                config['spec']['providers']['after'] = {
                    'transport': {'type': 'stdio', 'location': 'agent',
                                  'argv': [shutil.which('python3'), '-c', 'import sys; sys.stdin.read(); sys.exit(7)']},
                    'timeout_ms': 500, 'max_output_bytes': 32}
                config['spec']['events']['tool.before'] = {'enabled': True, 'required': True, 'steps': [
                    {'id': 'scan-code', 'provider': 'security', 'operation': 'scan_code',
                     'effects': ['observe', 'block'], 'on_error': 'block'}]}
                config['spec']['events']['tool.after'] = {'enabled': True, 'required': True, 'steps': [
                    {'id': 'after', 'provider': 'after', 'effects': ['observe'], 'on_error': 'report'}]}
            path = root / 'aw.json'
            path.write_text(json.dumps(config))
            aw = str(args.aw.resolve())
            try:
                code, out, err = call([aw, 'run', '--config', path, '--agent', 'pi'], root, env, timeout=900 if args.capacity else 45)
                (output / (name + '.stderr')).write_text(err)
                (output / (name + '-model.json')).write_text(json.dumps(requests, ensure_ascii=False, indent=2))
                for filename in ['scanner.jsonl', 'submissions.jsonl']:
                    if (root / filename).exists():
                        shutil.copy(root / filename, output / (name + '-' + filename))
                assert not forbidden, ('REJECTED INPUT REACHED MODEL', name, forbidden)
                assert code == 0, (name, out, err)
                row = json.loads(out)
                assert row['passed'] and len(requests) == row['model_calls'] + (1 if scenario == 'normal' else 0), (name, row, requests)
                if scenario == 'transforms':
                    assert row['rejected_transforms'] == ['transformContext', 'convertToLlm', 'onPayload'], row
                    assert len(requests) == 1, ('rewritten input reached HTTP transport', requests)
                status = json.loads(call([aw, 'status', '--config', path], root, env)[1])['result']
                assert status['bindings'] == 0 and status['events'] == 0, status
                assert status['resources']['usage']['invocations'] == 0, status
                scanner_rows = [json.loads(line) for line in (root / 'scanner.jsonl').read_text().splitlines()]
                if scenario == 'tools-report':
                    assert row['submissions'] == 3, row
                    assert sum('scan-code' in item['argv'] for item in scanner_rows) == 2, scanner_rows
                for line in (root / 'scanner.jsonl').read_text().splitlines():
                    scanner = json.loads(line)
                    proc = Path(f"/proc/{scanner['pid']}/stat")
                    assert not proc.exists() or proc.read_text().rsplit(')', 1)[1].split()[19] != scanner['start'], scanner
                results.append({'case': name, **row, 'http_requests': len(requests), 'bindings': status['bindings'], 'events': status['events'],
                                'invocations': status['resources']['usage']['invocations'], 'status': status})
            finally:
                try:
                    stopped = call([aw, 'stop', '--config', path], root, env, timeout=15)
                    assert stopped[0] == 0, stopped
                    # Stop acknowledges shutdown before the listener is dropped.
                    deadline = time.monotonic() + 5
                    while (root / 'state/aw.sock').exists() and time.monotonic() < deadline:
                        time.sleep(0.05)
                    assert not (root / 'state/aw.sock').exists(), 'AW socket survived stop'
                finally:
                    server.shutdown()
                    server.server_close()
                    thread.join(timeout=5)
                    assert not thread.is_alive(), 'model endpoint did not stop'
    (output / 'results.json').write_text(json.dumps({'passed': True, 'cases': results}, indent=2))
    print(json.dumps({'passed': True, 'cases': len(results)}))


if __name__ == '__main__':
    main()
