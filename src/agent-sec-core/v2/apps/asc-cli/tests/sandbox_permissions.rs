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
    run_real_helper(REAL_HELPER);
}

#[test]
fn subtree_exceptions_work_through_json_and_cedar_cli_inputs() {
    run_real_helper(SUBTREE_HELPER);
}

fn run_real_helper(script: &str) {
    if std::env::var_os("ASC_SANDBOX_INTEGRATION").is_none() {
        return;
    }
    let helper = std::env::var_os("ASC_SANDBOX_HELPER")
        .expect("ASC_SANDBOX_HELPER must select the built strict helper");
    let output = Command::new("python3")
        .args(["-c", script, env!("CARGO_BIN_EXE_agent-sec-cli")])
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

const SUBTREE_HELPER: &str = r#"
import json, pathlib, shutil, subprocess, sys, tempfile

with tempfile.TemporaryDirectory(prefix='asc-subtree-cli-') as root:
    root = pathlib.Path(root)
    cli = root / 'agent-sec-cli'
    shutil.copy2(sys.argv[1], cli)
    shutil.copy2(sys.argv[2], root / 'linux-sandbox')
    parent = root / 'sessions'
    child = parent / 'current'
    child.mkdir(parents=True)
    (parent / 'secret').write_text('hidden')
    (child / 'seed').write_text('allowed')
    source = root / 'permissions'
    def tree(path):
        return f'(resource.path == {json.dumps(str(path))} || resource.path like {json.dumps(str(path) + "/*")})'
    policy = f'''
permit(principal, action == SecCore::Action::"file.read", resource);
permit(principal, action == SecCore::Action::"file.write", resource) when {{ {tree(child)} }};
forbid(principal, action == SecCore::Action::"file.read", resource)
when {{ {tree(parent)} }} unless {{ {tree(child)} }};
'''
    permissions = {'version': 1, 'filesystem': {'default': 'deny', 'rules': [
        {'effect':'allow', 'path':'/', 'rights':['read']},
        {'effect':'deny', 'path':str(parent), 'rights':['read'], 'excluded_paths':[str(child)]},
        {'effect':'allow', 'path':str(child), 'rights':['write']}
    ]}, 'execution': {'deny_paths':[]}, 'network': {'mode':'off'}}
    script = f'''set -eu
cat seed > result
sh -c 'cat result > child-result'
if ls '{parent}'; then exit 41; fi
if cat '{parent}/secret'; then exit 42; fi
if touch '{parent}/new'; then exit 43; fi
'''
    def run(text, command=script):
        source.write_text(text)
        return subprocess.run([str(cli), '--socket', str(root/'absent.sock'), 'sandbox', 'run',
            '--permissions', str(source), '--', '/bin/sh', '-c', command],
            cwd=child, capture_output=True, text=True, timeout=30)
    for text in [json.dumps(permissions), policy]:
        result = run(text)
        assert result.returncode == 0, (result.stdout, result.stderr)
        assert (child/'child-result').read_text() == 'allowed'
    permissions['filesystem']['rules'][2]['rights'] = ['read']
    result = run(json.dumps(permissions), "cat seed; if touch denied-write; then exit 44; fi")
    assert result.returncode == 0 and not (child/'denied-write').exists(), result.stderr
    permissions['filesystem']['rules'][2]['rights'] = ['write']
    permissions['version'] = 2
    result = run(json.dumps(permissions), "touch must-not-run")
    assert result.returncode != 0 and not (child/'must-not-run').exists(), result.stderr
    assert 'unsupported OS permission version' in result.stderr, result.stderr
    permissions['version'] = 1
    alias = parent / 'alias'
    alias.symlink_to(child, target_is_directory=True)
    for boundary in [alias, parent/'secret', parent/'missing']:
        permissions['filesystem']['rules'][1]['excluded_paths'] = [str(boundary)]
        result = run(json.dumps(permissions), "touch must-not-run")
        assert result.returncode != 0 and not (child/'must-not-run').exists(), result.stderr
    permissions['filesystem']['rules'][1].pop('excluded_paths')
    result = run(json.dumps(permissions), "touch must-not-run")
    assert result.returncode != 0 and not (child/'must-not-run').exists(), result.stderr
"#;

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
if sys.argv[1] == '--check-landlock':
    print('landlock-write-v1')
    sys.exit(0)
with os.fdopen(int(sys.argv[2])) as source:
    spec = json.load(source)
print(json.dumps(spec['permissions']))
installed = {'stage':'installed'}
if 'landlock' in spec:
    with open(os.path.join(os.path.dirname(__file__), 'evidence.json')) as evidence:
        mode, status = json.load(evidence)
    assert spec['landlock'] == mode
    installed = {'stage':'installed_with_landlock','landlock':status}
with os.fdopen(int(sys.argv[4]), 'w') as status:
    for event in [installed, {'stage':'started'}, {'stage':'exited','exit_code':23,'signal':None}]:
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
    source.write_text(json.dumps(expected))
    skipped = {'state':'skipped','reason':'LSM disabled'}
    enforced = {'state':'enforced','abi':3}
    for mode, evidence, code, diagnostic in [
        ('auto', skipped, 23, 'Landlock skipped: LSM disabled'),
        ('required', enforced, 23, 'Landlock enforced (kernel ABI 3)'),
        ('required', skipped, None, 'invalid backend handshake'),
        ('required', {'state':'enforced','abi':2}, None, 'invalid backend handshake'),
    ]:
        (root / 'evidence.json').write_text(json.dumps([mode, evidence]))
        result = subprocess.run([str(cli), 'sandbox', 'run', '--landlock', mode,
            '--permissions', str(source), '--', '/usr/bin/true'], cwd=root,
            capture_output=True, text=True, timeout=20)
        if code is None:
            assert result.returncode not in (0, 23), result
        else:
            assert result.returncode == code, result
        assert diagnostic in result.stderr, result
    result = subprocess.run([str(cli), 'sandbox', 'run', '--backend', 'docker',
        '--landlock', 'auto', '--image', 'unused:test', '--permissions', str(source),
        '--', '/usr/bin/true'], cwd=root, capture_output=True, text=True, timeout=20)
    assert result.returncode != 0 and '--landlock requires the linux backend' in result.stderr, result
    for text in [cedar, json.dumps({'language':'cedar','content':{'text':cedar}})]:
        source.write_text(text)
        result = subprocess.run([str(cli), 'sandbox', 'run', '--backend', 'docker',
            '--image', 'unused:test', '--docker-cli', str(root / 'absent-docker'),
            '--permissions', str(source), '--', '/usr/bin/true'],
            capture_output=True, text=True, timeout=20, cwd=root)
        assert result.returncode != 0 and not result.stdout, (result.stdout, result.stderr)
        assert 'Cedar inputs require the linux backend' in result.stderr, result.stderr
    source.write_text(json.dumps(expected))
    for program, error in [('true', 'absolute program path inside the image'),
                           ('/usr/bin/true', 'docker CLI not found')]:
        result = subprocess.run([str(cli), 'sandbox', 'run', '--backend', 'docker',
            '--image', 'unused:test', '--docker-cli', str(root / 'absent-docker'),
            '--permissions', str(source), '--', program],
            capture_output=True, text=True, timeout=20, cwd=root)
        assert result.returncode != 0 and not result.stdout, (result.stdout, result.stderr)
        assert error in result.stderr, result.stderr
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
    output = root / 'new-output'
    create = read + 'permit(principal, action == SecCore::Action::"file.write", resource) when { resource.path == ' + json.dumps(str(output)) + ' && context.input.operation == SecCore::WriteOperation::"create" };'
    for policy in [read, read + write, json.dumps(original_json), create]:
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
    assert len(received) == 4, received
    creation = received[3]['params']['context']['requested_permissions']['filesystem']['rules']
    assert any(rule['path'] == str(root) and rule['effect'] == 'allow' and 'write' in rule['rights'] for rule in creation), creation
    assert all(rule['path'] != str(output) for rule in creation) and not output.exists(), creation
    for request in received:
        assert request['method'] == 'authorization.evaluate', request
        assert set(request['params']) == {'request_id','principal','action','resource','context'}, request
        assert request['params']['principal'] == {'type':'SecCore::Subject','id':'request'}, request
        assert request['params']['action'] == {'type':'SecCore::Action','id':'process.spawn'}, request
        assert request['params']['resource'] == {
            'type':'SecCore::Process','id':request['params']['context']['launch']['program']}, request
        assert request['params']['context']['agent'] == {'agent_type':'cosh'}, request
        assert 'policy' not in request['params'], request
        assert 'policy' not in request['params']['context'], request
    first, second, legacy = [item['params']['context']['requested_permissions'] for item in received[:3]]
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
    # A missing creation target requests the whole parent, including sibling mutations.
    output = cwd / 'new-output'
    protected = cwd / 'protected'
    protected.write_text('unchanged')
    create = base + rule('permit', 'file.write',
        f'resource.path == {json.dumps(str(output))} && context.input.operation == SecCore::WriteOperation::"create"')
    create += rule('forbid', 'file.write', f'resource.path == {json.dumps(str(protected))}')
    check(run(create, ['/bin/sh', '-c', 'printf created > new-output; printf sibling > sibling']), True)
    assert output.read_text() == 'created' and (cwd / 'sibling').read_text() == 'sibling'
    output.unlink()
    check(run(create, ['/bin/sh', '-c', 'printf changed > protected']), False)
    assert protected.read_text() == 'unchanged' and not output.exists()
    check(run(create, ['/bin/sh', '-c', f'printf outside > {root / "outside"}']), False)
    assert not (root / 'outside').exists()
    # Existing directory write covers explicitly mentioned missing descendants without create.
    nested = cwd / 'a' / 'b' / 'new'
    ancestor_write = base + rule('permit', 'file.write',
        tree(cwd) + ' && context.input.operation == SecCore::WriteOperation::"write"')
    ancestor_write += rule('forbid', 'file.write', 'context.input.operation == SecCore::WriteOperation::"create"')
    ancestor_write += rule('permit', 'file.read', f'resource.path == {json.dumps(str(nested))}')
    check(run(ancestor_write, ['/bin/sh', '-c', 'mkdir -p a/b && printf nested > a/b/new']), True)
    assert nested.read_text() == 'nested'
    shutil.rmtree(cwd / 'a')
    uncovered = base + rule('permit', 'file.write', f'resource.path == {json.dumps(str(nested))}')
    denied = run(uncovered, ['/bin/sh', '-c', 'mkdir -p a/b && printf unexpected > a/b/new'])
    check(denied, False)
    assert str(nested) in denied.stderr and 'no existing writable ancestor covers the target' in denied.stderr, denied.stderr
    assert not (cwd / 'a').exists()
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
