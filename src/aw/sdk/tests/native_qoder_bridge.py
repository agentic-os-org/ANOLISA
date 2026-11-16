"""Build the candidate package and verify hosted public-Hook adoption in a private HOME."""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser()
p.add_argument("--bin", type=Path, required=True)
p.add_argument("--npm", type=Path, required=True)
p.add_argument("--output", type=Path, required=True)
a = p.parse_args()
a.output.mkdir(parents=True, exist_ok=True)
sdk = Path(__file__).resolve().parents[1] / "typescript"
subprocess.run(["node", "scripts/build.mjs"], cwd=sdk, check=True, timeout=30)
with tempfile.TemporaryDirectory(prefix="aw-qoder-bridge-") as directory:
    home = Path(directory)
    env = dict(os.environ, HOME=str(home))
    subprocess.run(
        [
            "node", str(Path(__file__).with_name("native-qoder-bridge.mjs")),
            str(a.bin.resolve() / "aw"),
            str(a.npm.resolve() / "node_modules/@qoder-ai/qoder-agent-sdk/dist/index.js"),
            str(sdk / "dist/qoder.mjs"), str(a.output.resolve()),
        ],
        env=env, check=True, timeout=240,
    )
