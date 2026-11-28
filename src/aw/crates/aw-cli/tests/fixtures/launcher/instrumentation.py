"""Exercise builtins through real Python and Node processes and AW policy admission."""

from __future__ import annotations

import hashlib
import json
import os
import select
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CLI = str(Path(sys.argv[1]).resolve())
SDK = Path(__file__).resolve().parents[5] / "sdk"

POLICY = """
import json, sys
request = json.load(sys.stdin)
response = {
    "api_version": request["api_version"],
    "request_id": request["request_id"],
    "status": "ok",
}
if request["method"] == "describe":
    response["operations"] = [{
        "name":"check", "events":["tool.before"], "effects":["observe", "block"]
    }]
elif request["method"] == "invoke":
    denied = request["event"]["tool"]["input"]["command"] == "deny"
    response["input_digest"] = request["input_digest"]
    response["effects"] = [{"type":"block","reason_code":"policy_denied"}] if denied else []
print(json.dumps(response))
"""

PYTHON = """
import asyncio, os, subprocess, sys
from aw_profile_fixture import call_tool, call_tool_async
from aw_sdk.profile_preload import preloaded_profile_source
from aw_sdk.transport import AwError

assert os.environ.get("TEST_SITE_CUSTOMIZE") == "loaded"
assert "AW_PROFILE_BOOTSTRAP" not in os.environ
assert "sitecustomize.py" not in os.environ["PYTHONPATH"]
assert os.environ["PYTHONPATH"] == os.environ["TEST_PYTHONPATH"]

def check():
    assert call_tool("probe", {"command":"allow"}) == "allow"
    try:
        call_tool("probe", {"command":"deny"})
    except AwError as error:
        assert error.code == "source_denied", error
    else:
        raise AssertionError("denial did not reach application")

async def async_check():
    assert await call_tool_async("probe", {"command":"allow"}) == "allow"
    try:
        await call_tool_async("probe", {"command":"deny"})
    except AwError as error:
        assert error.code == "source_denied", error
    else:
        raise AssertionError("coroutine bypassed denial")

async def explicit():
    await async_check()
    async with preloaded_profile_source.request():
        check()
        await async_check()

if len(sys.argv) == 1:
    asyncio.run(explicit())
    check()
    child = subprocess.run([sys.executable, "-c", "import os; assert 'AW_PROFILE_BOOTSTRAP' not in os.environ"])
    assert child.returncode == 0
elif sys.argv[1] == "exit":
    p = subprocess.Popen(["/bin/sleep", "60"])
    open("descendant", "w").write(str(p.pid))
    os._exit(3)
else:
    for request in sys.stdin:
        if request.strip() == "quit":
            break
        try:
            check()
        except (TimeoutError, AwError) as error:
            assert isinstance(error, TimeoutError) or error.code == "instance_not_ready", error
            print("detached", flush=True)
        else:
            print("protected", flush=True)
"""

NODE = """
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import readline from "node:readline";
import { execFileSync, spawn } from "node:child_process";
import { writeFileSync } from "node:fs";

const { callTool } = createRequire(import.meta.url)("aw-profile-fixture");
assert.equal(globalThis.appPreloaded, true);
assert.equal(process.env.AW_PROFILE_BOOTSTRAP, undefined);
assert.equal(process.env.NODE_OPTIONS, process.env.TEST_NODE_OPTIONS);
function check() {
  assert.equal(callTool("probe", { command: "allow" }), "allow");
  assert.throws(() => callTool("probe", { command: "deny" }), {
    name: "SourceDenied",
  });
}
if (!process.argv[2]) {
  check();
  execFileSync(process.execPath, ["-e", "if (process.env.AW_PROFILE_BOOTSTRAP) process.exit(1)"]);
} else if (process.argv[2] === "exit") {
  const child = spawn("/bin/sleep", ["60"], { stdio: "ignore" });
  writeFileSync("descendant", String(child.pid));
  process.exit(3);
} else {
  const input = readline.createInterface({ input: process.stdin });
  for await (const request of input) {
    if (request === "quit") break;
    try {
      check();
      console.log("protected");
    } catch (error) {
      assert(error.name === "AbortError" || error.code === "ABORT_ERR", error);
      console.log("detached");
    }
  }
}
"""


def call(
    root: Path, env: dict[str, str], *args: str, ok: bool = True
) -> subprocess.CompletedProcess:
    result = subprocess.run(
        [CLI, *args],
        check=False,
        env=env,
        cwd=root,
        capture_output=True,
        text=True,
        timeout=20,
    )
    assert (result.returncode == 0) is ok, result
    return result


def config(root: Path, runtime: str, command: list[str]) -> dict:
    policy = root / "policy.py"
    policy.write_text(POLICY)
    return {
        "apiVersion": "aw/v1alpha1",
        "kind": "AWConfiguration",
        "metadata": {"name": "instrumentation"},
        "spec": {
            "daemon": {
                "startup": "on_demand",
                "endpoint": "auto",
                "state_dir": str(root / "state"),
            },
            "execution": {"guarantee": "native_hook", "default_event_budget_ms": 1000},
            "audit": {"enabled": True, "payload": "metadata_only"},
            "agents": {
                "app": {
                    "adapter": "sdk",
                    "argv": command,
                    "sdk": {"profiles": [f"fixture.{runtime}"]},
                }
            },
            "providers": {
                "policy": {
                    "protocol": "aw-provider/v1alpha1",
                    "timeout_ms": 1000,
                    "max_output_bytes": 4096,
                    "config": {},
                    "transport": {
                        "type": "stdio",
                        "location": "agent",
                        "argv": [sys.executable, str(policy)],
                    },
                }
            },
            "events": {
                "tool.before": {
                    "enabled": True,
                    "required": True,
                    "steps": [
                        {
                            "id": "policy",
                            "provider": "policy",
                            "operation": "check",
                            "effects": ["observe", "block"],
                            "on_error": "block",
                        }
                    ],
                }
            },
        },
    }


def python(root: Path, env: dict[str, str]) -> list[str]:
    package = root / "aw_profile_fixture-1.0.0.dist-info"
    package.mkdir()
    (package / "METADATA").write_text("Name: aw-profile-fixture\nVersion: 1.0.0\n")
    (root / "aw_profile_fixture.py").write_text(
        'def call_tool(name, args):\n    assert args["command"] != "deny"\n    return args["command"]\n'
        'async def call_tool_async(name, args):\n    assert args["command"] != "deny"\n    return args["command"]\n'
        "def observe_tool(name, args):\n    return args\n"
    )
    (root / "sitecustomize.py").write_text(
        'import os\nos.environ["TEST_SITE_CUSTOMIZE"] = "loaded"\n'
    )
    env["PYTHONPATH"] = os.pathsep.join((str(root), str(SDK / "python/src")))
    env["TEST_PYTHONPATH"] = env["PYTHONPATH"]
    (root / "app.py").write_text(PYTHON)
    return [sys.executable, "app.py"]


def node(root: Path, env: dict[str, str]) -> list[str]:
    package = root / "node_modules/aw-profile-fixture"
    package.mkdir(parents=True)
    (package / "package.json").write_text(
        '{"name":"aw-profile-fixture","version":"1.0.0","main":"index.cjs"}'
    )
    (package / "index.cjs").write_text(
        'exports.callTool = (name, args) => { if(args.command === "deny") throw Error("side effect"); return args.command; };'
    )
    sdk = root / "node_modules/@anolisa/aw-sdk"
    (sdk / "dist").mkdir(parents=True)
    shutil.copyfile(SDK / "typescript/package.json", sdk / "package.json")
    shutil.copytree(SDK / "typescript/src", sdk / "dist", dirs_exist_ok=True)
    preload = root / "app preload.cjs"
    preload.write_text("globalThis.appPreloaded = true;")
    env["NODE_OPTIONS"] = f'--require "{preload}"'
    env["TEST_NODE_OPTIONS"] = env["NODE_OPTIONS"]
    (root / "app.mjs").write_text(NODE)
    return ["node", "app.mjs"]


def ready_line(application: subprocess.Popen) -> str:
    readable, _, _ = select.select([application.stdout], [], [], 8)
    assert readable, "application did not respond before the deadline"
    line = application.stdout.readline().strip()
    if not line:
        _, stderr = application.communicate(timeout=2)
        raise AssertionError(stderr)
    return line


def resident(
    root: Path, env: dict[str, str], command: list[str], selector: list[str]
) -> None:
    setup = root / "setup"
    result = json.loads(
        call(
            root, env, "instance", "setup", *selector, "--directory", str(setup)
        ).stdout
    )
    assert stat.S_IMODE(setup.stat().st_mode) == 0o700
    for filename in result["files"]:
        assert stat.S_IMODE(Path(filename).stat().st_mode) == 0o400
    # A conflicting asset makes setup roll back every file already created in this attempt.
    occupied = root / "occupied"
    occupied.mkdir(mode=0o700)
    (occupied / "profile-bootstrap.json").write_text("application-owned")
    call(
        root,
        env,
        "instance",
        "setup",
        *selector,
        "--directory",
        str(occupied),
        ok=False,
    )
    assert sorted(path.name for path in occupied.iterdir()) == [
        "profile-bootstrap.json"
    ]
    assert (occupied / "profile-bootstrap.json").read_text() == "application-owned"

    bootstrap = Path(result["environment"]["AW_PROFILE_BOOTSTRAP"])
    original = bootstrap.read_text()
    metadata = json.loads(original)
    bootstrap.chmod(0o600)
    # Test strict JSON and the canonical profile digest through the real shims.
    invalid = json.loads(original)
    key = next(iter(invalid["profiles"]))
    payload = json.loads(invalid["profiles"][key])
    payload["targets"][0]["id"] = "changed"
    invalid["profiles"][key] = json.dumps(payload)
    for data, code in [
        ('{"profile":{},"profile":{}}', "source_profile"),
        (json.dumps(invalid), "source_profile_binding_mismatch"),
    ]:
        bootstrap.write_text(data)
        refused = subprocess.run(
            command,
            check=False,
            cwd=root,
            env=dict(env, **result["environment"]),
            capture_output=True,
            text=True,
            timeout=10,
        )
        assert (
            refused.returncode != 0 and refused.stdout == "" and code in refused.stderr
        ), refused
    metadata["timeoutMs"] = 1000
    bootstrap.write_text(json.dumps(metadata))
    bootstrap.chmod(0o400)
    with subprocess.Popen(
        [*command, "resident"],
        cwd=root,
        env=dict(env, **result["environment"]),
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    ) as application:
        try:
            connected = json.loads(
                call(
                    root,
                    env,
                    "connect",
                    *selector[:2],
                    "--sdk-pid",
                    str(application.pid),
                ).stdout
            )
            for _ in range(2):
                application.stdin.write("call\n")
                application.stdin.flush()
                assert ready_line(application) == "protected"
                disconnected = json.loads(
                    call(
                        root,
                        env,
                        "disconnect",
                        *selector[:2],
                        "--sdk-instance",
                        connected["application_id"],
                    ).stdout
                )
                assert disconnected["result"]["state"] == "completed"
                application.stdin.write("call\n")
                application.stdin.flush()
                assert ready_line(application) == "detached"
                connected = json.loads(
                    call(
                        root,
                        env,
                        "connect",
                        *selector[:2],
                        "--sdk-pid",
                        str(application.pid),
                    ).stdout
                )
            stdout, stderr = application.communicate("quit\n", timeout=10)
            assert (application.returncode, stdout, stderr) == (0, "", "")
        finally:
            if application.poll() is None:
                os.killpg(application.pid, 9)
                _, stderr = application.communicate(timeout=5)
                sys.stderr.write(stderr)


def scenario(root: Path, runtime: str) -> None:
    env = dict(os.environ, HOME=str(root), XDG_RUNTIME_DIR=str(root))
    command = (python if runtime == "python" else node)(root, env)
    document = config(root, runtime, command)
    config_path = root / "aw.json"
    config_path.write_text(json.dumps(document))
    config_path.chmod(0o600)
    selector = ["--config", str(config_path), "--agent", "app"]
    try:
        descriptor = json.loads(
            call(root, env, "config", "descriptor", *selector).stdout
        )
        explicit = call(
            root,
            env,
            "config",
            "descriptor",
            *selector,
            "--profile",
            "application-hooks",
        )
        assert json.loads(explicit.stdout) == descriptor
        binding = descriptor["instrumentation_profiles"]
        assert binding["runtime"] == runtime
        plan = json.loads(call(root, env, "plan", *selector).stdout)["hook_plan"]
        assert plan["hooks"]["tool.before"]["control_owner"]["kind"] == runtime, plan
        output = call(root, env, "run", *selector)
        assert output.stdout == "", output
        failed = call(root, env, "run", *selector, "--", "exit", ok=False)
        assert "source_cleanup_unconfirmed" in failed.stderr, failed
        pid = (root / "descendant").read_text()
        status = Path("/proc") / pid / "stat"
        limit = time.monotonic() + 3
        while (
            status.exists() and status.read_text().rsplit(")", 1)[1].split()[0] != "Z"
        ):
            assert time.monotonic() < limit, "descendant survived its foreground owner"
            time.sleep(0.02)
        assert not list(
            (root / "state").glob("launch-*")
        ), "bootstrap files survived their owner"
        resident(root, env, command, selector)
        descriptor = json.loads((root / "setup/deployment.json").read_text())
        assert (
            descriptor["configuration_sha256"]
            == hashlib.sha256(config_path.read_bytes()).hexdigest()
        )
        if runtime == "python":
            profiles = document["spec"]["agents"]["app"]["sdk"]["profiles"]
            for invalid in [
                "fixture.js",
                "fixture.python",
                "/tmp/custom.json",
                "missing",
            ]:
                profiles.append(invalid)
                config_path.write_text(json.dumps(document))
                call(root, env, "config", "descriptor", *selector, ok=False)
                profiles.pop()
            profiles.append("fixture.second")
            config_path.write_text(json.dumps(document))
            rejected = call(root, env, "config", "descriptor", *selector, ok=False)
            assert "requested effect" in rejected.stderr, rejected
    finally:
        endpoint = root / "state/aw.sock"
        if endpoint.exists():
            call(root, env, "service", "stop", "--socket", str(endpoint))


for language in ("python", "js"):
    with tempfile.TemporaryDirectory(prefix=f"aw-p4b-{language}-") as directory:
        scenario(Path(directory), language)
