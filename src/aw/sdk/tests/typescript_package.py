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
        consumer = root / "consumer.mjs"
        consumer.write_text("""import assert from 'node:assert/strict';
import { Instance } from '@anolisa/aw-sdk';
import { qoderOptions, createQoderBridge } from '@anolisa/aw-sdk/qoder';
assert.equal(typeof createQoderBridge,'function');
const instance = new Instance({endpoint: process.argv[2], target:'app',
  availability:'native_fallback', capabilities:{}, install:()=>true});
assert.equal(await instance.connect(),false);
const task=await instance.task();
const native={model:'application-model', hooks:{}};
assert.equal(qoderOptions(task,native).options.model,native.model);
await task.close(); await instance.close();
""")
        run(["node", str(consumer), str(root / "missing.sock")], root, environment)
    print(json.dumps({"status": "passed", "package": "typescript"}))


if __name__ == "__main__":
    main(sys.argv[1])
