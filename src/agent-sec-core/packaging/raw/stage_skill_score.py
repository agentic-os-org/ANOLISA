#!/usr/bin/env python3
"""Add assessment capability to an explicitly selected preview raw payload."""

import argparse
import json
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import tomllib
from pathlib import Path

SCORE_ROOT = Path("lib/anolisa/sec-core/skill-score")


def stage_score(stage: Path, payload: Path, version: str) -> None:
    """Keep the V2 entrypoint and all optional files under installer ownership."""
    contract_path = stage / ".anolisa/component.toml"
    contract_text = contract_path.read_text()
    contract = tomllib.loads(contract_text)
    base = contract["component"]["version"]
    if not re.fullmatch(
        re.escape(base) + r"-skillscore\.[a-f0-9]{7,40}(?:\.base\.[a-f0-9]{12})?",
        version,
    ):
        raise ValueError(
            f"preview version must be {base}-skillscore.<7-40 character Git SHA>"
        )
    score_root = stage / SCORE_ROOT
    launcher = stage / "bin/agent-sec-skill-score"
    if score_root.exists() or launcher.exists():
        raise ValueError("assessment payload is already staged")
    component = Path(__file__).resolve().parents[2]
    subprocess.run(
        [
            sys.executable,
            str(component / "packaging/skill-score/verify-release.py"),
            "--payload-only",
            str(payload),
        ],
        check=True,
    )
    with tempfile.TemporaryDirectory(prefix="sec-core-score-stage-") as directory:
        extracted = Path(directory)
        with tarfile.open(payload) as archive:
            archive.extractall(extracted, filter="data")
        for kind, interpreter in (("cli", "python3.11"), ("runtime", "python3.12")):
            source = extracted / f"opt/agent-sec/skill-score-{kind}"
            if not (source / "bin" / interpreter).is_file():
                raise ValueError(f"assessment payload has no {kind} interpreter")
            shutil.copytree(source, score_root / kind, symlinks=False)
    private_cli = score_root / "cli/bin/agent-sec-cli"
    if not private_cli.is_file() or (score_root / "cli/bin/agent-sec-daemon").exists():
        raise ValueError(
            "assessment payload must expose a private CLI without a daemon"
        )
    # Raw installation owns regular files and their modes, never source symlinks.
    for path in score_root.rglob("*"):
        path.chmod(0o755 if path.is_dir() or path.stat().st_mode & 0o111 else 0o644)
    contract_text, count = re.subn(
        r'(?m)^version = "' + re.escape(base) + r'"$',
        f'version = "{version}"',
        contract_text,
        count=1,
    )
    if count != 1:
        raise ValueError("raw component version declaration was not found")
    contract_text += (
        "\n[[component.layout.files]]\n"
        f"source = {json.dumps(f'{SCORE_ROOT}/')}\n"
        'target = "{libdir}/sec-core/skill-score/"\n'
    )
    contract_path.write_text(contract_text)


def main() -> None:
    """Stage an already built payload without Docker or native compilation."""
    parser = argparse.ArgumentParser()
    parser.add_argument("stage", type=Path)
    parser.add_argument("payload", type=Path)
    parser.add_argument("version")
    args = parser.parse_args()
    stage_score(args.stage, args.payload, args.version)


if __name__ == "__main__":
    main()
