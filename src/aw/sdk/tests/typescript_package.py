"""Build and install core and optional TypeScript SDK packages outside the repository."""

import concurrent.futures
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
from semantic_contract import run as semantic
from event_contract import run as events
from source_contract import run as language_source


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


def pack(package: Path, destination: Path, environment: dict[str, str]) -> Path:
    destination.mkdir(parents=True, exist_ok=True)
    run(["npm", "pack", "--pack-destination", str(destination)], package, environment)
    (artifact,) = destination.glob("*.tgz")
    return artifact


def assert_core_tarball_boundary(artifact: Path) -> None:
    with tarfile.open(artifact, "r:gz") as archive:
        names = [member.name for member in archive.getmembers() if member.isfile()]
    forbidden = [
        name
        for name in names
        if name.startswith(
            ("package/dist/native/qoder-sdk/", "package/dist/native/pi/")
        )
    ]
    assert not forbidden, f"core npm package must not bundle framework adapters: {forbidden}"
    for required in (
        "package/dist/client.mjs",
        "package/dist/admission.mjs",
        "package/dist/task-scope.mjs",
        "package/dist/sources.mjs",
        "package/dist/sources.d.ts",
        "package/dist/native/shared/scope.mjs",
        "package/dist/native/shared/transport.mjs",
    ):
        assert required in names, f"core npm package missing {required}"


def main(binary: str) -> None:
    sdk = Path(__file__).resolve().parents[1]
    core_package = sdk / "typescript"
    adapters = sdk / "typescript-adapters"
    environment = dict(os.environ, AW_TEST_BINARY=str(Path(binary).resolve()))
    with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
        test_futures = [
            pool.submit(
                run,
                ["node", "--test", *map(str, sorted(test_dir.glob("*.test.mjs")))],
                test_dir.parent,
                environment,
            )
            for test_dir in (
                core_package / "tests",
                adapters / "qoder/tests",
                adapters / "pi/tests",
            )
        ]
        for future in test_futures:
            future.result()
    with tempfile.TemporaryDirectory(prefix="aw-sdk-package-") as temporary:
        root = Path(temporary)
        # npm's cache stays private and needs no registry access for these tarballs.
        environment["npm_config_cache"] = str(root / "cache")
        with concurrent.futures.ThreadPoolExecutor(max_workers=3) as pool:
            core_f = pool.submit(pack, core_package, root / "pack-core", environment)
            qoder_f = pool.submit(pack, adapters / "qoder", root / "pack-qoder", environment)
            pi_f = pool.submit(pack, adapters / "pi", root / "pack-pi", environment)
            core_artifact = core_f.result()
            qoder_artifact = qoder_f.result()
            pi_artifact = pi_f.result()
        assert_core_tarball_boundary(core_artifact)
        core_root = root / "core-app"
        core_root.mkdir(mode=0o700)
        run(
            [
                "npm",
                "install",
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--prefix",
                str(core_root),
                str(core_artifact),
            ],
            core_root,
            environment,
        )
        # Core-only install passes semantic, event and language-source contracts and gives
        # clear installation guidance when optional framework adapters are absent.
        semantic_consumer = core_root / "semantic-consumer.mjs"
        shutil.copyfile(sdk / "tests/semantic-consumer.mjs", semantic_consumer)
        event_consumer = core_root / "event-consumer.mjs"
        shutil.copyfile(sdk / "tests/event-consumer.mjs", event_consumer)
        source_consumer = core_root / "source-consumer.mjs"
        shutil.copyfile(sdk / "tests/source-consumer.mjs", source_consumer)

        missing_check = core_root / "missing-adapters.mjs"
        missing_check.write_text(
            """import assert from 'node:assert/strict';
import { Instance } from '@anolisa/aw-sdk';
import { LanguageSource } from '@anolisa/aw-sdk/sources';
import { runPiWorker } from '@anolisa/aw-sdk/pi';
import { qoderOptions, createQoderBridge } from '@anolisa/aw-sdk/qoder';
assert.equal(typeof Instance, 'function');
assert.equal(typeof LanguageSource, 'function');
assert.throws(
  () => qoderOptions({}, {}),
  (error) => error.code === 'optional_adapter_not_installed' && error.message.includes('@anolisa/aw-sdk-qoder'),
);
await assert.rejects(
  createQoderBridge({ configFile: '/tmp/aw.json', query() {} }),
  (error) => error.code === 'optional_adapter_not_installed' && error.message.includes('@anolisa/aw-sdk-qoder'),
);
await assert.rejects(
  runPiWorker({ application: '/tmp/worker.mjs', configFile: '/tmp/aw.json' }),
  (error) => error.code === 'optional_adapter_not_installed' && error.message.includes('@anolisa/aw-sdk-pi'),
);
"""
        )
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            contract_futures = [
                pool.submit(
                    semantic, binary, ["node", str(semantic_consumer)], core_root, environment
                ),
                pool.submit(
                    events, binary, ["node", str(event_consumer)], core_root, environment
                ),
                pool.submit(
                    language_source, binary, ["node", str(source_consumer)], core_root, environment
                ),
                pool.submit(run, ["node", str(missing_check)], core_root, environment),
            ]
            for future in contract_futures:
                future.result()

        # Install optional framework adapter tarballs alongside core and verify compatibility.
        run(
            [
                "npm",
                "install",
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                "--prefix",
                str(core_root),
                str(qoder_artifact),
                str(pi_artifact),
            ],
            core_root,
            environment,
        )
        deployment = json.loads((sdk / "tests/fixtures/deployment.json").read_text())[
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
        pi_deployment = json.loads((sdk / "tests/fixtures/deployment.json").read_text())[
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
        consumer = core_root / "consumer.mjs"
        consumer.write_text(
            """import assert from 'node:assert/strict';
import { Instance } from '@anolisa/aw-sdk';
import { runPiWorker } from '@anolisa/aw-sdk/pi';
import { runPiWorker as directPiWorker } from '@anolisa/aw-sdk-pi';
import { qoderOptions, createQoderBridge } from '@anolisa/aw-sdk/qoder';
import {
  qoderOptions as directQoderOptions,
  createQoderBridge as directQoderBridge,
} from '@anolisa/aw-sdk-qoder';
assert.equal(typeof createQoderBridge, 'function');
assert.equal(typeof directQoderBridge, 'function');
assert.equal(typeof runPiWorker, 'function');
assert.equal(typeof directPiWorker, 'function');
const instance = new Instance({endpoint: process.argv[2], target:'app',
  availability:'native_fallback', capabilities:{}, install:()=>true});
assert.equal(await instance.connect(),false);
const task=await instance.task();
const native={model:'application-model', hooks:{}};
assert.equal(qoderOptions(task,native).options.model,native.model);
assert.equal(directQoderOptions(task,native).options.model,native.model);
await task.close(); await instance.close();
for (const factory of [createQoderBridge, directQoderBridge]) {
  const bridge = await factory({configFile:process.argv[3], descriptorFile:process.argv[4],
    availability:'required', executable:'/missing/aw',
    query(){ throw new Error('native Query must not start'); }});
  await bridge.close();
}
for (const runner of [runPiWorker, directPiWorker]) {
  await assert.rejects(
    runner({application:process.argv[5], configFile:process.argv[6],
      descriptorFile:process.argv[7], executable:'/missing/aw', automatic:true}),
    /Cannot resolve installed @earendil-works\\/pi-coding-agent/,
  );
}
"""
        )
        run(
            [
                "node",
                # Exercise Node 22.0–22.11 ESM loading on current CI runtimes.
                "--no-experimental-require-module",
                str(consumer),
                str(root / "missing.sock"),
                str(config),
                str(descriptor),
                str(pi_worker),
                str(pi_config),
                str(pi_descriptor),
            ],
            core_root,
            environment,
        )
    print(json.dumps({"status": "passed", "package": "typescript"}))


if __name__ == "__main__":
    main(sys.argv[1])
