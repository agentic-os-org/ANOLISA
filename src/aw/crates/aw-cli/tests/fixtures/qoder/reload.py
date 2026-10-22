"""Require native Qoder TUI readiness and idle policy switching before dispatch."""

import argparse
import copy
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time

AW = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(AW / 'scripts'))
from native_qoder import install
from native_cleanup import own_children, reap_children


def interrupted(signum, frame):
    raise KeyboardInterrupt(f'interrupted by signal {signum}')


def drain(terminal):
    try:
        if select.select([terminal], [], [], 0.05)[0]:
            os.read(terminal, 65536)
    except OSError:
        pass


def run(argv, cwd, environment, terminal):
    with subprocess.Popen(argv, cwd=cwd, env=environment, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, stdin=subprocess.DEVNULL, text=True) as child:
        deadline = time.monotonic()+10
        try:
            while time.monotonic() < deadline:
                drain(terminal)
                try:
                    stdout, stderr = child.communicate(timeout=0.1)
                except subprocess.TimeoutExpired:
                    continue
                if child.returncode:
                    raise RuntimeError(f"AW control command failed ({child.returncode}): {stderr.strip()}")
                return json.loads(stdout)
            raise TimeoutError('AW control command exceeded its deadline')
        finally:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=5)


def status(aw, config, cwd, environment, terminal):
    return run([aw, 'status', '--config', str(config)], cwd, environment, terminal)['result']


def exercise(binaries, output):
    token = os.environ.get('QODER_PERSONAL_ACCESS_TOKEN')
    if not token:
        raise RuntimeError('Qoder TUI acceptance requires QODER_PERSONAL_ACCESS_TOKEN')
    pin = json.loads((AW / 'tests/native-matrix.json').read_text())['agents']['qoder']['qoder_reload_cli']
    cases, identities = [], []
    with tempfile.TemporaryDirectory(prefix='aw-qoder-reload-') as directory:
        root = Path(directory)
        # Version/install probes never receive the model credential.
        os.environ.pop('QODER_PERSONAL_ACCESS_TOKEN')
        try:
            installed = install(pin, root / 'qoder')
        finally:
            os.environ['QODER_PERSONAL_ACCESS_TOKEN'] = token
        (output / 'qoder-version.json').write_text(json.dumps(installed, indent=2)+'\n')
        home = root / 'home'
        home.mkdir(mode=0o700)
        environment = {**os.environ, 'HOME': str(home), 'TERM': 'xterm-256color'}
        controls = {key: value for key, value in environment.items() if key != 'QODER_PERSONAL_ACCESS_TOKEN'}
        aw = str(binaries / 'aw')
        config = root / 'current.json'
        document = {'apiVersion': 'aw/v1alpha1', 'kind': 'AWConfiguration',
                    'metadata': {'name': 'qoder-reload-native'}, 'spec': {
                        'daemon': {'startup': 'on_demand', 'endpoint': 'auto', 'state_dir': str(root/'state')},
                        'execution': {'guarantee': 'native_hook', 'default_event_budget_ms': 1000},
                        'audit': {'enabled': True, 'payload': 'metadata_only'},
                        'agents': {'qoder': {'adapter': 'qoder', 'argv': [str(root/'qoder/qodercli')], 'qoder': {'sequential': True}}},
                        'providers': {}, 'events': {}}}
        config.write_text(json.dumps(document))
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 30, 100, 0, 0))
        command = [aw, 'run', '--config', str(config), '--agent', 'qoder', '--reloadable']
        process = None
        native = None
        record = {'command': command, 'cwd': str(root), 'pid': None, 'pgid': None,
                  'ports': [], 'expected_lifetime_seconds': 90, 'log': None,
                  'output_policy': 'discard terminal bytes; metadata report only', 'stop': None}
        (output/'process.json').write_text(json.dumps(record, indent=2)+'\n')
        try:
            process = subprocess.Popen(command, cwd=root, env=environment, stdin=slave,
                                       stdout=slave, stderr=slave, start_new_session=True,
                                       preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
            record.update(pid=process.pid, pgid=process.pid, stop=f'kill -TERM -- -{process.pid}')
            (output/'process.json').write_text(json.dumps(record, indent=2)+'\n')
            os.close(slave)
            slave = None
            os.set_blocking(master, False)
            deadline = time.monotonic()+90
            while time.monotonic() < deadline:
                drain(master)
                if process.poll() is not None:
                    raise RuntimeError(f'Qoder TUI exited before native readiness: {process.returncode}')
                if not (root/'state/aw.sock').exists():
                    continue
                snapshot = status(aw, config, root, controls, master)
                if len(snapshot['instances']) == 1 and snapshot['instances'][0]['ready']:
                    native = snapshot['instances'][0]
                    break
            if native is None:
                raise TimeoutError('Qoder did not confirm native SessionStart readiness')
            assert native['native_version'] == '1.1.73' and native['entrypoint'] == 'tui'
            assert native['requests'] == native['tools'] == 0
            identities.append(native['process'])
            cases.append({'case': 'prewarm', 'passed': True})
            candidate = copy.deepcopy(document)
            candidate['spec']['providers']['observe'] = {
                'protocol': 'native-hook/v1alpha1',
                'transport': {'type': 'stdio', 'location': 'agent', 'argv': ['/bin/true']},
                'timeout_ms': 500, 'max_output_bytes': 4096, 'config': {}}
            candidate['spec']['events']['tool.before'] = {'enabled': True, 'steps': [
                {'id': 'observe', 'provider': 'observe', 'native': {}, 'on_error': 'block'}]}
            for name, policy in [('add-hook', candidate), ('remove-hook', document)]:
                policy_path = root/'next.json'
                policy_path.write_text(json.dumps(policy))
                activated = run([aw, 'update', '--config', str(config), '--agent', 'qoder',
                                 '--policy', str(policy_path)], root, controls, master)
                current = status(aw, config, root, controls, master)['instances'][0]
                expected = hashlib.sha256(policy_path.read_bytes()).hexdigest()
                assert activated['policy_revision'] == current['policy_revision'] == expected
                assert current['instance_id'] == native['instance_id']
                assert current['requests'] == current['tools'] == 0 and current['ready']
                assert current['process']['alive'] and process.poll() is None
                identities.append(current['process'])
                cases.append({'case': name, 'passed': True})
        finally:
            try:
                if process is not None:
                    if process.poll() is None:
                        process.send_signal(signal.SIGTERM)
                    limit = time.monotonic()+10
                    while process.poll() is None and time.monotonic() < limit:
                        drain(master)
                        time.sleep(0.01)
                    if process.poll() is None:
                        process.kill()
                        process.wait(timeout=5)
                        raise RuntimeError('AW launcher did not cleanly stop native Qoder')
                if (root/'state/aw.sock').exists():
                    final = status(aw, config, root, controls, master)
                    assert final['bindings'] == final['events'] == 0
                    run([aw, 'stop', '--config', str(config)], root, controls, master)
                    limit = time.monotonic()+5
                    while (root/'state/aw.sock').exists() and time.monotonic() < limit:
                        time.sleep(0.01)
                    assert not (root/'state/aw.sock').exists()
                if native is not None:
                    assert not Path(f"/proc/{native['process']['pid']}").exists(), 'native Qoder still running'
            finally:
                os.close(master)
                if slave is not None:
                    os.close(slave)
        assert identities and all(identity == identities[0] for identity in identities)
        report = {'passed': len(cases) == 3, 'native_version': installed['version'],
                  'same_process': True, 'instance_released': True, 'runtime_removed': True,
                  'cases': cases, 'process': identities[0]}
    assert not root.exists()
    (output/'results.json').write_text(json.dumps(report, indent=2)+'\n')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bin', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, interrupted)
    own_children()
    try:
        exercise(args.bin.resolve(), output)
    finally:
        reaped = reap_children()
        (output/'cleanup.json').write_text(json.dumps({'reaped': reaped, 'passed': True})+'\n')
