"""Run a Pi SDK fixture using either launcher with bounded process cleanup."""
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess


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


def _kill_group(pid: int) -> None:
    try:
        os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass


def cooperative_run(aw, launcher, config, cwd, env, timeout):
    with subprocess.Popen([shutil.which('node'), launcher], cwd=cwd, env=env,
                          text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          start_new_session=True) as worker:
        try:
            code, out, err = call([aw, 'connect', '--config', config,
                                   '--sdk-pid', worker.pid, '--timeout-ms', 10000],
                                  cwd, env, timeout=15)
            if code:
                if worker.poll() is None:
                    _kill_group(worker.pid)
                worker_out, worker_err = worker.communicate(timeout=5)
                if worker.returncode and not out and not err:
                    return worker.returncode, worker_out, worker_err
                if worker_err:
                    err = f'{err}\n--- worker stderr ---\n{worker_err}'.strip()
                return code, out, err
            out, err = worker.communicate(timeout=timeout)
            return worker.returncode, out, err
        finally:
            if worker.poll() is None:
                _kill_group(worker.pid)
                worker.wait(timeout=5)


def run_application(aw, application, config, cwd, env, *, bridge=None, timeout=45):
    if bridge is None:
        return call([aw, 'run', '--config', config, '--agent', 'pi'], cwd, env, timeout=timeout)
    # Deployment compiles the descriptor once; the worker starts without the parser.
    code, out, err = call([aw, 'config', 'descriptor', '--config', config, '--agent', 'pi',
                           '--profile', 'pi-worker-hooks'], cwd, env, timeout=15)
    if code:
        return code, out, err
    descriptor = cwd / 'descriptor.json'
    descriptor.write_text(out)
    descriptor.chmod(0o600)
    launcher = cwd / 'launcher.mjs'
    options = {"application": str(application), "configFile": str(config),
               "descriptorFile": str(descriptor), "executable": "/nonexistent/aw",
               "attachTimeoutMs": 12000}
    module = json.dumps(Path(bridge).resolve().as_uri())
    launcher.write_text(f'import {{ runPiWorker }} from {module};\n'
                        f'await runPiWorker({json.dumps(options)});\n')
    return cooperative_run(aw, launcher, config, cwd, env, timeout)
