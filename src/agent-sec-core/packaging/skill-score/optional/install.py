#!/usr/bin/env python3
"""Prepare a content-addressed worker from a verified optional raw candidate."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path, PurePosixPath
from typing import Any

# Helpers also support direct --check invocation without leaving unowned bytecode.
sys.dont_write_bytecode = True
from lifecycle import (  # noqa: E402
    exclusive,
    pending_cleanup,
    record_environment,
    request_directory,
)

MAX_ARCHIVE = 2 * 1024**3
INTERRUPTED = False


def interrupted(_signal: int, _frame: object) -> None:
    # The entire preparation group receives TERM. Let an in-flight worker finish
    # its owned cleanup before the installer removes the candidate directory.
    global INTERRUPTED
    INTERRUPTED = True


def check_cancelled() -> None:
    if INTERRUPTED:
        raise InterruptedError("optional worker preparation cancelled")


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def safe_relative(value: str) -> Path:
    path = PurePosixPath(value)
    if not value or path.is_absolute() or ".." in path.parts or "\\" in value:
        raise ValueError("artifact contains an unsafe relative path")
    return Path(*path.parts)


def check_file(path: Path, expected: dict[str, Any]) -> bool:
    return (
        path.is_file()
        and not path.is_symlink()
        and path.stat().st_size <= MAX_ARCHIVE
        and ("bytes" not in expected or path.stat().st_size == expected["bytes"])
        and digest(path) == expected["sha256"]
    )


def fetch(
    spec: dict[str, Any], cache: Path, source: Path | None, offline: bool
) -> Path:
    checksum = spec["sha256"]
    if not re.fullmatch(r"[a-f0-9]{64}", checksum):
        raise ValueError("dependency must have a pinned SHA256")
    target = cache / checksum
    if check_file(target, spec):
        return target
    if target.exists() or target.is_symlink():
        target.unlink()
    local = source / safe_relative(spec["file"]) if source else None
    if local is not None and local.exists():
        if not check_file(local, spec):
            raise ValueError("local dependency checksum mismatch")
        shutil.copyfile(local, target)
        return target
    if offline:
        raise ValueError(
            "offline dependency is absent from the verified cache/source directory"
        )
    url = spec["url"]
    parts = urllib.parse.urlsplit(url)
    if parts.scheme != "https" or parts.username or parts.password:
        raise ValueError("dependency URL must use HTTPS without credentials")
    partial = cache / (checksum + ".partial")
    request = urllib.request.Request(url, headers={"User-Agent": "anolisa/0.3.20"})
    try:
        with (
            urllib.request.urlopen(request, timeout=60) as response,
            partial.open("wb") as output,
        ):
            if urllib.parse.urlsplit(response.url).scheme != "https":
                raise ValueError("dependency redirected outside HTTPS")
            count = 0
            while block := response.read(1024 * 1024):
                check_cancelled()
                count += len(block)
                if count > min(MAX_ARCHIVE, spec.get("bytes", MAX_ARCHIVE)):
                    raise ValueError("dependency exceeds its declared size limit")
                output.write(block)
        if not check_file(partial, spec):
            raise ValueError("downloaded dependency checksum/size mismatch")
        partial.replace(target)
        return target
    except urllib.error.HTTPError as error:
        raise ValueError(
            f"dependency download failed for {safe_relative(spec['file']).name} "
            f"from {parts.hostname}: HTTP {error.code}"
        ) from error
    finally:
        partial.unlink(missing_ok=True)


def unpack(archive: Path, destination: Path) -> None:
    """Extract trusted upstream archives without accepting escaping links/devices."""
    with tarfile.open(archive) as stream:
        members = stream.getmembers()
        total = 0
        names = set()
        for member in members:
            relative = safe_relative(member.name)
            if relative in names:
                raise ValueError("dependency archive contains duplicate members")
            names.add(relative)
            if not (member.isfile() or member.isdir() or member.issym()):
                raise ValueError("dependency archive contains a special file")
            total += member.size
            if total > MAX_ARCHIVE:
                raise ValueError("expanded dependency exceeds its size limit")
            if member.issym():
                target = member.linkname
                if target.startswith("/") or "\\" in target:
                    raise ValueError("dependency symlink escapes its archive")
                depth = len(relative.parent.parts)
                for part in PurePosixPath(target).parts:
                    depth += -1 if part == ".." else 0 if part == "." else 1
                    if depth < 0:
                        raise ValueError("dependency symlink escapes its archive")
        # No member may write through another member's symbolic link.
        links = {safe_relative(m.name) for m in members if m.issym()}
        if any(any(parent in links for parent in path.parents) for path in names):
            raise ValueError("dependency archive writes through a symbolic link")
        for member in members:
            member.mode &= 0o777
            stream.extract(member, destination, set_attrs=False)
            if member.isfile():
                (destination / safe_relative(member.name)).chmod(member.mode)


def run(
    arguments: list[str],
    *,
    timeout: int = 120,
    env: dict[str, str] | None = None,
) -> str:
    check_cancelled()
    result = subprocess.run(
        arguments,
        check=False,
        capture_output=True,
        text=True,
        timeout=timeout,
        env=env,
    )
    check_cancelled()
    if result.returncode:
        # Transport/package output can include authenticated mirror URLs. Error
        # details remain local to the tool; do not echo them into install logs.
        raise RuntimeError(
            f"preparation command {Path(arguments[0]).name} failed ({result.returncode})"
        )
    return result.stdout


def require_local_docker() -> None:
    # Preparation and execution must address the same system daemon. Changing
    # this process's environment does not modify the user's saved Docker context.
    os.environ["DOCKER_HOST"] = "unix:///var/run/docker.sock"
    os.environ.pop("DOCKER_CONTEXT", None)
    if run(["docker", "info", "--format", "{{.OSType}}"]).strip() != "linux":
        raise ValueError("optional worker requires a Linux Docker daemon")
    if "rootless" in run(["docker", "info", "--format", "{{json .SecurityOptions}}"]):
        raise ValueError("rootless Docker is not a supported optional worker host")
    run(["docker", "compose", "version"])


def network_check(image_ids: dict[str, str]) -> None:
    """Check the exact compatibility rules in an isolated, request-owned namespace."""
    name = "asc-score-install-" + uuid.uuid4().hex
    image = image_ids["egress"]
    try:
        run(
            [
                "docker",
                "run",
                "--rm",
                "--pull",
                "never",
                "--network",
                "none",
                "--cap-add",
                "NET_ADMIN",
                "--cap-add",
                "NET_RAW",
                "--name",
                name,
                "--label",
                "io.anolisa.skill-score.install=" + name,
                "--entrypoint",
                "/usr/local/bin/asc-egress-install",
                image,
            ],
            timeout=45,
        )
    finally:
        # Cancellation may stop the Docker client before --rm finishes. Verify
        # ownership before removing only this readiness probe, never prune.
        probe = subprocess.run(
            [
                "docker",
                "container",
                "ls",
                "-aq",
                "--filter",
                "name=^/" + name + "$",
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=15,
        )
        if probe.returncode:
            raise RuntimeError("cannot verify readiness network-probe cleanup")
        if probe.stdout.strip():
            inspected = subprocess.run(
                ["docker", "container", "inspect", name],
                check=False,
                capture_output=True,
                text=True,
                timeout=15,
            )
            values = json.loads(inspected.stdout) if inspected.returncode == 0 else []
            if (
                len(values) != 1
                or (values[0].get("Config", {}).get("Labels") or {}).get(
                    "io.anolisa.skill-score.install"
                )
                != name
            ):
                raise RuntimeError("readiness network-probe ownership is unknown")
            removed = subprocess.run(
                ["docker", "container", "rm", "--force", name],
                check=False,
                capture_output=True,
                timeout=15,
            )
            if removed.returncode:
                raise RuntimeError("readiness network-probe cleanup failed")


def images_ready(manifest: dict[str, Any]) -> dict[str, str] | None:
    """Resolve only archive-bound IDs, never mutable image names."""
    resolved = {}
    for image in manifest["images"]:
        for identity in (image["id"], image["manifest_id"]):
            try:
                values = json.loads(
                    run(["docker", "image", "inspect", identity], timeout=15)
                )
            except (RuntimeError, ValueError):
                continue
            if (
                len(values) == 1
                and values[0].get("Id") == identity
                and (values[0].get("Os"), values[0].get("Architecture"))
                == ("linux", "amd64")
            ):
                resolved[image["role"]] = identity
                break
        else:
            return None
    return resolved


def validate_manifest(manifest: dict[str, Any], payload: Path, prefix: Path) -> None:
    if (
        manifest.get("schema") != "agent-sec-skill-score/runtime-v1"
        or manifest.get("component") != "sec-core-skill-score"
        or manifest.get("protocol_version") != "agent-sec-skill-score/worker-v1"
        or manifest.get("platform") != "linux/amd64"
        or manifest.get("harbor_version") != "0.22.0"
    ):
        raise ValueError("unsupported optional runtime manifest")
    contract = prefix / "share/anolisa/components/sec-core/component.toml"
    text = contract.read_text()
    component = re.search(r"(?ms)^\[component\]\s*$(.*?)(?=^\[|\Z)", text)
    version = (
        re.search(r'^version\s*=\s*"([^"]+)"\s*$', component[1], re.M)
        if component
        else None
    )
    if version is None or version[1] != manifest["version"]:
        raise ValueError(
            "install the matching sec-core version before sec-core-skill-score"
        )
    images = manifest["images"]
    if len(images) != 2 or {item["role"] for item in images} != {
        "work",
        "egress",
    }:
        raise ValueError("runtime must bind exactly work and egress images")
    for image in images:
        if (
            any(
                not re.fullmatch(r"sha256:[a-f0-9]{64}", image.get(key, ""))
                for key in ("id", "manifest_id")
            )
            or image["platform"] != "linux/amd64"
        ):
            raise ValueError("runtime image must have an immutable platform-bound ID")
    for item in manifest["files"]:
        if not check_file(payload / safe_relative(item["file"]), item):
            raise ValueError("first-party candidate file checksum mismatch")


def python_check(environment: Path, manifest: dict[str, Any]) -> None:
    statement = (
        "import sys,json,platform,importlib.metadata;"
        "from pathlib import Path;root=Path(sys.argv[1]);"
        "sys.path[:0]=[str(root/p) for p in ('worker','shared','site-packages')];"
        "from agent_sec_skill_score_runtime.worker import main,SCHEMA;"
        "print(json.dumps([platform.python_version(),importlib.metadata.version('harbor'),SCHEMA]))"
    )
    values = json.loads(
        run(
            [
                str(environment / "python/bin/python3.12"),
                "-I",
                "-c",
                statement,
                str(environment),
            ]
        )
    )
    if values != [
        manifest["python"]["version"],
        manifest["harbor_version"],
        manifest["protocol_version"],
    ]:
        raise ValueError(
            "prepared worker dependency/protocol versions differ from the release"
        )


def run_readiness_worker(arguments: list[str], timeout: float = 100) -> None:
    check_cancelled()
    process = subprocess.Popen(
        arguments,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        deadline = time.monotonic() + timeout
        while process.poll() is None:
            check_cancelled()
            if time.monotonic() >= deadline:
                raise subprocess.TimeoutExpired("readiness worker", timeout)
            time.sleep(0.05)
        check_cancelled()
        if process.returncode:
            raise RuntimeError("readiness worker failed")
    finally:
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=3)


def cleanup_readiness_request(environment: Path, request_id: str, project: str) -> bool:
    # This recovery subprocess must still run after cancellation. It only uses
    # the worker's double-label ownership check; it cannot create new resources.
    statement = (
        "import sys,json;from pathlib import Path;root=Path(sys.argv[1]);"
        "sys.path[:0]=[str(root/p) for p in ('worker','shared','site-packages')];"
        "from agent_sec_skill_score_runtime.resources import cleanup;"
        "print(json.dumps(cleanup(sys.argv[2],sys.argv[3])))"
    )
    try:
        response = subprocess.run(
            [
                str(environment / "python/bin/python3.12"),
                "-I",
                "-c",
                statement,
                str(environment),
                request_id,
                project,
            ],
            check=False,
            capture_output=True,
            text=True,
            timeout=30,
        )
        return response.returncode == 0 and json.loads(response.stdout) == [
            "complete",
            [],
        ]
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return False


def environment_check(
    environment: Path,
    manifest: dict[str, Any],
    recovery_root: Path,
    image_ids: dict[str, str],
) -> None:
    request_id = "install-" + uuid.uuid4().hex
    project = "asc-score-" + request_id + "__env"
    recovery_root.mkdir(mode=0o700, parents=True, exist_ok=True)
    recovery = recovery_root / (request_id + ".json")
    # Persist outside the candidate before the worker can create resources. A
    # killed helper or deleted candidate must not erase the recovery coordinates.
    with open(
        recovery, "x", opener=lambda path, flags: os.open(path, flags, 0o600)
    ) as output:
        json.dump(
            {
                "schema": "agent-sec-skill-score/prepare-recovery-v1",
                "request_id": request_id,
                "compose_project": project,
                "manifest_sha256": environment.name,
                "cleanup_status": "pending",
            },
            output,
        )
    try:
        with tempfile.TemporaryDirectory(
            prefix="readiness-", dir=environment
        ) as directory:
            root = Path(directory)
            request = {
                "schema_version": manifest["protocol_version"],
                "request_id": request_id,
                "compose_project": project,
                "operation": "environment",
                "root": str(root),
                "timeout": 60,
                "deadline_unix_ms": int((time.time() + 90) * 1000),
                "config": {},
                "image_ids": image_ids,
            }
            (root / "request.json").write_text(json.dumps(request))
            run_readiness_worker(
                [
                    str(environment / "python/bin/python3.12"),
                    "-I",
                    str(environment / "worker-entry.py"),
                    str(root / "request.json"),
                    str(root / "response.json"),
                ]
            )
            response = json.loads((root / "response.json").read_text())
            if (
                response.get("schema_version") != manifest["protocol_version"]
                or response.get("request_id") != request_id
                or response.get("error")
                or response.get("cleanup_status") != "complete"
                or not response.get("result")
            ):
                raise ValueError("worker readiness protocol did not complete cleanly")
    except Exception:
        if cleanup_readiness_request(environment, request_id, project):
            recovery.unlink()
            raise
        raise RuntimeError(
            f"readiness cleanup could not be confirmed; recovery coordinates retained at {recovery}; "
            "inspect Docker resources matching both recorded request and Compose project labels"
        ) from None
    recovery.unlink()


def ready_matches(
    environment: Path, manifest_digest: str, manifest: dict[str, Any]
) -> bool:
    try:
        ready = json.loads((environment / "ready.json").read_text())
        if (
            ready["manifest_sha256"] != manifest_digest
            or ready["images"] != manifest["images"]
        ):
            return False
        inventory_path = environment / "installed-files.json"
        if digest(inventory_path) != ready["installed_files_sha256"]:
            return False
        for item in json.loads(inventory_path.read_text()):
            if not check_file(environment / safe_relative(item["file"]), item):
                return False
        python_check(environment, manifest)
        return True
    except (OSError, ValueError, KeyError, RuntimeError):
        return False


def build_environment(
    candidate: Path,
    payload: Path,
    manifest: dict[str, Any],
    cache: Path,
    source: Path | None,
    offline: bool,
) -> None:
    unpack(fetch(manifest["python"], cache, source, offline), candidate)
    with tempfile.TemporaryDirectory(prefix="uv-", dir=cache) as directory:
        unpack(fetch(manifest["uv"], cache, source, offline), Path(directory))
        uv = Path(directory) / "uv-x86_64-unknown-linux-gnu/uv"
        if run([str(uv), "--version"]).strip() != "uv " + manifest["uv"]["version"]:
            raise ValueError("prepared uv version differs from the manifest")
        index = os.environ.get("ANOLISA_SKILL_SCORE_PYPI_INDEX", manifest["pypi_index"])
        parts = urllib.parse.urlsplit(index)
        if parts.scheme != "https" or parts.username or parts.password:
            raise ValueError(
                "package mirror must use HTTPS without embedded credentials"
            )
        args = [
            str(uv),
            "--no-config",
            "pip",
            "install",
            "--python",
            str(candidate / "python/bin/python3.12"),
            "--target",
            str(candidate / "site-packages"),
            "--require-hashes",
            "--no-deps",
            "--no-build",
            "-r",
            str(payload / "requirements.lock"),
        ]
        if offline:
            if source is None:
                args += ["--offline"]
            else:
                args += [
                    "--offline",
                    "--no-index",
                    "--find-links",
                    str(source / "wheels"),
                ]
        else:
            args += ["--index-url", index]
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("UV_", "PIP_", "PYTHON"))
        }
        environment["UV_CACHE_DIR"] = str(cache / "uv")
        environment["UV_PYTHON_DOWNLOADS"] = "never"
        run(args, timeout=1200, env=environment)
    for name in ("worker", "shared"):
        shutil.copytree(payload / name, candidate / name)
    shutil.copyfile(payload / "worker-entry.py", candidate / "worker-entry.py")
    python_check(candidate, manifest)


def prepare(payload: Path, prefix: Path, cache_root: Path) -> Path:
    root = prefix / "lib/anolisa/sec-core-skill-score"
    with exclusive(root, create=True):
        pending_cleanup(root)
        request_directory(root)
        return prepare_locked(payload, prefix, cache_root)


def prepare_locked(payload: Path, prefix: Path, cache_root: Path) -> Path:
    if (platform.system(), platform.machine()) != ("Linux", "x86_64"):
        raise ValueError("optional runtime supports Linux x86_64 only")
    manifest_path = payload / "runtime-manifest.json"
    manifest = json.loads(manifest_path.read_text())
    manifest_digest = digest(manifest_path)
    validate_manifest(manifest, payload, prefix)
    require_local_docker()
    cache = cache_root / "sec-core-skill-score"
    cache.mkdir(parents=True, exist_ok=True)
    source_value = os.environ.get("ANOLISA_SKILL_SCORE_SOURCE_DIR")
    source = Path(source_value).resolve() if source_value else None
    offline = os.environ.get("ANOLISA_SKILL_SCORE_OFFLINE") == "1"
    image_ids = images_ready(manifest)
    if image_ids is None:
        archive = fetch(manifest["image_archive"], cache, source, offline)
        run(["docker", "load", "--input", str(archive)], timeout=300)
        image_ids = images_ready(manifest)
        if image_ids is None:
            raise ValueError(
                "loaded image IDs/platforms differ from the verified manifest"
            )
    environments = prefix / "lib/anolisa/sec-core-skill-score/environments"
    environments.mkdir(parents=True, exist_ok=True)
    target = environments / manifest_digest
    if ready_matches(target, manifest_digest, manifest):
        network_check(image_ids)
        environment_check(target, manifest, environments.parent / "recovery", image_ids)
        record_environment(target, manifest_digest)
        return target
    staging = Path(tempfile.mkdtemp(prefix=".candidate-", dir=environments))
    candidate = staging / manifest_digest
    candidate.mkdir()
    shutil.copyfile(manifest_path, candidate / "runtime-manifest.json")
    try:
        build_environment(candidate, payload, manifest, cache, source, offline)
        network_check(image_ids)
        environment_check(
            candidate, manifest, environments.parent / "recovery", image_ids
        )
        files = [
            {
                "file": str(path.relative_to(candidate)),
                "bytes": path.stat().st_size,
                "sha256": digest(path),
            }
            for path in sorted(candidate.rglob("*"))
            if path.is_file()
            and not path.is_symlink()
            and "__pycache__" not in path.parts
        ]
        inventory = candidate / "installed-files.json"
        inventory.write_text(json.dumps(files, separators=(",", ":")) + "\n")
        ready = {
            "schema": "agent-sec-skill-score/ready-v1",
            "manifest_sha256": manifest_digest,
            "version": manifest["version"],
            "source_commit": manifest["source_commit"],
            "protocol_version": manifest["protocol_version"],
            "worker_version": manifest["worker_version"],
            "python_version": manifest["python"]["version"],
            "harbor_version": manifest["harbor_version"],
            "images": manifest["images"],
            "installed_files_sha256": digest(inventory),
        }
        (candidate / "ready.json").write_text(json.dumps(ready, indent=2) + "\n")
        record_environment(candidate, manifest_digest)
        previous = None
        if target.exists():
            previous = environments / (
                manifest_digest + ".retained-" + uuid.uuid4().hex
            )
            target.rename(previous)
        try:
            candidate.rename(target)
        except OSError:
            if previous is not None:
                previous.rename(target)
            raise
        return target
    finally:
        if staging.exists():
            shutil.rmtree(staging)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--prefix", type=Path, required=True)
    parser.add_argument("--cache-dir", type=Path)
    parser.add_argument(
        "--check",
        action="store_true",
        help="Verify only; never download, load images, or repair",
    )
    args = parser.parse_args()
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        payload = Path(__file__).resolve().parent
        prefix = args.prefix.absolute()
        if args.check:
            root = prefix / "lib/anolisa/sec-core-skill-score"
            with exclusive(root):
                pending_cleanup(root)
                manifest_path = payload / "runtime-manifest.json"
                manifest = json.loads(manifest_path.read_text())
                validate_manifest(manifest, payload, prefix)
                require_local_docker()
                identity = digest(manifest_path)
                environment = root / "environments" / identity
                image_ids = images_ready(manifest)
                if image_ids is None or not ready_matches(
                    environment, identity, manifest
                ):
                    raise ValueError(
                        "optional runtime is not ready; explicitly repair sec-core-skill-score"
                    )
                network_check(image_ids)
                environment_check(environment, manifest, root / "recovery", image_ids)
            print(json.dumps({"status": "ready", "actual_image_ids": image_ids}))
            return
        else:
            if args.cache_dir is None:
                parser.error("--cache-dir is required for preparation")
            prepare(payload, prefix, args.cache_dir.resolve())
    except Exception as exc:
        raise SystemExit(
            f"optional worker preparation failed: {type(exc).__name__}: {exc}"
        ) from None
    print("Optional Skill Score runtime verified and prepared.")


if __name__ == "__main__":
    main()
