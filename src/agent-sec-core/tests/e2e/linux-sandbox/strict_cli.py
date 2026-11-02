"""Exercise the installed CLI and real Linux sandbox without a daemon."""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path


def main() -> None:
    cli = Path(sys.argv[1]).resolve()
    with tempfile.TemporaryDirectory(prefix="asc-strict-cli-") as directory:
        work = Path(directory).resolve()
        (work / "key").write_text("secret")
        shutil.copy2("/bin/true", work / "payload")
        permission = {
            "version": 1,
            "filesystem": {
                "default": "deny",
                "rules": [
                    {"effect": "allow", "path": "/", "rights": ["read"]},
                    {"effect": "allow", "path": str(work), "rights": ["write"]},
                    {"effect": "deny", "path": str(work / "key"), "rights": ["read"]},
                ],
            },
            "execution": {"deny_paths": [str(work / "payload")]},
            "network": {"mode": "off"},
        }
        permission_path = work / "permissions.json"
        permission_path.write_text(json.dumps(permission))
        code = """
import os, pathlib, socket, subprocess
p = pathlib.Path('.')
(p / 'result').write_text('ok')
try:
    (p / 'key').read_text()
except (OSError, UnicodeError):
    pass
else:
    raise AssertionError('key readable')
try:
    subprocess.run([str(p.resolve() / 'payload')], check=True)
except PermissionError:
    pass
else:
    raise AssertionError('noexec bypassed')
try:
    socket.socket(socket.AF_INET, socket.SOCK_STREAM)
except OSError:
    pass
else:
    raise AssertionError('IP socket available in off mode')
assert 'CapEff:\\t0000000000000000' in pathlib.Path('/proc/self/status').read_text()
assert 'NoNewPrivs:\\t1' in pathlib.Path('/proc/self/status').read_text()
assert subprocess.run(['/usr/bin/true']).returncode == 0
print('CLI isolation passed')
"""
        environment = dict(
            os.environ, AGENT_SEC_DAEMON_SOCKET=str(work / "absent.sock")
        )
        subprocess.run(
            [
                str(cli),
                "sandbox",
                "run",
                "--permissions",
                str(permission_path),
                "--",
                sys.executable,
                "-c",
                code,
            ],
            cwd=work,
            env=environment,
            check=True,
            timeout=30,
        )
        assert (work / "result").read_text() == "ok"
        node = shutil.which("node")
        assert node is not None, "Node.js is required for the runtime acceptance test"
        node_code = """
const fs = require('node:fs');
const cp = require('node:child_process');
const assert = require('node:assert/strict');
fs.writeFileSync('node-result', 'ok');
assert.throws(() => fs.readFileSync('key'));
assert.throws(() => cp.execFileSync(process.cwd() + '/payload'));
assert.equal(cp.spawnSync('/usr/bin/true').status, 0);
console.log('Node isolation passed');
"""
        subprocess.run(
            [
                str(cli),
                "sandbox",
                "run",
                "--permissions",
                str(permission_path),
                "--",
                node,
                "-e",
                node_code,
            ],
            cwd=work,
            env=environment,
            check=True,
            timeout=30,
        )
        assert (work / "node-result").read_text() == "ok"
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen()
            permission["network"]["mode"] = "full"
            permission_path.write_text(json.dumps(permission))
            port = listener.getsockname()[1]
            subprocess.run(
                [
                    str(cli),
                    "sandbox",
                    "run",
                    "--permissions",
                    str(permission_path),
                    "--",
                    sys.executable,
                    "-c",
                    f"import socket; socket.create_connection(('127.0.0.1', {port}), timeout=2).close()",
                ],
                cwd=work,
                env=environment,
                check=True,
                timeout=30,
            )
            connection, _ = listener.accept()
            connection.close()
        permission["filesystem"]["rules"] = []
        permission_path.write_text(json.dumps(permission))
        failed = subprocess.run(
            [
                str(cli),
                "sandbox",
                "run",
                "--permissions",
                str(permission_path),
                "--",
                "/usr/bin/touch",
                str(work / "must-not-run"),
            ],
            cwd=work,
            env=environment,
            timeout=30,
            check=False,
        )
        assert failed.returncode != 0
        assert not (work / "must-not-run").exists()
        print("Strict CLI acceptance passed")


if __name__ == "__main__":
    main()
