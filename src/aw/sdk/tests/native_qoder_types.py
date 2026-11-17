"""Compile public bridge consumers against the pinned Qoder SDK's actual Query."""

import argparse
import json
from pathlib import Path
import shutil
import subprocess
import tempfile


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--npm", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    package = Path(__file__).resolve().parents[1] / "typescript"
    args.output.mkdir(parents=True, exist_ok=True)
    sdk = args.npm / "node_modules/@qoder-ai/qoder-agent-sdk"
    compiler = args.npm / "node_modules/typescript"
    sdk_version = json.loads((sdk / "package.json").read_text())["version"]
    compiler_version = json.loads((compiler / "package.json").read_text())["version"]
    assert sdk_version == "1.0.56" and compiler_version == "5.9.3"
    with tempfile.TemporaryDirectory(prefix="aw-sdk-types-", dir="/tmp") as temporary:
        root = Path(temporary)
        packed = subprocess.run(
            ["npm", "pack", "--pack-destination", str(root)],
            cwd=package,
            capture_output=True,
            text=True,
            timeout=30,
        )
        assert packed.returncode == 0, packed.stderr
        (tarball,) = root.glob("*.tgz")
        subprocess.run(
            [
                "npm",
                "install",
                "--prefix",
                str(root),
                "--offline",
                "--ignore-scripts",
                "--no-audit",
                "--no-fund",
                str(tarball),
            ],
            check=True,
            capture_output=True,
            timeout=30,
        )
        consumer = root / "consumer.mts"
        consumer.write_text(
            "import { query } from " + json.dumps(str(sdk / "dist/index.js")) + ";\n"
            'import { Instance } from "@anolisa/aw-sdk";\n'
            'import { createQoderBridge, qoderOptions } from "@anolisa/aw-sdk/qoder";\n'
            "export async function consumer(instance: Instance) {\n"
            ' const bridge = await createQoderBridge({configFile:"/tmp/aw.yaml", query, availability:"native_fallback"});\n'
            ' const session = bridge.query({prompt:"text"});\n'
            " await session.initializationResult();\n"
            " const task = await instance.task();\n"
            ' const native = qoderOptions(task).bind(query({prompt:"text"}));\n'
            " await native.initializationResult();\n"
            " return session;\n}\n"
        )
        result = subprocess.run(
            [
                "node",
                str(compiler / "bin/tsc"),
                "--strict",
                "--noEmit",
                "--skipLibCheck",
                "--module",
                "nodenext",
                "--target",
                "es2022",
                "--lib",
                "es2022,dom",
                str(consumer),
            ],
            capture_output=True,
            text=True,
            timeout=30,
        )
        (args.output / "tsc.log").write_text(result.stdout + result.stderr)
        shutil.copyfile(consumer, args.output / "consumer.mts")
        assert result.returncode == 0, result.stdout + result.stderr
    (args.output / "result.json").write_text(
        json.dumps(
            {
                "passed": True,
                "sdk": sdk_version,
                "typescript": compiler_version,
            }
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
