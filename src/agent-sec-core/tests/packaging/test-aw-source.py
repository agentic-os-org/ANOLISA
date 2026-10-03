#!/usr/bin/env python3
"""Verify that Source0 carries the AW dependency without the parent checkout."""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path


def run(
    command: list[str],
    cwd: Path,
    timeout: int = 120,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(
            command, cwd=cwd, env=env, check=True, capture_output=True, timeout=timeout
        )
    except subprocess.CalledProcessError as error:
        sys.stderr.buffer.write(error.stderr)
        raise


def check(repository: Path, output: Path) -> None:
    output.mkdir(parents=True, exist_ok=True)
    original_manifest = repository / "src/agent-sec-core/v2/Cargo.toml"
    original_digest = hashlib.sha256(original_manifest.read_bytes()).hexdigest()
    with tempfile.TemporaryDirectory(prefix="source0-", dir=output) as temporary:
        root = Path(temporary)
        source = root / "stage/agent-sec-core-0.13.2"
        source.mkdir(parents=True)
        # Copy the actual component source tree as package-source does, omitting
        # generated files up front rather than copying gigabytes then deleting.
        shutil.copytree(
            repository / "src/agent-sec-core",
            source,
            dirs_exist_ok=True,
            ignore=shutil.ignore_patterns(
                ".venv",
                "node_modules",
                "target",
                "dist",
                "__pycache__",
                ".pytest_cache",
                ".ruff_cache",
            ),
            symlinks=True,
        )
        staging_environment = os.environ.copy()
        staging_environment.update(
            GIT_TEST_ASSUME_DIFFERENT_OWNER="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_SYSTEM=os.devnull,
        )
        untrusted = subprocess.run(
            ["git", "ls-files", "-z", "src/aw"],
            cwd=repository,
            env=staging_environment,
            capture_output=True,
            check=False,
            timeout=15,
        )
        assert untrusted.returncode == 128
        assert b"dubious ownership" in untrusted.stderr
        run(
            [
                sys.executable,
                str(repository / "src/agent-sec-core/scripts/package-aw-source.py"),
                str(repository),
                str(source),
            ],
            repository,
            env=staging_environment,
        )
        archive = root / "source0.tar.gz"
        with tarfile.open(archive, "w:gz") as stream:
            stream.add(source, arcname=source.name)
        shutil.rmtree(source.parent)
        unpacked = root / "unpacked"
        unpacked.mkdir()
        with tarfile.open(archive) as stream:
            # This archive contains only our own checked source files. Links
            # elsewhere in sec-core retain their original source layout.
            stream.extractall(unpacked)
        workspace = unpacked / source.name / "v2"
        metadata = json.loads(
            run(
                [
                    "cargo",
                    "+1.93.0",
                    "metadata",
                    "--locked",
                    "--offline",
                    "--format-version",
                    "1",
                ],
                workspace,
            ).stdout
        )
        local = [
            package for package in metadata["packages"] if package["source"] is None
        ]
        for package in local:
            if not Path(package["manifest_path"]).is_relative_to(unpacked):
                raise AssertionError(
                    f"source package escaped Source0: {package['name']}"
                )
        if not {"aw-provider", "aw-config"}.issubset(
            {package["name"] for package in local}
        ):
            raise AssertionError("AW contract source packages missing")
        environment = os.environ.copy()
        # Reuse only build output. All source paths still resolve inside Source0.
        environment["CARGO_TARGET_DIR"] = str(
            repository / "src/agent-sec-core/v2/target"
        )
        build = subprocess.run(
            ["cargo", "+1.93.0", "build", "--offline", "--locked", "-p", "asc-cli"],
            cwd=workspace,
            env=environment,
            capture_output=True,
            check=True,
            timeout=600,
        )
        (output / "source0-build.log").write_bytes(build.stdout + build.stderr)
        binary = Path(environment["CARGO_TARGET_DIR"]) / "debug/agent-sec-cli"
        request = b'{"api_version":"aw-provider/v1alpha1","method":"describe","request_id":"source0"}'
        response = subprocess.run(
            [str(binary), "aw-provider"],
            input=request,
            cwd=workspace,
            capture_output=True,
            check=True,
            timeout=10,
        )
        value = json.loads(response.stdout)
        assert value["request_id"] == "source0"
        assert value["operations"][0]["name"] == "scan_code"
        assert not response.stderr
        (output / "source0-result.json").write_text(
            json.dumps(
                {
                    "local_source_packages": len(local),
                    "container_checkout_ownership": "passed",
                    "offline_build": "passed",
                    "provider_describe": value,
                    "source0_removed": True,
                },
                indent=2,
            )
            + "\n"
        )
    assert hashlib.sha256(original_manifest.read_bytes()).hexdigest() == original_digest
    assert not root.exists()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    repository = Path(__file__).resolve().parents[4]
    check(repository, args.output.resolve())


if __name__ == "__main__":
    main()
