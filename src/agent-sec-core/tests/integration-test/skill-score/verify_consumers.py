"""Verify original consumer semantics inside a disposable, credential-free container."""

import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path


def check_validator(script: Path, root: Path, use_jq: bool, malicious: bool) -> dict:
    work = root / f"validator-{use_jq}-{malicious}"
    work.mkdir()
    project = work / "project"
    project.mkdir()
    (project / "file").write_text("ordinary input\n")
    config = {
        "checks": [{"pattern": "file", "type": "file", "min": 1}],
        "executable": [],
    }
    (work / "safe.json").write_text(json.dumps(config))
    filename = "safe.json"
    if malicious:
        filename = "safe.json', 'r') as f:\n    pass\nfrom pathlib import Path\nPath('marker').write_text('observed')\nwith open('safe.json"
        (work / filename).write_text(json.dumps(config))
    environment = dict(os.environ)
    if not use_jq:
        binaries = work / "bin"
        binaries.mkdir()
        for name in (
            "python3",
            "dirname",
            "find",
            "cat",
            "tr",
            "head",
            "grep",
            "readlink",
            "basename",
            "realpath",
            "wc",
        ):
            executable = shutil.which(name)
            if executable:
                (binaries / name).symlink_to(executable)
        environment["PATH"] = str(binaries)
    result = subprocess.run(
        ["/bin/bash", str(script), str(project), "--config", filename, "--json"],
        cwd=work,
        env=environment,
        capture_output=True,
        text=True,
        timeout=20,
        check=False,
    )
    positive = (work / "marker").exists()
    assert positive is (malicious and not use_jq)
    return {
        "original_script": str(script),
        "jq_present": use_jq,
        "malicious_filename": malicious,
        "marker_written": positive,
        "exit_code": result.returncode,
        "stdout": result.stdout[-1500:],
        "stderr": result.stderr[-1500:],
    }


def check_tar_default(root: Path) -> dict:
    destination = root / "tar-extract"
    destination.mkdir()
    archive = root / "archive.tar"
    with tarfile.open(archive, "w") as stream:
        member = tarfile.TarInfo("../tar-marker")
        member.size = 8
        stream.addfile(member, io.BytesIO(b"observed"))
    with tarfile.open(archive) as stream:
        stream.extractall(destination)
    return {
        "python": sys.version.split()[0],
        "default_extractall_allows_parent": (root / "tar-marker").exists(),
        "ar_available": shutil.which("ar") is not None,
        "scope": "Runtime default tar API behavior; not proof that the original Skill's ar-dependent branch was activated",
    }


def main() -> None:
    if os.environ.get("ANTHROPIC_API_KEY"):
        raise RuntimeError("Consumer contrasts must not receive a real model key")
    script = (
        Path(sys.argv[1])
        / "project-structure-validator/scripts/validate_project_structure.sh"
    )
    with tempfile.TemporaryDirectory(prefix="score-consumer-") as directory:
        root = Path(directory)
        print(
            json.dumps(
                {
                    "validator": [
                        check_validator(script, root, jq, malicious)
                        for jq, malicious in [
                            (False, True),
                            (True, True),
                            (False, False),
                        ]
                    ],
                    "deb_api": check_tar_default(root),
                },
                ensure_ascii=False,
                indent=2,
            )
        )


if __name__ == "__main__":
    main()
