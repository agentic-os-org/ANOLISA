//! Real CLI input routing; the recording helper does not claim kernel isolation.
#![cfg(target_os = "linux")]

use std::process::Command;

#[test]
fn json_and_cedar_reach_the_direct_backend_without_a_daemon() {
    run_script(DIRECT);
}

#[test]
fn hook_rereads_cedar_for_each_authorization_request() {
    if !rustix::process::geteuid().is_root() {
        eprintln!("requires root to satisfy the daemon client's peer-UID contract");
        return;
    }
    run_script(HOOK);
}

#[test]
fn cedar_permissions_are_enforced_by_the_real_linux_helper() {
    if std::env::var_os("ASC_SANDBOX_INTEGRATION").is_none() {
        return;
    }
    let helper = std::env::var_os("ASC_SANDBOX_HELPER")
        .expect("ASC_SANDBOX_HELPER must select the built strict helper");
    let output = Command::new("python3")
        .args(["-c", REAL_HELPER, env!("CARGO_BIN_EXE_agent-sec-cli")])
        .arg(helper)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_script(script: &str) {
    let output = Command::new("python3")
        .args(["-c", script, env!("CARGO_BIN_EXE_agent-sec-cli")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

const DIRECT: &str = r#"
import json, os, pathlib, shutil, subprocess, sys, tempfile

with tempfile.TemporaryDirectory(prefix='asc-permission-cli-') as root:
    root = pathlib.Path(root)
    cli = root / 'agent-sec-cli'
    shutil.copy2(sys.argv[1], cli)
    helper = root / 'linux-sandbox'
    helper.write_text('''#!/usr/bin/python3
import json, os, sys
with os.fdopen(int(sys.argv[2])) as source:
    spec = json.load(source)
print(json.dumps(spec['permissions']))
with os.fdopen(int(sys.argv[4]), 'w') as status:
    for event in [{'stage':'installed'}, {'stage':'started'}, {'stage':'exited','exit_code':23,'signal':None}]:
        status.write(json.dumps(event) + '\\n')
''')
    helper.chmod(0o700)
    source = root / 'input.policy'
    expected = {'version':1, 'filesystem':{'default':'deny','rules':[
        {'effect':'allow','path':'/','rights':['read']}]},
        'execution':{'deny_paths':[]}, 'network':{'mode':'off'}}
    cedar = 'permit(principal, action == SecCore::Action::"file.read", resource);'
    for text in [json.dumps(expected), cedar]:
        source.write_text(text)
        result = subprocess.run([str(cli), '--socket', str(root / 'absent.sock'),
            'sandbox', 'run', '--permissions', str(source), '--', '/usr/bin/true'],
            capture_output=True, text=True, timeout=20, cwd=root)
        assert result.returncode == 23, (result.stdout, result.stderr)
        assert json.loads(result.stdout) == expected, result.stdout
    for invalid in [b'permit this is broken', b'\xff', b' ' * (4 * 1024 * 1024 + 1)]:
        source.write_bytes(invalid)
        result = subprocess.run([str(cli), 'sandbox', 'run', '--permissions', str(source), '--', '/usr/bin/true'],
            capture_output=True, text=True, timeout=20, cwd=root)
        assert result.returncode != 0 and not result.stdout, (result.stdout, result.stderr)
"#;

const HOOK: &str = r#"
import json, pathlib, socket, subprocess, sys, tempfile, threading

with tempfile.TemporaryDirectory(prefix='asc-permission-hook-') as root:
    root = pathlib.Path(root)
    endpoint = root / 'daemon.sock'
    source = root / 'request.policy'
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(endpoint))
    listener.listen()
    listener.settimeout(15)
    received, errors = [], []
    def receive():
        try:
            connection, _ = listener.accept()
            with connection, connection.makefile('rb') as stream:
                received.append(json.loads(stream.readline()))
                connection.sendall(json.dumps({'requestId':'input-test','error':{
                    'code':'unavailable','message':'recording request only'}}).encode() + b'\n')
        except Exception as error:
            errors.append(error)
    read = 'permit(principal, action == SecCore::Action::"file.read", resource);'
    write = 'permit(principal, action == SecCore::Action::"file.write", resource);'
    original_json = {'version':1, 'filesystem':{'default':'deny','rules':[
        {'effect':'allow','path':'/','rights':['read']}]},
        'execution':{'deny_paths':[]}, 'network':{'mode':'off'}}
    for policy in [read, read + write, json.dumps(original_json)]:
        source.write_text(policy)
        thread = threading.Thread(target=receive)
        thread.start()
        result = subprocess.run([sys.argv[1], '--socket', str(endpoint), 'sandbox', 'hook', 'prepare',
            '--permissions', str(source)], input=json.dumps({'session_id':'cedar-input',
                'cwd':str(root),'tool_input':{'command':'true'}}),
            capture_output=True, text=True, timeout=20)
        thread.join(timeout=20)
        assert not thread.is_alive() and not errors, errors
        assert result.returncode != 0 and 'recording request only' in result.stderr, result.stderr
    listener.close()
    assert len(received) == 3, received
    for request in received:
        assert request['method'] == 'authorization.evaluate', request
        assert request['params']['principal'] == {'agent_id':'cosh:cedar-input','agent_type':'cosh'}
        assert 'policy' not in request['params'], request
    first, second, legacy = [item['params']['requested_permissions'] for item in received]
    assert first == legacy == original_json
    assert first != second
    assert all('write' not in rule['rights'] for rule in first['filesystem']['rules'])
    assert any('write' in rule['rights'] for rule in second['filesystem']['rules'])
"#;

const REAL_HELPER: &str = r#"
import json, os, pathlib, shutil, socket, subprocess, sys, tempfile

with tempfile.TemporaryDirectory(prefix='asc-cedar-real-') as root:
    root = pathlib.Path(root)
    cli = root / 'agent-sec-cli'
    shutil.copy2(sys.argv[1], cli)
    shutil.copy2(sys.argv[2], root / 'linux-sandbox')
    cwd = root / 'cwd'
    cwd.mkdir()
    target = root / 'private'
    target.mkdir()
    (target / 'key').write_text('secret')
    (target / 'public').write_text('public')
    alias = root / 'alias'
    alias.symlink_to(target, target_is_directory=True)
    file_alias = root / 'file-alias'
    file_alias.symlink_to(target / 'key')
    source = root / 'permissions.policy'
    def tree(path):
        return f'(resource.path == {json.dumps(str(path))} || resource.path like {json.dumps(str(path) + "/*")})'
    def rule(effect, action, condition=None):
        return f'{effect}(principal, action == SecCore::Action::"{action}", resource)' + (f' when {{ {condition} }}' if condition else '') + ';\n'
    base = ''.join(rule('permit', 'file.read', tree(path))
        for path in ['/usr', '/etc', '/bin', '/lib', '/lib64', str(cwd)] if os.path.exists(path))
    def run(policy, command, envelope=False):
        if envelope:
            policy = json.dumps({'language':'cedar','content':{
                'schema':{'schemaId':'seccore.system','revision':1},'text':policy}})
        source.write_text(policy)
        return subprocess.run([str(cli), '--socket', str(root / 'absent.sock'),
            'sandbox', 'run', '--permissions', str(source), '--'] + command,
            cwd=cwd, capture_output=True, text=True, timeout=20)
    def check(result, expected, stdout=None):
        assert (result.returncode == 0) == expected, (result.returncode, result.stdout, result.stderr)
        if stdout is not None:
            assert result.stdout == stdout, (result.stdout, result.stderr)
    # This grant has no readable parent mount; losing the target exclusion grants the secret.
    file_read = rule('permit', 'file.read', f'resource.path == {json.dumps(str(file_alias))}')
    deny_key = rule('forbid', 'file.read', f'resource.path == {json.dumps(str(target / "key"))}')
    check(run(base + file_read, ['/usr/bin/cat', str(file_alias)]), True, 'secret')
    denied = run(base + file_read + deny_key, ['/usr/bin/cat', str(file_alias)])
    check(denied, False)
    assert 'secret' not in denied.stdout
    # Read-only directory aliases and intermediate links preserve the canonical carveout.
    directory_read = rule('permit', 'file.read', tree(alias))
    for envelope in [False, True]:
        check(run(base + directory_read + deny_key,
            ['/usr/bin/cat', str(alias / 'public')], envelope), True, 'public')
        check(run(base + directory_read + deny_key,
            ['/usr/bin/cat', str(alias / 'key')], envelope), False)
    # Only directory boundaries occur in these policies. Their exclusions must
    # retain the helper's recursive hardlink check even outside readable roots.
    work = root / 'work'
    work.mkdir()
    (work / 'key').write_text('ordinary')
    work_read = rule('permit', 'file.read', tree(work))
    exclusions = [
        f'resource.path == {json.dumps(str(target))}',
        f'resource.path like {json.dumps(str(target) + "/*")}',
        tree(target),
    ]
    policies = [base + work_read + rule('permit', 'file.write', tree(work))
        + rule('forbid', action, exclusion)
        for action in ['file.read', 'file.write'] for exclusion in exclusions]
    for policy in policies:
        for envelope in [False, True]:
            check(run(policy, ['/usr/bin/cat', str(work / 'key')], envelope), True, 'ordinary')
    (work / 'key').unlink()
    os.link(target / 'key', work / 'key')
    check(run(base + work_read, ['/usr/bin/cat', str(work / 'key')]), True, 'secret')
    for policy in policies:
        for envelope in [False, True]:
            denied = run(policy, ['/usr/bin/cat', str(work / 'key')], envelope)
            assert denied.returncode != 0, (policy, envelope, denied.stdout, denied.stderr)
            assert 'hardlink' in denied.stderr and 'secret' not in denied.stdout, (denied.stdout, denied.stderr)
    (work / 'key').unlink()
    # A complement forbidding everything outside granted trees remains usable.
    permitted = ' || '.join(tree(path) for path in ['/usr', '/etc', '/bin', '/lib', '/lib64', str(cwd)] if os.path.exists(path))
    complement = rule('permit', 'file.read') + f'forbid(principal, action == SecCore::Action::"file.read", resource) unless {{ {permitted} }};'
    check(run(complement, ['/usr/bin/true']), True)
    # The approved write mapping includes deletion and applies to subprocesses.
    writable = base + rule('permit', 'file.write', tree(cwd)) + rule('forbid', 'file.write',
        'context.input.operation == SecCore::WriteOperation::"delete"')
    check(run(writable, ['/bin/sh', '-c', 'printf child > report; /bin/sh -c "rm report"']), True)
    assert not (cwd / 'report').exists()
    # A link beneath a writable parent must fail before executing the command.
    mutable_alias = cwd / 'link'
    mutable_alias.symlink_to(target / 'key')
    unsafe = run(writable + rule('forbid', 'file.read',
        f'resource.path == {json.dumps(str(mutable_alias))}'), ['/bin/sh', '-c', 'touch started'])
    check(unsafe, False)
    assert 'writable ancestor' in unsafe.stderr and not (cwd / 'started').exists(), unsafe.stderr
    # An independently listening host socket distinguishes off from explicit full networking.
    with socket.socket() as server:
        server.bind(('127.0.0.1', 0))
        server.listen()
        script = f'import socket; socket.create_connection(("127.0.0.1", {server.getsockname()[1]}), timeout=2)'
        check(run(base, ['/usr/bin/python3', '-c', script]), False)
        check(run(base + rule('permit', 'network.use'), ['/usr/bin/python3', '-c', script]), True)
"#;
