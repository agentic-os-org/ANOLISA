#!/usr/bin/env python3
"""Capture pinned Gradle workloads without modifying their build definitions."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import selectors
import subprocess
import sys
import time
from pathlib import Path
from urllib.parse import urlparse


PROJECTS = {
    "mockito": ("mockito/mockito", "v5.24.0", "21", ["build"], ["clean", "build"]),
    "caffeine": ("ben-manes/caffeine", "v3.3.1", "21", [":simulator:build"], ["clean", ":simulator:build"]),
    "resilience4j": ("resilience4j/resilience4j", "v2.4.0", "21", ["build"], ["clean", "build"]),
}

RUN_TIMEOUT_SECONDS = 2700


def checked(args: list[str], workdir: Path | None = None) -> str:
    """Run preparation separately so its output never enters the corpus."""
    return subprocess.check_output(args, cwd=workdir, stderr=subprocess.STDOUT).decode()


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def resolve_proxy() -> tuple[str | None, int | None]:
    raw = (
        os.environ.get("HTTPS_PROXY")
        or os.environ.get("https_proxy")
        or os.environ.get("HTTP_PROXY")
        or os.environ.get("http_proxy")
    )
    if not raw:
        return None, None
    parsed = urlparse(raw if "://" in raw else f"http://{raw}")
    if not parsed.hostname or parsed.port is None:
        return None, None
    return parsed.hostname, parsed.port


def capture(
    command: list[str],
    recorded_command: list[str],
    directory: Path,
    container_name: str,
    timeout_seconds: int = RUN_TIMEOUT_SECONDS,
) -> dict[str, object]:
    """Preserve raw bytes and record parent receipt order, not JVM chronology."""
    directory.mkdir()
    started = time.time()
    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as proc:
        with selectors.DefaultSelector() as selector:
            selector.register(proc.stdout, selectors.EVENT_READ, "stdout")
            selector.register(proc.stderr, selectors.EVENT_READ, "stderr")
            streams = {name: bytearray() for name in ("stdout", "stderr", "observed")}
            chunks = []
            while selector.get_map():
                if time.time() - started > timeout_seconds:
                    subprocess.run(
                        ["docker", "rm", "-f", container_name],
                        stdout=subprocess.DEVNULL,
                        stderr=subprocess.DEVNULL,
                        check=False,
                    )
                    proc.kill()
                    proc.wait()
                    raise TimeoutError(f"{directory.name} exceeded {timeout_seconds}s")
                for key, _ in selector.select(timeout=1.0):
                    chunk = key.fileobj.read1(65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    chunks.append({"stream": key.data, "bytes": len(chunk)})
                    streams[key.data].extend(chunk)
                    streams["observed"].extend(chunk)
            status = proc.wait()
    duration = round(time.time() - started, 3)
    record = {
        "command": recorded_command,
        "exit_code": status,
        "duration_seconds": duration,
        "sha256": {},
    }
    for name, data in streams.items():
        raw = bytes(data)
        (directory / f"{name}.txt").write_bytes(raw)
        record["sha256"][name] = hashlib.sha256(raw).hexdigest()
        record[f"{name}_bytes"] = len(raw)
        record[f"{name}_cr"] = raw.count(b"\r")
        record[f"{name}_lf"] = raw.count(b"\n")
        record[f"{name}_nul"] = raw.count(b"\x00")
    record["observed_chunks"] = chunks
    (directory / "capture.json").write_text(json.dumps(record, indent=2) + "\n")
    print(
        f"{directory.name}: exit={status}, bytes={record['observed_bytes']}, "
        f"cr={record['observed_cr']}, {duration}s",
        flush=True,
    )
    return record


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--project", choices=PROJECTS, required=True)
    parser.add_argument(
        "--failure-probe",
        action="store_true",
        help="also capture an unknown-task invocation, which exits non-zero",
    )
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    work = output / "work"
    source = work / "source"
    gradle_home = work / "gradle-home"
    for directory in (work, gradle_home, work / "home"):
        directory.mkdir()
    repo, tag, jdk, build_args, clean_build_args = PROJECTS[args.project]
    image = f"eclipse-temurin:{jdk}-jdk"
    image_id = checked(["docker", "image", "inspect", image, "--format", "{{.Id}}"]).strip()

    proxy_host, proxy_port = resolve_proxy()
    use_host_network = proxy_host in ("127.0.0.1", "localhost", "::1")
    network_mode = "host" if use_host_network else "bridge"
    proxy_host_port = f"{proxy_host}:{proxy_port}" if proxy_host and proxy_port is not None else None
    if proxy_host and proxy_port is not None:
        (gradle_home / "gradle.properties").write_text(
            f"systemProp.http.proxyHost={proxy_host}\n"
            f"systemProp.http.proxyPort={proxy_port}\n"
            f"systemProp.https.proxyHost={proxy_host}\n"
            f"systemProp.https.proxyPort={proxy_port}\n"
        )

    url = f"https://github.com/{repo}.git"
    checked(["git", "init", "--quiet", str(source)])
    checked(["git", "-C", str(source), "remote", "add", "origin", url])
    checked(["git", "-C", str(source), "fetch", "--quiet", "--depth", "1", "origin", "tag", tag])
    checked(["git", "-C", str(source), "checkout", "--quiet", "FETCH_HEAD"])
    commit = checked(["git", "-C", str(source), "rev-parse", "HEAD"]).strip()
    status = checked(["git", "-C", str(source), "status", "--porcelain"]).strip()
    if status:
        raise SystemExit(f"dirty checkout after pinning {tag}: {status}")
    if (source / ".gitmodules").exists():
        raise SystemExit(f"{repo}@{tag} uses submodules; capture would not be self-contained")
    gradlew = source / "gradlew"
    if not gradlew.exists():
        raise SystemExit(f"{repo}@{tag} has no Gradle wrapper")
    gradlew.chmod(0o755)
    wrapper = source / "gradle/wrapper/gradle-wrapper.properties"
    distribution = [
        line.split("=", 1)[1].replace("\\", "")
        for line in wrapper.read_text().splitlines()
        if line.startswith("distributionUrl=")
    ]
    properties = source / "gradle.properties"
    (output / "environment.json").write_text(json.dumps({
        "repository": repo,
        "tag": tag,
        "commit": commit,
        "clone": "git init + fetch --depth 1 tag (shallow)",
        "image": image,
        "image_id": image_id,
        "distribution": distribution,
        "gradle_properties": properties.read_text() if properties.exists() else None,
        "docker_version": checked(["docker", "version", "--format", "{{.Server.Version}}"]).strip(),
        "java_version": checked(
            ["docker", "run", "--rm", image_id, "java", "-version"], workdir=None
        ).strip(),
        "git_version": checked(["git", "--version"]).strip(),
        "python_version": checked(["python3", "--version"], workdir=None).strip(),
        "cpus": "2",
        "cpu_note": "Gradle workers follow the container CPU quota (JDK 21 container awareness)",
        "proxy_configured": proxy_host_port is not None,
    }, indent=2) + "\n")

    if proxy_host_port is not None:
        print(
            f"note: using HTTP(S) proxy for {args.project}; host proxy and paths are redacted in JSON outputs",
            file=sys.stderr,
            flush=True,
        )

    canonical_envs = [
        "--env", "GRADLE_USER_HOME=/work/gradle-home",
        "--env", "HOME=/work/home",
        "--env", "TZ=UTC",
        "--env", "LANG=C.UTF-8",
    ]
    base_docker = ["docker", "run", "--rm", "--cpus", "2"]
    if use_host_network:
        base_docker += ["--network", "host"]
    base_envs = list(canonical_envs)
    if proxy_host and proxy_port is not None:
        gradle_opts = (
            f"-Dhttp.proxyHost={proxy_host} -Dhttp.proxyPort={proxy_port} "
            f"-Dhttps.proxyHost={proxy_host} -Dhttps.proxyPort={proxy_port}"
        )
        base_envs += ["--env", f"GRADLE_OPTS={gradle_opts}"]
    base_mount = [
        "--mount", f"type=bind,src={work},dst=/work",
        "--workdir", "/work/source",
        image_id, "./gradlew",
    ]
    sanitized_prefix = [
        "docker", "run", "--rm", "--cpus", "2",
        *canonical_envs,
        "--mount", "type=bind,src=/work-host/work,dst=/work",
        "--workdir", "/work/source",
        image_id, "./gradlew",
    ]

    runs = [
        ("cold", build_args),
        ("incremental", build_args),
        ("clean_rebuild", clean_build_args),
    ]
    if args.failure_probe:
        # A misspelled task name is a realistic agent invocation and a real failure shape.
        runs.append(("failure_unknown_task", ["buid"]))
    for name, options in runs:
        container_name = f"tokenless-gradle-{args.project}-{name}-{os.getpid()}"
        command = base_docker + ["--name", container_name] + base_envs + base_mount + options
        recorded_command = sanitized_prefix + options
        record = capture(command, recorded_command, output / name, container_name=container_name)
        (output / name / "raw_bytes.json").write_text(json.dumps({
            "command": record["command"], "exit_code": record["exit_code"],
        }, indent=2) + "\n")
    checked(["docker", "run", "--rm", "--mount", f"type=bind,src={work},dst=/work",
             image_id, "chown", "-R", f"{os.getuid()}:{os.getgid()}", "/work"])
    gradle_home_note = (
        "work/gradle-home (empty except proxy gradle.properties before cold run)"
        if proxy_host_port is not None
        else "work/gradle-home (empty before cold run)"
    )
    (output / "provenance.json").write_text(json.dumps({
        "project": args.project, "repository": repo, "tag": tag, "commit": commit,
        "image_id": image_id, "gradle_home": gradle_home_note,
        "source_state": "unmodified pinned checkout; no build definition edits",
        "runs": {name: {
            "options": options,
            "exit_code": json.loads((output / name / "capture.json").read_text())["exit_code"],
            "observed_sha256": json.loads((output / name / "capture.json").read_text())["sha256"]["observed"],
        } for name, options in runs},
    }, indent=2) + "\n")
    print(f"{args.project}: captured {len(runs)} runs into {output}", flush=True)


if __name__ == "__main__":
    main()
