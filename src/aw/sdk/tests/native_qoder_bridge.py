"""Build candidate core + Qoder packages and verify hosted public-Hook adoption."""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile

p = argparse.ArgumentParser()
p.add_argument("--bin", type=Path, required=True)
p.add_argument("--npm", type=Path, required=True)
p.add_argument("--output", type=Path, required=True)
p.add_argument("--warm-post", action="store_true")
a = p.parse_args()
a.output.mkdir(mode=0o700, parents=True, exist_ok=True)
sdk_root = Path(__file__).resolve().parents[1]
with tempfile.TemporaryDirectory(prefix="aw-qoder-bridge-") as directory:
    home = Path(directory)
    env = dict(os.environ, HOME=str(home), npm_config_cache=str(home / "npm-cache"))
    tarballs = []
    for rel in ("typescript", "typescript-adapters/qoder"):
        out = home / ("pack-" + Path(rel).name)
        out.mkdir(parents=True, exist_ok=True)
        subprocess.run(
            ["npm", "pack", "--pack-destination", str(out)],
            cwd=sdk_root / rel,
            env=env,
            check=True,
            timeout=30,
        )
        (tarball,) = out.glob("*.tgz")
        tarballs.append(str(tarball))
    app = home / "app"
    app.mkdir()
    subprocess.run(
        [
            "npm",
            "install",
            "--offline",
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
            "--prefix",
            str(app),
            *tarballs,
        ],
        cwd=app,
        env=env,
        check=True,
        timeout=60,
    )
    subprocess.run(
        [
            "node",
            str(Path(__file__).with_name("native-qoder-bridge.mjs")),
            str(a.bin.resolve() / "aw"),
            str(a.npm.resolve() / "node_modules/@qoder-ai/qoder-agent-sdk/dist/index.js"),
            str(app / "node_modules/@anolisa/aw-sdk-qoder/dist/index.mjs"),
            str(app / "node_modules/@anolisa/aw-sdk/dist/client.mjs"),
            str(a.output.resolve()),
            *(["warm-post"] if a.warm_post else []),
        ],
        env=env,
        check=True,
        timeout=240,
    )
