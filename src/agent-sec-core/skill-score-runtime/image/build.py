"""Explicit maintainer build: image, inventory and offline Docker archive."""

import argparse
import hashlib
import json
import shutil
import subprocess
import tempfile
from pathlib import Path

from agent_sec_skill_score_runtime.images import sidecar_image


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    parser.add_argument("--build-network", choices=("default", "host"), default="default")
    args = parser.parse_args()
    component = Path(__file__).resolve().parents[2]
    image = "agent-sec-skill-score:1"
    args.output.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="skill-score-image-") as temporary:
        context = Path(temporary)
        for name in ("Dockerfile", "observe.py", "claude-observed"):
            shutil.copy2(Path(__file__).parent / name, context / name)
        source = component / "agent-sec-cli/src/agent_sec_cli"
        target = context / "scanner-source/agent_sec_cli"
        target.mkdir(parents=True)
        shutil.copy2(source / "__init__.py", target / "__init__.py")
        # Only genuine code-scanner modules needed by the subject scanner Skill, not CLI/ML.
        for name in ("code_scanner", "model_service"):
            shutil.copytree(
                source / name, target / name, ignore=shutil.ignore_patterns("__pycache__")
            )
        subprocess.run(
            [
                "docker",
                "build",
                "--network",
                args.build_network,
                "--pull=false",
                "-t",
                image,
                str(context),
            ],
            check=True,
        )
    sidecar = sidecar_image()
    subprocess.run(
        [
            "docker",
            "build",
            "--network",
            args.build_network,
            "--pull=false",
            "-t",
            sidecar,
            str(component / "skill-score-runtime/egress"),
        ],
        check=True,
    )
    output = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--network",
            "none",
            "--entrypoint",
            "python",
            image,
            "/opt/poc/observe.py",
            "inventory",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    inventory = json.loads(output.stdout)
    inventory.update(
        docker_image=image,
        network="ASC static model-host allowlist for proxied TCP; local destinations and ICMP remain direct",
        mock="http://127.0.0.1:8765; static routes, no downstream model inference",
    )
    text = json.dumps(inventory, ensure_ascii=False, indent=2) + "\n"
    (args.output / "tool-inventory.json").write_text(text)
    subprocess.run(
        ["docker", "save", "-o", str(args.output / "skill-score-images.tar"), image, sidecar],
        check=True,
    )
    details = json.loads(
        subprocess.run(
            ["docker", "image", "inspect", image, sidecar],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    claude = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--network",
            "none",
            "--entrypoint",
            "/usr/bin/claude",
            image,
            "--version",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    gost = subprocess.run(
        [
            "docker",
            "run",
            "--rm",
            "--pull",
            "never",
            "--network",
            "none",
            "--entrypoint",
            "/bin/gost",
            sidecar,
            "-V",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    (args.output / "images.json").write_text(
        json.dumps(
            {
                "images": [image, sidecar],
                "image_details": [
                    {
                        "name": name,
                        "id": detail["Id"],
                        "repo_digests": detail.get("RepoDigests", []),
                    }
                    for name, detail in zip((image, sidecar), details, strict=True)
                ],
                "versions": {
                    "harbor": "0.22.0",
                    "claude": claude.stdout.strip(),
                    "gost": gost.stdout.strip(),
                    "observer_sha256": hashlib.sha256(
                        (Path(__file__).parent / "observe.py").read_bytes()
                    ).hexdigest(),
                },
            },
            indent=2,
        )
        + "\n"
    )


if __name__ == "__main__":
    main()
