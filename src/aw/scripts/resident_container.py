"""Build an allowlisted image and run credential-free native resident acceptance."""

import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import uuid

from check import AW, REPO, git, sha
from check_native import load
from native_acceptance import Commands, verify_bundle
from resident_contract import BINARIES, shards, suites, verify, verify_shard

SOURCES = (
    "scripts/check.py",
    "scripts/check_native.py",
    "scripts/native_acceptance.py",
    "scripts/native_cleanup.py",
    "scripts/native_qoder.py",
    "scripts/native_reports.py",
    "scripts/resident_contract.py",
    "tests/native-matrix.json",
    "tests/sdk_resident.py",
    "tests/resident_image.py",
    "crates/aw-cli/tests/fixtures/claude-sdk/native_worker.py",
)


def source_files(root: Path) -> list[Path]:
    """Exclude checkout metadata, deployment configuration and generated artifacts."""
    paths = [root / name for name in SOURCES]
    paths.extend(sorted((root / "tests/fixtures/resident").glob("*.py")))
    paths.extend(sorted((root / "tests/fixtures/resident").glob("*.mjs")))
    if any(path.is_symlink() or not path.is_file() for path in paths):
        raise ValueError("resident image requires regular allowlisted source files")
    return paths


def stage(
    root: Path, binaries: Path, context: Path, expected: str, manifest: dict
) -> dict:
    """Give Docker only reviewed public fixtures and the checksummed candidate payload."""
    verify_bundle(binaries, expected)
    selected = suites(manifest)
    record = {}
    for source in source_files(root):
        relative = source.relative_to(root)
        target = context / "source" / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        record[str(relative)] = hashlib.sha256(source.read_bytes()).hexdigest()
    (context / "source/source-sha.txt").write_text(expected + "\n")
    payload = context / "payload"
    payload.mkdir()
    checksums = []
    for line in (binaries / "SHA256SUMS").read_text().splitlines():
        _, name = line.split("  ", 1)
        if name not in BINARIES:
            continue
        shutil.copyfile(binaries / name, payload / name)
        (payload / name).chmod(0o755)
        checksums.append(line)
    (payload / "SHA256SUMS").write_text("\n".join(checksums) + "\n")
    shutil.copyfile(binaries / "source-sha.txt", payload / "source-sha.txt")
    (context / "python-version.txt").write_text(selected["claude-sdk"]["python"] + "\n")
    (context / "requirements.txt").write_text(
        "\n".join(selected["claude-sdk"]["requirements"]) + "\n"
    )
    (context / "pi").mkdir()
    for name in ("package.json", "package-lock.json"):
        shutil.copyfile(
            root / selected["pi"]["npm_fixture"] / name, context / "pi" / name
        )
    return record


class Docker:
    """Own exact container/image names and leave cleanup evidence on every path."""

    def __init__(self, evidence: Path):
        self.environment = {
            key: value for key, value in os.environ.items()
            if key in {"PATH", "HOME", "DOCKER_HOST", "DOCKER_CONTEXT", "DOCKER_CONFIG"}
        }
        self.commands = Commands(evidence, self.environment)
        self.image_settled = True

    def query(self, *argv: str) -> str:
        return subprocess.check_output(
            ["docker", *argv], env=self.environment, text=True,
            stderr=subprocess.PIPE, timeout=30,
        ).strip()

    def run(self, *argv: str, timeout: int) -> None:
        if argv[:2] not in {("image", "load"), ("buildx", "build")}:
            self.commands.run(["docker", *argv], REPO, timeout)
            return
        # The daemon can publish an image after a disconnected client exits.
        # Keep the bounded client alive before checking/removing its final tag.
        pending = []

        def defer(signum: int, _frame: object) -> None:
            if not pending:
                pending.append(signum)

        previous = {sig: signal.signal(sig, defer) for sig in (signal.SIGINT, signal.SIGTERM)}
        try:
            self.commands.run(["docker", *argv], REPO, timeout)
        except subprocess.TimeoutExpired:
            self.image_settled = False
            raise
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)
        if pending:
            raise InterruptedError(f"received signal {pending[0]} during image mutation")

    def remove_image(self, tag: str) -> bool:
        if self.query("image", "ls", "-q", tag):
            self.query("image", "rm", tag)
        return not self.query("image", "ls", "-q", tag) and self.image_settled


def build(binaries: Path, output: Path, expected: str, manifest: dict) -> None:
    """Export one candidate image for all shards, then remove its local tag."""
    output.mkdir(parents=True, exist_ok=False)
    docker = Docker(output)
    tag = "aw-resident:" + uuid.uuid4().hex
    record = {"candidate_sha": expected, "tag": tag, "image_removed": False,
              "remove_image": f"docker image rm {tag}"}
    try:
        with tempfile.TemporaryDirectory(prefix="resident-image-", dir=AW / "target") as temporary:
            context = Path(temporary)
            inputs = stage(AW, binaries, context, expected, manifest)
            shutil.copyfile(AW / "tests/container/Dockerfile", context / "Dockerfile")
            (output / "context.json").write_text(json.dumps(inputs, indent=2) + "\n")
            docker.run("buildx", "build", "--load", "--tag", tag, "--label",
                       "org.opencontainers.image.revision=" + expected,
                       str(context), timeout=1200)
        record["image"] = json.loads(docker.query("image", "inspect", tag))[0]["Id"]
        docker.run("image", "save", "--output", str(output / "image.tar"), tag, timeout=300)
        with (output / "image.tar").open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        record["archive_sha256"] = digest
    finally:
        try:
            record["image_removed"] = docker.remove_image(tag)
        finally:
            (output / "image.json").write_text(json.dumps(record, indent=2) + "\n")
    if not record["image_removed"]:
        raise ValueError("resident build image cleanup failed")


def run_shard(bundle: Path, output: Path, expected: str, manifest: dict, shard: str) -> None:
    """Load the same image archive and bound one shard including owned cleanup."""
    plan = shards(manifest)[shard]
    source = json.loads((bundle / "image.json").read_text())
    if source.get("candidate_sha") != expected or source.get("image_removed") is not True:
        raise ValueError("resident image bundle identity or build cleanup is invalid")
    with (bundle / "image.tar").open("rb") as stream:
        digest = hashlib.file_digest(stream, "sha256").hexdigest()
    if digest != source["archive_sha256"]:
        raise ValueError("resident image archive checksum mismatch")
    tag = source["tag"]
    if not re.fullmatch(r"aw-resident:[0-9a-f]{32}", tag):
        raise ValueError("resident image has an invalid ownership tag")
    output.mkdir(parents=True, exist_ok=False)
    docker = Docker(output)
    if docker.query("image", "ls", "-q", tag):
        raise ValueError("resident image tag already exists; refusing to take ownership")
    name = "aw-resident-" + uuid.uuid4().hex
    record = {"candidate_sha": expected, "shard": shard, "name": name, "tag": tag,
              "ports": [], "removed": False, "image_removed": False,
              "stop": f"docker stop --time 15 {name}", "remove": f"docker rm -f {name}",
              "remove_image": f"docker image rm {tag}"}
    try:
        docker.run("image", "load", "--input", str(bundle / "image.tar"), timeout=300)
        details = json.loads(docker.query("image", "inspect", tag))[0]
        if (details["Id"] != source["image"] or
                details["Config"]["Labels"]["org.opencontainers.image.revision"] != expected):
            raise ValueError("loaded resident image identity differs from the candidate")
        record["id"] = docker.query(
            "create", "--name", name, "--init", "--network", "none",
            "--security-opt", "no-new-privileges", "--cap-drop", "ALL",
            "--pids-limit", "512", "--memory", "4g", tag, "--shard", shard,
        )
        container = json.loads(docker.query("inspect", name))[0]
        record.update(image=container["Image"], network=container["HostConfig"]["NetworkMode"],
                      mounts=container["Mounts"])
        (output / "container.json").write_text(json.dumps(record, indent=2) + "\n")
        try:
            docker.run("start", "--attach", name, timeout=plan["timeout_seconds"])
        finally:
            docker.query("cp", name + ":/evidence/.", str(output))
    finally:
        try:
            if docker.query("ps", "-aq", "--filter", "name=^/" + name + "$"):
                docker.query("rm", "-f", name)
            record["removed"] = not docker.query("ps", "-aq", "--filter", "name=^/" + name + "$")
        finally:
            try:
                record["image_removed"] = docker.remove_image(tag)
            finally:
                (output / "container.json").write_text(json.dumps(record, indent=2) + "\n")
    verify_shard(output, manifest, expected, shard)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate-sha", required=True)
    parser.add_argument("--bin", type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--build-only", action="store_true")
    parser.add_argument("--image-bundle", type=Path)
    parser.add_argument("--shard")
    args = parser.parse_args()
    if args.image_bundle:
        if not args.shard or args.bin or args.build_only:
            parser.error("--image-bundle requires --shard and excludes --bin/--build-only")
    elif args.bin is None or args.shard:
        parser.error("building requires --bin and excludes --shard")
    expected = sha(args.candidate_sha)
    if git("rev-parse", "HEAD") != expected:
        raise ValueError("resident image checkout differs from candidate")
    paths = [str(path.relative_to(REPO)) for path in source_files(AW)]
    paths += ["src/aw/tests/container/Dockerfile", "src/aw/crates/aw-cli/tests/fixtures/pi"]
    if git("status", "--porcelain", "--untracked-files=all", "--", *paths):
        raise ValueError("resident image inputs contain uncommitted changes")
    manifest = load(AW)
    output = args.output.resolve()
    if args.image_bundle:
        run_shard(args.image_bundle.resolve(), output, expected, manifest, args.shard)
    elif args.build_only:
        build(args.bin.resolve(), output, expected, manifest)
    else:
        output.mkdir(parents=True, exist_ok=False)
        build(args.bin.resolve(), output / "image", expected, manifest)
        for shard in shards(manifest):
            run_shard(output / "image", output / shard, expected, manifest, shard)
        verify(output, manifest, expected)
    print("Resident acceptance passed with complete evidence and cleanup")


if __name__ == "__main__":

    def interrupted(signum: int, _frame: object) -> None:
        raise InterruptedError(f"received signal {signum}")

    signal.signal(signal.SIGTERM, interrupted)
    os.umask(0o077)
    main()
