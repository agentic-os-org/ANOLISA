#!/usr/bin/env python3
"""Verify the native base and explicitly installed optional worker without a model."""

import argparse
import hashlib
import json
import os
import subprocess
import tempfile
import tomllib
from pathlib import Path


def run(arguments: list[str], **kwargs: object) -> subprocess.CompletedProcess:
    return subprocess.run(
        arguments, check=False, capture_output=True, text=True, timeout=180, **kwargs
    )


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("release", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--prefix", type=Path, default=Path("/usr/local"))
    parser.add_argument("--base-only", action="store_true")
    args = parser.parse_args()
    release = json.loads((args.release / "release-manifest.json").read_text())
    assert release["schema_version"] == "agent-sec-skill-score/release-v3"
    for artifact in release["artifacts"]:
        path = args.release / artifact["file"]
        with path.open("rb") as stream:
            assert (
                hashlib.file_digest(stream, "sha256").hexdigest() == artifact["sha256"]
            )
        assert path.stat().st_size == artifact["bytes"]
    contract = tomllib.loads(
        (args.prefix / "share/anolisa/components/sec-core/component.toml").read_text()
    )
    assert contract["component"]["version"] == release["version"]
    assert not (args.prefix / "lib/anolisa/sec-core/skill-score").exists()
    assert not (args.prefix / "bin/agent-sec-skill-score").exists()
    cli = args.prefix / "bin/agent-sec-cli"
    assert cli.read_bytes()[:4] == b"\x7fELF"
    checks = {
        "artifact_hashes": True,
        "native_cli": True,
        "base_has_no_score_python": True,
    }
    environment = {
        key: value
        for key, value in os.environ.items()
        if not key.startswith(("ANTHROPIC_", "AGENT_SEC_MODEL_"))
    }
    with tempfile.TemporaryDirectory(prefix="asc-native-install-check-") as directory:
        environment["AGENT_SEC_MODEL_CONFIG"] = str(
            Path(directory) / "absent-model-config.json"
        )
        for stage in ("plan", "run"):
            result = run(
                [str(cli), "skillsec", "score", stage, "--help"], env=environment
            )
            assert result.returncode == 0, result.stderr
        invalid = run(
            [str(cli), "skillsec", "score", "run", "-"], input="{}", env=environment
        )
        assert invalid.returncode == 2, invalid.stderr
        skill = Path(directory) / "harmless skill"
        skill.mkdir()
        (skill / "SKILL.md").write_text(
            "---\nname: harmless\ndescription: Installation check\n---\nPrint hello.\n"
        )
        planned = run(
            [str(cli), "skillsec", "score", "plan", str(skill)], env=environment
        )
        assert (
            planned.returncode != 0 and "model" in planned.stderr.lower()
        ), planned.stderr
        assert "install sec-core-skill-score" not in planned.stderr, planned.stderr
        checks["native_plan_reaches_shared_model_configuration"] = True
        checks["invalid_task_exit_2"] = True
    root = args.prefix / "lib/anolisa/sec-core-skill-score"
    if args.base_only:
        assert not root.exists()
        checks["optional_not_implicitly_installed"] = True
    else:
        manifest_path = root / "runtime-manifest.json"
        data = manifest_path.read_bytes()
        assert data == (args.release / release["runtime_manifest"]).read_bytes()
        runtime = json.loads(data)
        identity = hashlib.sha256(data).hexdigest()
        ready = json.loads(
            (root / "environments" / identity / "ready.json").read_text()
        )
        assert ready["manifest_sha256"] == identity
        assert ready["version"] == release["version"]
        assert ready["images"] == runtime["images"]
        lock = root / ".lifecycle.lock"
        assert lock.is_file() and not lock.is_symlink() and lock.stat().st_nlink == 1
        requests = root / "requests"
        assert requests.is_dir() and not requests.is_symlink()
        assert requests.stat().st_mode & 0o7777 == 0o1777
        assert not list(requests.iterdir()), "unconfirmed worker cleanup remains"
        generated = json.loads(
            (root / "environments" / identity / ".generated.json").read_text()
        )
        assert generated == {
            "schema": "agent-sec-skill-score/generated-v1",
            "component": "sec-core-skill-score",
            "manifest_sha256": identity,
        }
        result = run(
            [
                "python3",
                str(root / "install.py"),
                "--check",
                "--prefix",
                str(args.prefix),
            ]
        )
        assert result.returncode == 0, result.stderr
        readiness = json.loads(result.stdout)
        assert readiness["status"] == "ready"
        actual = readiness["actual_image_ids"]
        assert set(actual) == {"work", "egress"}
        assert all(
            actual[item["role"]] in (item["id"], item["manifest_id"])
            for item in runtime["images"]
        )
        checks["actual_image_ids"] = actual
        checks["optional_readiness_and_cleanup"] = True
        checks["optional_lifecycle_ready"] = True
        checks["runtime_manifest_sha256"] = identity
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps({"status": "PASS", "checks": checks}, indent=2) + "\n"
    )


if __name__ == "__main__":
    main()
