#!/usr/bin/env python3
"""Bind installation-local image outputs to verified source inputs."""

import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
import uuid
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import BinaryIO

SCHEMA = "agent-sec-skill-score/image-build-v1"


@contextmanager
def build_log(directory: Path) -> Iterator[BinaryIO]:
    # Source registries may emit credentials; expose only the owner-readable path.
    with tempfile.NamedTemporaryFile(
        prefix="image-build-", suffix=".log", dir=directory, delete=False
    ) as stream:
        try:
            yield stream
        except Exception as error:
            raise RuntimeError(
                f"{error}; private image build log: {stream.name}"
            ) from error
        else:
            Path(stream.name).unlink()


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def validate(receipt: dict, manifest: dict, identity: str) -> None:
    if (
        receipt.get("schema") != SCHEMA
        or receipt.get("manifest_sha256") != identity
        or receipt.get("source_commit") != manifest["source_commit"]
        or receipt.get("version") != manifest["version"]
    ):
        raise ValueError(
            "image build receipt differs from the installed source release"
        )
    images = receipt.get("images", [])
    if len(images) != 2 or {item.get("role") for item in images} != {"work", "egress"}:
        raise ValueError("image build must bind exactly work and egress images")
    for image in images:
        if image.get("platform") != "linux/amd64" or any(
            not isinstance(image.get(key), str)
            or not re.fullmatch(r"sha256:[a-f0-9]{64}", image[key])
            for key in ("id", *(["manifest_id"] if "manifest_id" in image else []))
        ):
            raise ValueError(
                "built image must have immutable platform-bound identities"
            )
    base = receipt.get("work_base_image", {})
    if (
        base.get("platform") != "linux/amd64"
        or not re.fullmatch(r"sha256:[a-f0-9]{64}", base.get("image_id", ""))
        or receipt.get("inventory", {}).get("work_base_image") != base
    ):
        raise ValueError("build receipt base differs from the tool inventory")


def load(environment: Path, manifest: dict, identity: str) -> dict:
    path = environment / "image-build.json"
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 1024 * 1024:
        raise ValueError("image build receipt is missing or invalid")
    receipt = json.loads(path.read_text())
    validate(receipt, manifest, identity)
    return receipt


def matches_override(receipt: dict) -> bool:
    """An explicit deployment input must not silently reuse another business base."""
    reference = os.environ.get("ANOLISA_SKILL_SCORE_BASE_IMAGE")
    recipe = os.environ.get("ANOLISA_SKILL_SCORE_BASE_RECIPE")
    base = receipt["work_base_image"]
    return (not reference or reference == base["reference"]) and (
        not recipe or json.loads(Path(recipe).read_text()) == base.get("recipe")
    )


def build(
    payload: Path,
    candidate: Path,
    manifest: dict,
    identity: str,
    check_cancelled: Callable[[], None],
    log_directory: Path,
) -> dict:
    if os.environ.get("ANOLISA_SKILL_SCORE_OFFLINE") == "1":
        raise ValueError(
            "fresh image construction needs network access; build the template online first"
        )
    network = os.environ.get("ANOLISA_SKILL_SCORE_BUILD_NETWORK", "default")
    if network not in {"default", "host"}:
        raise ValueError("image build network must be default or host")
    with (
        tempfile.TemporaryDirectory(
            prefix=".image-build-", dir=candidate.parent
        ) as directory,
        build_log(log_directory) as log,
    ):
        output = Path(directory)
        arguments = [
            sys.executable,
            str(payload / manifest["image_build"]["entry"]),
            str(output),
            "--version",
            manifest["version"] + ".build." + uuid.uuid4().hex[:12],
            "--local-build",
            "--build-network",
            network,
            "--base-image",
            os.environ.get(
                "ANOLISA_SKILL_SCORE_BASE_IMAGE",
                manifest["image_build"]["default_base"],
            ),
        ]
        recipe = os.environ.get("ANOLISA_SKILL_SCORE_BASE_RECIPE")
        if recipe:
            arguments += ["--base-image-recipe", recipe]
        # A separate process group bounds Docker CLI descendants as well as the
        # builder script; no global Docker cleanup or shared-image pruning.
        process = subprocess.Popen(
            arguments,
            start_new_session=True,
            stdout=log,
            stderr=subprocess.STDOUT,
        )
        try:
            deadline = time.monotonic() + 1500
            while process.poll() is None:
                check_cancelled()
                if time.monotonic() >= deadline:
                    raise TimeoutError("image build exceeded 1500 seconds")
                time.sleep(0.1)
            check_cancelled()
            if process.returncode:
                raise RuntimeError(
                    "image build failed; verify Docker BuildKit, base image and apt/npm/pip network access"
                )
        finally:
            if process.poll() is None:
                os.killpg(process.pid, signal.SIGTERM)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
        metadata = json.loads((output / "images.json").read_text())
        images = [
            dict(item, role=role)
            for role, item in zip(
                ("work", "egress"), metadata["image_details"], strict=True
            )
        ]
        receipt = {
            "schema": SCHEMA,
            "manifest_sha256": identity,
            "source_commit": manifest["source_commit"],
            "version": manifest["version"],
            "images": images,
            "work_base_image": metadata["work_base_image"],
            "inventory": json.loads((output / "tool-inventory.json").read_text()),
        }
        validate(receipt, manifest, identity)
        (candidate / "image-build.json").write_text(
            json.dumps(receipt, indent=2) + "\n"
        )
        return receipt
