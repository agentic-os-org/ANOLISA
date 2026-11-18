"""Build and install the SDK outside the repository without network or Agent packages."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import shutil
from semantic_contract import run as semantic
from event_contract import run as events


def run(command: list[str], cwd: Path, environment: dict[str, str]) -> str:
    result = subprocess.run(
        command,
        cwd=cwd,
        env=environment,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=90,
    )
    if result.returncode:
        raise RuntimeError(result.stdout)
    return result.stdout


def main(binary: str) -> None:
    package = Path(__file__).resolve().parents[1] / "typescript"
    environment = dict(os.environ, AW_TEST_BINARY=str(Path(binary).resolve()))
    run(
        ["node", "--test", *map(str, sorted((package / "tests").glob("*.test.mjs")))],
        package,
        environment,
    )
    with tempfile.TemporaryDirectory(prefix="aw-sdk-package-") as temporary:
        root = Path(temporary)
        # npm's cache stays private and needs no registry access for this tarball.
        environment["npm_config_cache"] = str(root / "cache")
        run(["npm", "pack", "--pack-destination", str(root)], package, environment)
        (artifact,) = root.glob("*.tgz")
        run(
            [
                "npm",
                "install",
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--prefix",
                str(root),
                str(artifact),
            ],
            root,
            environment,
        )
        semantic_consumer = root / "semantic-consumer.mjs"
        shutil.copyfile(package.parent / "tests/semantic-consumer.mjs", semantic_consumer)
        semantic(binary, ["node", str(semantic_consumer)], root, environment)
        event_consumer = root / "event-consumer.mjs"
        shutil.copyfile(package.parent / "tests/event-consumer.mjs", event_consumer)
        events(binary, ["node", str(event_consumer)], root, environment)
        deployment = json.loads((package.parent / "tests/fixtures/deployment.json").read_text())[
            "configuration"
        ]
        deployment["spec"]["daemon"]["state_dir"] = str(root / "state")
        config = root / "deployment-config.json"
        config.write_text(json.dumps(deployment))
        config.chmod(0o600)
        descriptor = root / "deployment.json"
        descriptor.write_text(
            run(
                [
                    str(Path(binary).resolve()),
                    "config",
                    "descriptor",
                    "--config",
                    str(config),
                    "--agent",
                    "app",
                    "--profile",
                    "qoder-public-hooks",
                ],
                root,
                environment,
            )
        )
        descriptor.chmod(0o600)
        pi_deployment = json.loads((package.parent / "tests/fixtures/deployment.json").read_text())[
            "configuration"
        ]
        pi_deployment["spec"]["agents"]["app"]["adapter"] = "pi"
        pi_deployment["spec"]["daemon"]["state_dir"] = str(root / "state")
        pi_config = root / "pi-config.json"
        pi_config.write_text(json.dumps(pi_deployment))
        pi_config.chmod(0o600)
        pi_descriptor = root / "pi-deployment.json"
        pi_descriptor.write_text(
            run(
                [
                    str(Path(binary).resolve()),
                    "config",
                    "descriptor",
                    "--config",
                    str(pi_config),
                    "--agent",
                    "app",
                    "--profile",
                    "pi-worker-hooks",
                ],
                root,
                environment,
            )
        )
        pi_descriptor.chmod(0o600)
        pi_worker = root / "pi-worker.mjs"
        pi_worker.write_text("export function createSession() {}\n")
        consumer = root / "consumer.mjs"
        consumer.write_text("""import assert from 'node:assert/strict';
import { Instance } from '@anolisa/aw-sdk';
import { runPiWorker } from '@anolisa/aw-sdk/pi';
import { qoderOptions, createQoderBridge } from '@anolisa/aw-sdk/qoder';
assert.equal(typeof createQoderBridge,'function');
assert.equal(typeof runPiWorker,'function');
const instance = new Instance({endpoint: process.argv[2], target:'app',
  availability:'native_fallback', capabilities:{}, install:()=>true});
assert.equal(await instance.connect(),false);
const task=await instance.task();
const native={model:'application-model', hooks:{}};
assert.equal(qoderOptions(task,native).options.model,native.model);
await task.close(); await instance.close();
const bridge = await createQoderBridge({configFile:process.argv[3], descriptorFile:process.argv[4],
  availability:'required', executable:'/missing/aw',
  query(){ throw new Error('native Query must not start'); }});
await bridge.close();
await assert.rejects(
  runPiWorker({application:process.argv[5], configFile:process.argv[6],
    descriptorFile:process.argv[7], executable:'/missing/aw'}),
  /Cannot resolve installed @earendil-works\\/pi-coding-agent/,
);
""")
        run(
            [
                "node",
                str(consumer),
                str(root / "missing.sock"),
                str(config),
                str(descriptor),
                str(pi_worker),
                str(pi_config),
                str(pi_descriptor),
            ],
            root,
            environment,
        )
    print(json.dumps({"status": "passed", "package": "typescript"}))


if __name__ == "__main__":
    main(sys.argv[1])
