#!/usr/bin/env python3
"""Actual Pi SDK + AW + sec-core Provider: checked Skill adoption and input scanning."""
import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import importlib.util
import json
import os
from pathlib import Path
import shutil
import tempfile
import threading
import time

fixture = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('submission_smoke', fixture / 'submission-smoke.py')
submission = importlib.util.module_from_spec(spec)
spec.loader.exec_module(submission)
call = submission.call


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ['aw', 'root', 'sec-core-provider', 'output']:
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    rows = []
    scenarios = ['signed', 'plain', 'unsigned', 'drifted', 'tampered', 'denied',
                 'error', 'changed', 'directory-swap', 'forged-manifest', 'missing-digest', 'cancel', 'prompt-denied', 'pii-denied']
    for scenario in scenarios:
        with tempfile.TemporaryDirectory(prefix='aw-pi-skill-') as temporary:
            root = Path(temporary)
            (root / 'node_modules').symlink_to(args.root.resolve() / 'node_modules')
            for name in ['submission-native.mjs', 'submission.mjs', 'skill-native.mjs', 'skill-scanner.py']:
                shutil.copy(fixture / name, root / name)
            skill = root / 'skills/fixture'
            skill.mkdir(parents=True)
            (skill / 'SKILL.md').write_text('---\nname: fixture\ndescription: fixture\n---\nCHECKED_SKILL_BODY\n')
            requests = []

            class Model(BaseHTTPRequestHandler):
                def log_message(self, *_args):
                    pass

                def do_POST(self):
                    body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                    requests.append(body)
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.end_headers()
                    item = {'type': 'message', 'id': 'msg_fixture', 'role': 'assistant', 'status': 'completed',
                            'content': [{'type': 'output_text', 'text': 'OK', 'annotations': []}]}
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
            env.update(FAKE_ROOT=str(root), HOME=str(root), PI_SKILL_CASE=scenario,
                       PI_MODEL_URL=f'http://127.0.0.1:{server.server_port}/v1')
            config = {'apiVersion': 'aw/v1alpha1', 'kind': 'AWConfiguration', 'metadata': {'name': 'pi-skill'}, 'spec': {
                'daemon': {'startup': 'on_demand', 'endpoint': 'auto', 'state_dir': str(root / 'state')},
                'execution': {'guarantee': 'native_hook', 'default_event_budget_ms': 8000},
                'audit': {'enabled': True, 'payload': 'metadata_only'},
                'agents': {'pi': {'adapter': 'pi', 'argv': [shutil.which('node'), str(root / 'skill-native.mjs')]}},
                'providers': {'security': {'protocol': 'aw-provider/v1alpha1',
                    'transport': {'type': 'stdio', 'location': 'agent', 'argv': [str(args.sec_core_provider.resolve()),
                        '--cli', str(root / 'skill-scanner.py'), '--socket', str(root / 'scanner.sock')]},
                    'timeout_ms': 5000, 'max_output_bytes': 65536, 'config': {'version': 1, 'mode': 'block', 'skill': True,
                        'prompt': {'mode': 'block', 'scan_mode': 'fast'},
                        'pii': {'input_mode': 'block', 'output_mode': 'observe'}}}},
                'events': {'input.submit': {'enabled': True, 'required': True, 'steps': [
                    {'id': operation, 'provider': 'security', 'operation': operation,
                     'effects': ['observe', 'block'],
                     'on_error': 'report' if operation == 'check_skill' and scenario in ['directory-swap', 'forged-manifest'] else 'block'}
                    for operation in ['check_skill', 'scan_prompt', 'scan_user_input']]}}}}
            path = root / 'aw.json'
            path.write_text(json.dumps(config))
            aw = str(args.aw.resolve())
            try:
                code, out, err = call([aw, 'run', '--config', path, '--agent', 'pi'], root, env)
                (args.output / (scenario + '.stderr')).write_text(err)
                (args.output / (scenario + '-model.json')).write_text(json.dumps(requests, indent=2))
                if (root / 'scanner.jsonl').exists():
                    shutil.copy(root / 'scanner.jsonl', args.output / (scenario + '-scanner.jsonl'))
                assert code == 0, (scenario, out, err)
                row = json.loads(out)
                assert row['passed'] and len(requests) == row['model_calls'], (scenario, row, requests)
                assert len(requests) == (1 if scenario in ['signed', 'plain'] else 0), (scenario, requests)
                status = json.loads(call([aw, 'status', '--config', path], root, env)[1])['result']
                assert status['bindings'] == 0 and status['events'] == 0, status
                assert status['resources']['usage']['invocations'] == 0, status
                for line in (root / 'scanner.jsonl').read_text().splitlines():
                    scanner = json.loads(line)
                    proc = Path(f"/proc/{scanner['pid']}/stat")
                    assert not proc.exists() or proc.read_text().rsplit(')', 1)[1].split()[19] != scanner['start'], scanner
                rows.append({'case': scenario, **row, 'bindings': 0, 'events': 0, 'invocations': 0})
            finally:
                try:
                    stopped = call([aw, 'stop', '--config', path], root, env, timeout=15)
                    assert stopped[0] == 0, stopped
                    deadline = time.monotonic() + 5
                    while (root / 'state/aw.sock').exists() and time.monotonic() < deadline:
                        time.sleep(0.05)
                    assert not (root / 'state/aw.sock').exists()
                finally:
                    server.shutdown()
                    server.server_close()
                    thread.join(timeout=5)
                    assert not thread.is_alive()
    (args.output / 'results.json').write_text(json.dumps({'passed': True, 'cases': rows}, indent=2))
    print(json.dumps({'passed': True, 'cases': len(rows)}))


if __name__ == '__main__':
    main()
