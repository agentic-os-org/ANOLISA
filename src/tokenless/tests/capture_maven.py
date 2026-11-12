#!/usr/bin/env python3
"""Capture pinned Maven workloads without modifying their build definitions."""

from __future__ import annotations

import argparse
import hashlib
import json
import selectors
import subprocess
from pathlib import Path


PROJECTS = {
    "commons-cli": ("apache/commons-cli", "698b238276c0e22e97e4aec703a0b00201d29666", 21, []),
    "slf4j": ("qos-ch/slf4j", "c233ea1932228a7fc580823289f896e97ba8a74d", 21,
              ["-pl", "slf4j-api,slf4j-simple", "-am"]),
    "gson": ("google/gson", "828a97be0f8d58108b140b77df8dc76b657f4a87", 17,
             ["-pl", "gson,extras", "-am"]),
    "jsoup": ("jhy/jsoup", "7c56eb26c8cc772c5c3d0e052dc076a128173c85", 21, []),
}


def checked(args: list[str]) -> str:
    """Run preparation separately so its output never enters the Maven corpus."""
    return subprocess.check_output(args, stderr=subprocess.STDOUT).decode()


def capture(command: list[str], directory: Path) -> int:
    """Preserve raw bytes and record parent receipt order, not JVM chronology."""
    directory.mkdir()
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as proc:
        with selectors.DefaultSelector() as selector:
            selector.register(proc.stdout, selectors.EVENT_READ, "stdout")
            selector.register(proc.stderr, selectors.EVENT_READ, "stderr")
            streams = {name: bytearray() for name in ("stdout", "stderr", "observed")}
            chunks = []
            while selector.get_map():
                for key, _ in selector.select():
                    chunk = key.fileobj.read1(65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    chunks.append({"stream": key.data, "bytes": len(chunk)})
                    streams[key.data].extend(chunk)
                    streams["observed"].extend(chunk)
            status = proc.wait()
    hashes = {}
    for name, data in streams.items():
        (directory / f"{name}.txt").write_bytes(data)
        hashes[name] = hashlib.sha256(data).hexdigest()
    (directory / "capture.json").write_text(json.dumps({
        "command": command, "exit_code": status, "sha256": hashes,
        "chunks": chunks,
    }, indent=2) + "\n")
    print(f"{directory.name}: exit={status}, bytes={len(streams['observed'])}", flush=True)
    return status


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--project", choices=PROJECTS, required=True)
    parser.add_argument("--batch", action="store_true")
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    work = output / "work"
    work.mkdir()
    repo, commit, jdk, options = PROJECTS[args.project]
    image = f"maven:3.9.9-eclipse-temurin-{jdk}"
    image_id = checked(["docker", "image", "inspect", image, "--format", "{{.Id}}"]).strip()
    # Some older JDK container detectors still see the host's processor count.
    # Jetty's test proxy needs the same explicit CPU budget in the JVM.
    docker = ["docker", "run", "--rm", "--cpus", "2", "--env",
              "JAVA_TOOL_OPTIONS=-XX:ActiveProcessorCount=2", "--mount",
              f"type=bind,src={work},dst=/work", image_id]
    checked(docker + ["git", "clone", "--quiet", f"https://github.com/{repo}.git", "/work/source"])
    checked(docker + ["git", "-C", "/work/source", "checkout", "--quiet", commit])
    version = checked(docker + ["mvn", "--version"])
    (output / "environment.json").write_text(json.dumps({
        "repository": repo, "commit": commit, "image": image,
        "image_id": image_id, "version": version, "batch": args.batch,
    }, indent=2) + "\n")
    command = docker[:-1] + ["--workdir", "/work/source", image_id, "mvn",
                            "-Dmaven.repo.local=/work/cache"]
    command += (["--batch-mode"] if args.batch else []) + options + ["clean", "package"]
    for cache in ("cold", "warm"):
        if capture(command, output / cache):
            raise SystemExit(f"{args.project}/{cache} failed; retained all logs")


if __name__ == "__main__":
    main()
