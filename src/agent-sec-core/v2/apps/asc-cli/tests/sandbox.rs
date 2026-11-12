//! Linux CLI acceptance for terminal input, host termination and process-tree cleanup.
#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn sandbox_terminal_and_nonterminal_lifecycle() {
    if std::env::var_os("ASC_SANDBOX_INTEGRATION").is_none() {
        return;
    }
    let helper = std::env::var_os("ASC_SANDBOX_HELPER")
        .expect("ASC_SANDBOX_HELPER must select the built strict helper");
    let output = Command::new("python3")
        .args(["-c", LIFECYCLE, env!("CARGO_BIN_EXE_agent-sec-cli")])
        .arg(helper)
        .output()
        .expect("run Linux PTY acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "requires host mount privileges, local Docker, Python and built sibling guard"]
fn docker_cli_forwards_stdin_and_terminal_eof() {
    let image = std::env::var("ASC_DOCKER_TEST_IMAGE")
        .expect("set ASC_DOCKER_TEST_IMAGE to a locally available pinned Python image");
    let cli = env!("CARGO_BIN_EXE_agent-sec-cli");
    assert!(
        std::path::Path::new(cli)
            .with_file_name("asc-sandbox-docker-guard")
            .is_file(),
        "build asc-sandbox-docker-guard beside agent-sec-cli first"
    );
    let output = Command::new("python3")
        .args(["-c", DOCKER_STDIO, cli, &image])
        .output()
        .expect("run Docker CLI stdin acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const DOCKER_STDIO: &str = r#"
import errno, json, os, pathlib, pty, select, signal, subprocess, sys, tempfile, time

with tempfile.TemporaryDirectory(prefix='asc-docker-stdio-') as directory:
    root = pathlib.Path(directory)
    work = root / 'work'
    work.mkdir()
    permissions = root / 'permissions.json'
    permissions.write_text(json.dumps({
        'version': 1, 'filesystem': {'default': 'deny', 'rules': [
            {'effect': 'allow', 'path': str(work), 'rights': ['read', 'write']}]},
        'execution': {'deny_paths': []}, 'network': {'mode': 'off'}}))
    # Record only this test's container and config directory; never read personal Docker config.
    wrapper = root / 'docker'
    wrapper.write_text('#!/bin/sh\nif [ "$5" = start ]; then\n'
        f' printf "%s" "$8" > "{root}/name"\n'
        f' printf "%s" "$2" > "{root}/client"\n'
        'fi\nexec /usr/bin/docker "$@"\n')
    wrapper.chmod(0o700)
    (root / 'config.json').write_text('{}')
    env = {'PATH': '/usr/bin:/bin', 'AGENT_SEC_DAEMON_SOCKET': '/does/not/exist'}
    def argv(*target):
        return [sys.argv[1], 'sandbox', 'run', '--backend', 'docker',
            '--image', sys.argv[2], '--docker-cli', str(wrapper),
            '--permissions', str(permissions), '--', *target]
    def assert_clean():
        assert not pathlib.Path((root / 'client').read_text()).exists(), 'guard config leaked'
        name = (root / 'name').read_text()
        result = subprocess.run(['/usr/bin/docker', '--config', str(root), '--host',
            'unix:///var/run/docker.sock', 'ps', '--all', '--filter', f'name=^/{name}$',
            '--format', '{{.ID}}'], env=env, capture_output=True, timeout=10, check=True)
        assert not result.stdout.strip(), ('container leaked', name, result.stdout)
    # Cover binary data, input larger than the pipe buffer and empty input; cat must exit on EOF.
    for payload in (b'hello\n\x00\xff' + b'0123456789' * 8192, b''):
        result = subprocess.run(argv('/bin/cat'), cwd=work, env=env, input=payload,
            capture_output=True, timeout=15, check=False)
        assert result.returncode == 0, (result.returncode, result.stderr)
        assert result.stdout == payload, ('stdin corrupted', len(result.stdout), len(payload))
        assert_clean()
    code = "import sys; print('READY', flush=True); line=input(); print('GOT:'+line, flush=True); assert sys.stdin.read() == ''; print('EOF', flush=True)"
    pid, master = pty.fork()
    if pid == 0:
        os.chdir(work)
        os.execve(sys.argv[1], argv('/usr/local/bin/python3', '-c', code), env)
    data = b''
    completed = False
    deadline = time.monotonic() + 15
    def read_until(marker):
        global data, completed
        while marker not in data:
            dead, status = os.waitpid(pid, os.WNOHANG | os.WUNTRACED)
            if dead:
                completed = not os.WIFSTOPPED(status)
                raise AssertionError(('exited/stopped before terminal input', status, data))
            assert time.monotonic() < deadline, ('terminal input timeout', data)
            if select.select([master], [], [], .05)[0]:
                try:
                    data += os.read(master, 65536)
                except OSError as error:
                    if error.errno != errno.EIO:
                        raise
    try:
        read_until(b'READY\r\n')
        os.write(master, b'hello terminal\n')
        read_until(b'GOT:hello terminal\r\n')
        os.write(master, b'\x04')
        read_until(b'EOF\r\n')
        while True:
            dead, status = os.waitpid(pid, os.WNOHANG)
            if dead:
                completed = True
                assert os.waitstatus_to_exitcode(status) == 0, (status, data)
                break
            assert time.monotonic() < deadline, ('EOF did not finish CLI', data)
            time.sleep(.05)
        assert_clean()
    finally:
        if not completed:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            os.waitpid(pid, 0)
        os.close(master)
"#;

const LIFECYCLE: &str = r##"
import errno, json, os, pathlib, pty, select, shutil, signal, subprocess, sys, tempfile, time

with tempfile.TemporaryDirectory(prefix='asc-sandbox-cli-') as root:
    root = pathlib.Path(root)
    work = root / 'work'
    work.mkdir()
    cli = root / 'agent-sec-cli'
    shutil.copy2(sys.argv[1], cli)
    shutil.copy2(sys.argv[2], root / 'linux-sandbox')
    permissions = root / 'permissions.json'
    permissions.write_text(json.dumps({
        'version': 1,
        'filesystem': {'default': 'deny', 'rules': [
            {'effect': 'allow', 'path': '/', 'rights': ['read']},
            {'effect': 'allow', 'path': str(work), 'rights': ['read', 'write']}]},
        'execution': {'deny_paths': []}, 'network': {'mode': 'off'}}))
    env = dict(os.environ, AGENT_SEC_DAEMON_SOCKET='/does/not/exist', PATH='/usr/bin:/bin')
    def argv(script):
        return [str(cli), 'sandbox', 'run', '--permissions', str(permissions), '--', '/bin/sh', '-c', script]
    def terminal_case(script, send, expected):
        pid, master = pty.fork()
        if pid == 0:
            os.chdir(work)
            os.execve(cli, argv(script), env)
        data = b''
        completed = False
        try:
            end = time.monotonic() + 15
            while b'READY\r\n' not in data:
                dead, status = os.waitpid(pid, os.WNOHANG)
                completed = bool(dead)
                assert not dead, ('exited before input', status, data)
                assert time.monotonic() < end, ('input readiness timeout', data)
                if select.select([master], [], [], .1)[0]:
                    data += os.read(master, 65536)
            os.write(master, send)
            while True:
                dead, status = os.waitpid(pid, os.WNOHANG)
                if dead:
                    completed = True
                    while select.select([master], [], [], 0)[0]:
                        try:
                            chunk = os.read(master, 65536)
                            if not chunk: break
                            data += chunk
                        except OSError as error:
                            if error.errno != errno.EIO: raise
                            break
                    assert os.waitstatus_to_exitcode(status) == expected, (status, data)
                    break
                assert time.monotonic() < end, ('terminal command stopped or leaked', data)
                if select.select([master], [], [], .1)[0]:
                    try:
                        data += os.read(master, 65536)
                    except OSError as error:
                        if error.errno != errno.EIO: raise
            return data
        finally:
            if not completed:
                try: os.killpg(pid, signal.SIGKILL)
                except ProcessLookupError: pass
                os.waitpid(pid, 0)
            os.close(master)
    data = terminal_case("printf 'READY\n'; IFS= read -r value; printf 'GOT:%s\n' \"$value\"", b'hello sandbox\n', 0)
    assert b'GOT:hello sandbox' in data, data
    terminal_case("printf 'READY\n'; while :; do sleep 1; done", b'\x03', -signal.SIGINT)
    def running(pid):
        try:
            return pathlib.Path(f'/proc/{pid}/stat').read_text().split(') ', 1)[1][0] != 'Z'
        except (FileNotFoundError, ProcessLookupError):
            return False
    def descendants(pid):
        try:
            children = pathlib.Path(f'/proc/{pid}/task/{pid}/children').read_text().split()
        except (FileNotFoundError, ProcessLookupError):
            return []
        return [item for child in children for item in [int(child), *descendants(int(child))]]
    def wait_for_exit(pids):
        end = time.monotonic() + 5
        while any(running(pid) for pid in pids):
            assert time.monotonic() < end, ('sandbox processes survived parent exit', pids)
            time.sleep(.01)
    # A helper paused before Installed must not outlive the launcher, even on SIGKILL.
    installed_helper = root / 'linux-sandbox'
    setup_helper = root / 'setup-helper'
    setup_helper.write_text("#!/usr/bin/python3\nimport os, pathlib, sys, time\n"
        "assert sys.argv[1] == '--execution-spec-fd'\n"
        "print('BEFORE-INSTALL', os.getpid(), flush=True)\n"
        "time.sleep(1)\npathlib.Path('escaped-setup').write_text('escaped')\n")
    setup_helper.chmod(0o755)
    setup_helper.replace(installed_helper)
    child = subprocess.Popen(argv('true'), cwd=work, env=env, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    try:
        assert select.select([child.stdout], [], [], 15)[0], 'setup readiness timeout'
        ready, helper_pid = child.stdout.readline().split()
        assert ready == b'BEFORE-INSTALL'
        child.kill()
        stdout, stderr = child.communicate(timeout=5)
        assert child.returncode == -signal.SIGKILL, (child.returncode, stdout, stderr)
        wait_for_exit([int(helper_pid)])
        assert not (work / 'escaped-setup').exists(), 'uninstalled helper outlived its parent'
    finally:
        if child.poll() is None:
            child.kill()
            child.communicate(timeout=5)
    shutil.copy2(sys.argv[2], installed_helper)
    # READY can precede the helper's Started record; terminate immediately to exercise that race.
    for attempt in range(32):
        script = f"(sleep 1; echo escaped > escaped-{attempt}) & printf 'READY\n'; while :; do sleep 1; done"
        child = subprocess.Popen(argv(script), cwd=work, env=env, stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            assert select.select([child.stdout], [], [], 15)[0], 'headless readiness timeout'
            assert child.stdout.readline() == b'READY\n'
            tree = descendants(child.pid)
            assert tree, 'sandbox process tree was not created'
            termination = signal.SIGTERM if attempt % 2 == 0 else signal.SIGKILL
            child.send_signal(termination)
            stdout, stderr = child.communicate(timeout=5)
            assert child.returncode == -termination, (attempt, child.returncode, stdout, stderr)
            wait_for_exit(tree)
        finally:
            if child.poll() is None:
                child.kill()
                child.communicate(timeout=5)
    time.sleep(1.2)
    assert not list(work.glob('escaped-*')), 'background descendant survived parent termination'
"##;
