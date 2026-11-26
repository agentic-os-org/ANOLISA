#!/usr/bin/env python3
"""Capture pinned C/C++ CTest, build, and verbose compiler workloads offline."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import selectors
import subprocess
import time
from pathlib import Path

IMAGE_NAME = "tokenless-cpp-capture:ubuntu24.04"
RUN_TIMEOUT_SECONDS = 1800

PROJECTS: dict[str, tuple[str, str, str, list[str], bool]] = {
    "brotli": (
        "google/brotli",
        "v1.2.1",
        "db24a98aed38a16de78cc05ca15cdd8dc2a0d92d",
        [],
        True,
    ),
    "catch2": (
        "catchorg/Catch2",
        "v3.16.1",
        "08092139210881f01a34e60c5f2fd6afa0ea4024",
        ["-DCATCH_DEVELOPMENT_BUILD=ON", "-DCATCH_ENABLE_WERROR=OFF"],
        True,
    ),
    "fmt": (
        "fmtlib/fmt",
        "12.2.1",
        "208219daff58f2a7438f6931cc2ddc358acf33ef",
        [],
        True,
    ),
    "libgit2": (
        "libgit2/libgit2",
        "v1.9.7",
        "49e408b3208bc3093757a1c2db938d3590f3f412",
        [],
        False,
    ),
}

NATIVE_MAKE_PROJECTS: dict[str, tuple[str, str, str]] = {
    "zstd": (
        "facebook/zstd",
        "v1.5.7",
        "f8745da6ff1ad1e7bab384bd1f9d742439278e99",
    ),
}

ALL_PROJECT_NAMES: tuple[str, ...] = (*PROJECTS.keys(), *NATIVE_MAKE_PROJECTS.keys())

FAILURE_DIAGNOSTIC_MARKERS: dict[str, tuple[str, ...]] = {
    "clang_ninja_verbose_failure": (
        "FAILED:",
        "identifier '_kBrotliContextLookupTable' is reserved",
    ),
    "failure_unknown_target": ("unknown target 'tokenless_nonexistent_target'",),
}


def checked(args: list[str], workdir: Path | None = None) -> str:
    """Run preparation steps outside captured build/test streams."""
    return subprocess.check_output(args, cwd=workdir, stderr=subprocess.STDOUT).decode()


def resolve_source(
    project: str,
    repo: str,
    tag: str,
    expected_commit: str,
    dest: Path,
    sources_dir: Path | None,
) -> tuple[str, str]:
    if sources_dir is not None:
        cached = sources_dir / project
        if not (cached / ".git").exists():
            raise SystemExit(
                f"{project}: missing cached repository at {cached} for {expected_commit}"
            )
        try:
            resolved = checked(
                [
                    "git",
                    "-C",
                    str(cached),
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    f"{expected_commit}^{{commit}}",
                ]
            ).strip()
        except subprocess.CalledProcessError as exc:
            raise SystemExit(
                f"{project}: cached repository at {cached} does not contain pinned commit {expected_commit}"
            ) from exc
        if resolved != expected_commit:
            raise SystemExit(f"{project}: resolved commit {resolved} != expected {expected_commit}")
        try:
            tag_commit = checked(
                [
                    "git",
                    "-C",
                    str(cached),
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    f"refs/tags/{tag}^{{commit}}",
                ]
            ).strip()
        except subprocess.CalledProcessError as exc:
            raise SystemExit(
                f"{project}: cached repository at {cached} missing tag {tag} for {expected_commit}"
            ) from exc
        if tag_commit != expected_commit:
            raise SystemExit(
                f"{project}: cached tag {tag} resolves to {tag_commit} != expected {expected_commit}"
            )
        cached_uri = cached.resolve().as_uri()
        cached_head = checked(["git", "-C", str(cached), "rev-parse", "HEAD"]).strip()
        if cached_head == expected_commit:
            checked(["git", "clone", "--quiet", "--depth", "1", cached_uri, str(dest)])
            clone_method = "git clone --depth 1 file://<cached> (pinned tag+commit, unmodified)"
        else:
            checked(["git", "clone", "--quiet", "--no-checkout", cached_uri, str(dest)])
            checked(["git", "-C", str(dest), "checkout", "--quiet", "--detach", expected_commit])
            clone_method = (
                "git clone --no-checkout file://<cached> + detached checkout "
                "(pinned tag+commit, unmodified)"
            )
    else:
        url = f"https://github.com/{repo}.git"
        checked(["git", "init", "--quiet", str(dest)])
        checked(["git", "-C", str(dest), "remote", "add", "origin", url])
        checked(["git", "-C", str(dest), "fetch", "--quiet", "--depth", "1", "origin", "tag", tag])
        checked(["git", "-C", str(dest), "checkout", "--quiet", "FETCH_HEAD"])
        clone_method = "git init + fetch --depth 1 tag (pinned tag+commit, shallow, unmodified)"

    actual_head = checked(["git", "-C", str(dest), "rev-parse", "HEAD"]).strip()
    if actual_head != expected_commit:
        raise SystemExit(f"{project}: head mismatch {actual_head} != {expected_commit}")
    status = checked(["git", "-C", str(dest), "status", "--porcelain"]).strip()
    if status:
        raise SystemExit(f"dirty checkout after pinning {tag}: {status}")
    if (dest / ".gitmodules").exists():
        raise SystemExit(f"{repo}@{tag} uses submodules; capture would not be self-contained")
    return actual_head, clone_method


def capture(
    command: list[str],
    recorded_command: list[str],
    directory: Path,
    container_name: str,
    timeout_seconds: int = RUN_TIMEOUT_SECONDS,
) -> dict[str, object]:
    """Preserve raw bytes and record parent pipe receipt order."""
    directory.mkdir()
    started = time.time()

    def expire() -> None:
        """Stop the capture and its container, then report the deadline."""
        subprocess.run(
            ["docker", "rm", "-f", container_name],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
        )
        proc.kill()
        proc.wait()
        raise TimeoutError(f"{directory.name} exceeded {timeout_seconds}s")

    with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE) as proc:
        with selectors.DefaultSelector() as selector:
            assert proc.stdout is not None and proc.stderr is not None
            selector.register(proc.stdout, selectors.EVENT_READ, "stdout")
            selector.register(proc.stderr, selectors.EVENT_READ, "stderr")
            streams = {name: bytearray() for name in ("stdout", "stderr", "observed")}
            chunks = []
            while selector.get_map():
                if time.time() - started > timeout_seconds:
                    expire()
                for key, _ in selector.select(timeout=1.0):
                    chunk = key.fileobj.read1(65536)
                    if not chunk:
                        selector.unregister(key.fileobj)
                        continue
                    chunks.append({"stream": key.data, "bytes": len(chunk)})
                    streams[key.data].extend(chunk)
                    streams["observed"].extend(chunk)
            # Both pipes can reach EOF before the child exits. The deadline is a
            # budget for the whole capture, so the final wait must be bounded too.
            remaining = timeout_seconds - (time.time() - started)
            if remaining <= 0:
                expire()
            try:
                status = proc.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                expire()
    duration = round(time.time() - started, 3)
    record: dict[str, object] = {
        "command": recorded_command,
        "exit_code": status,
        "duration_seconds": duration,
        "sha256": {},
    }
    sha_map: dict[str, str] = {}
    for name, data in streams.items():
        raw = bytes(data)
        (directory / f"{name}.txt").write_bytes(raw)
        sha_map[name] = hashlib.sha256(raw).hexdigest()
        record[f"{name}_bytes"] = len(raw)
        record[f"{name}_cr"] = raw.count(b"\r")
        record[f"{name}_lf"] = raw.count(b"\n")
        record[f"{name}_nul"] = raw.count(b"\x00")
    record["sha256"] = sha_map
    record["observed_chunks"] = chunks
    (directory / "capture.json").write_text(json.dumps(record, indent=2) + "\n")
    (directory / "raw_bytes.json").write_text(
        json.dumps({"command": recorded_command, "exit_code": status}, indent=2) + "\n"
    )
    print(
        f"{directory.name}: exit={status}, bytes={record['observed_bytes']}, "
        f"cr={record['observed_cr']}, {duration}s",
        flush=True,
    )
    return record


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--project", choices=ALL_PROJECT_NAMES, required=True)
    parser.add_argument(
        "--suite",
        choices=("ctest", "build", "verbose", "all"),
        default="ctest",
        help="Capture suite: 'ctest' (default), 'build' (ninja/make cold+incremental), 'verbose' (VERBOSE=1/ninja -v/GNU Make V=1), or 'all'",
    )
    parser.add_argument(
        "--sources-dir",
        type=Path,
        default=None,
        help="Directory containing pre-cloned <dir>/<project> repositories with matching tag and pinned commit (fails closed if missing or mismatched)",
    )
    return parser


def verbose_runs_for_project(project: str) -> list[tuple[str, list[list[str]], list[str], bool]]:
    if project in NATIVE_MAKE_PROJECTS:
        return [
            (
                "make_lib_v1_gcc",
                [],
                ["make", "-C", "/work/source/lib", "-j", "2", "V=1", "libzstd.a"],
                True,
            ),
            (
                "make_lib_v1_clang",
                [["make", "-C", "/work/source/lib", "clean"]],
                ["make", "-C", "/work/source/lib", "-j", "2", "CC=clang", "V=1", "libzstd.a"],
                True,
            ),
            (
                "make_all_v1_gcc",
                [["make", "-C", "/work/source", "clean"]],
                ["make", "-C", "/work/source", "-j", "2", "V=1", "lib-release", "zstd-release"],
                True,
            ),
        ]

    cmake_opts = PROJECTS[project][3]
    runs: list[tuple[str, list[list[str]], list[str], bool]] = [
        (
            "ninja_verbose",
            [
                ["cmake", "-E", "rm", "-rf", "/work/build-ninja"],
                [
                    "cmake",
                    "-S",
                    "/work/source",
                    "-B",
                    "/work/build-ninja",
                    "-G",
                    "Ninja",
                    *cmake_opts,
                ],
            ],
            ["cmake", "--build", "/work/build-ninja", "-v"],
            True,
        ),
        (
            "make_verbose",
            [
                ["cmake", "-E", "rm", "-rf", "/work/build-make"],
                [
                    "cmake",
                    "-S",
                    "/work/source",
                    "-B",
                    "/work/build-make",
                    "-G",
                    "Unix Makefiles",
                    *cmake_opts,
                ],
            ],
            ["cmake", "--build", "/work/build-make", "--clean-first", "-j", "2", "--", "VERBOSE=1"],
            True,
        ),
        (
            "clang_make_verbose",
            [
                [
                    "cmake",
                    "-S",
                    "/work/source",
                    "-B",
                    "/work/build-make-clang",
                    "-G",
                    "Unix Makefiles",
                    "-DCMAKE_C_COMPILER=clang",
                    "-DCMAKE_CXX_COMPILER=clang++",
                    *cmake_opts,
                ]
            ],
            ["cmake", "--build", "/work/build-make-clang", "-j", "2", "--", "VERBOSE=1"],
            True,
        ),
        (
            "clang_ninja_verbose",
            [
                [
                    "cmake",
                    "-S",
                    "/work/source",
                    "-B",
                    "/work/build-ninja-clang",
                    "-G",
                    "Ninja",
                    "-DCMAKE_C_COMPILER=clang",
                    "-DCMAKE_CXX_COMPILER=clang++",
                    *cmake_opts,
                ]
            ],
            ["cmake", "--build", "/work/build-ninja-clang", "-v"],
            True,
        ),
    ]
    if project == "brotli":
        runs.append(
            (
                "clang_ninja_verbose_failure",
                [
                    [
                        "cmake",
                        "-S",
                        "/work/source",
                        "-B",
                        "/work/build-ninja-fail",
                        "-G",
                        "Ninja",
                        "-DCMAKE_C_COMPILER=clang",
                        "-DCMAKE_C_FLAGS=-Weverything -Werror",
                        *cmake_opts,
                    ]
                ],
                ["cmake", "--build", "/work/build-ninja-fail", "-v"],
                False,
            )
        )
    return runs


def build_run_provenance(
    per_run_prep: list[list[str]],
    cmd_args: list[str],
    record: dict[str, object],
) -> dict[str, object]:
    return {
        "setup": per_run_prep,
        "options": cmd_args,
        "exit_code": int(record["exit_code"]),  # type: ignore[arg-type]
        "duration_seconds": record["duration_seconds"],
        "observed_sha256": record["sha256"]["observed"],  # type: ignore[index]
    }


def main() -> None:
    args = build_parser().parse_args()

    is_native_make = args.project in NATIVE_MAKE_PROJECTS
    if is_native_make and args.suite in ("ctest", "build"):
        raise SystemExit(
            f"{args.project} uses native GNU Make and supports only --suite verbose or --suite all"
        )

    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    work = output / "work"
    source = work / "source"
    home = work / "home"
    work.mkdir()
    home.mkdir()

    if is_native_make:
        repo, tag, expected_commit = NATIVE_MAKE_PROJECTS[args.project]
        cmake_opts: list[str] = []
        ctest_expect_zero = True
    else:
        repo, tag, expected_commit, cmake_opts, ctest_expect_zero = PROJECTS[args.project]
    image_id = checked(["docker", "image", "inspect", IMAGE_NAME, "--format", "{{.Id}}"]).strip()
    commit, clone_method = resolve_source(
        args.project, repo, tag, expected_commit, source, args.sources_dir
    )

    tools: tuple[str, ...] = ("cmake", "ctest", "ninja", "gcc", "g++")
    if args.suite == "build":
        tools = ("cmake", "ctest", "ninja", "make", "gcc", "g++")
    elif args.suite in ("verbose", "all"):
        tools = ("cmake", "ctest", "ninja", "make", "gcc", "g++", "clang", "clang++")
    tool_versions = {}
    for tool in tools:
        tool_versions[tool] = checked(
            ["docker", "run", "--rm", "--network", "none", image_id, tool, "--version"]
        ).splitlines()[0].strip()

    (output / "environment.json").write_text(
        json.dumps(
            {
                "repository": repo,
                "tag": tag,
                "commit": commit,
                "cmake_options": cmake_opts,
                "clone": clone_method,
                "image": IMAGE_NAME,
                "image_id": image_id,
                "docker_version": checked(
                    ["docker", "version", "--format", "{{.Server.Version}}"]
                ).strip(),
                "tool_versions": tool_versions,
                "git_version": checked(["git", "--version"]).strip(),
                "python_version": checked(["python3", "--version"]).strip(),
                "cpus": "2",
                "network": "none",
            },
            indent=2,
        )
        + "\n"
    )

    uid_gid = f"{os.getuid()}:{os.getgid()}"
    common_flags = ["--rm", "--network", "none", "--cpus", "2"]
    user_and_env = [
        "--env",
        "HOME=/work/home",
        "--env",
        "TZ=UTC",
        "--env",
        "LANG=C.UTF-8",
    ]
    real_mount = [
        "--user",
        uid_gid,
        *user_and_env,
        "--mount",
        f"type=bind,src={work},dst=/work",
        "--workdir",
        "/work/source",
        image_id,
    ]
    sanitized_prefix = [
        "docker",
        "run",
        *common_flags,
        "--user",
        "1000:1000",
        *user_and_env,
        "--mount",
        "type=bind,src=/work-host/work,dst=/work",
        "--workdir",
        "/work/source",
        image_id,
    ]

    prep_steps: list[list[str]] = []
    if not is_native_make and args.suite != "verbose":
        prep_steps.append(
            ["cmake", "-S", "/work/source", "-B", "/work/build-ninja", "-G", "Ninja", *cmake_opts]
        )
        if args.suite == "ctest":
            prep_steps.append(["cmake", "--build", "/work/build-ninja"])
        elif args.suite in ("build", "all"):
            prep_steps.append(
                [
                    "cmake",
                    "-S",
                    "/work/source",
                    "-B",
                    "/work/build-make",
                    "-G",
                    "Unix Makefiles",
                    *cmake_opts,
                ]
            )
    for step in prep_steps:
        checked(["docker", "run", *common_flags, *real_mount, *step])

    build_runs: list[tuple[str, list[list[str]], list[str], bool]] = [
        ("ninja_cold", [], ["cmake", "--build", "/work/build-ninja"], True),
        ("ninja_incremental", [], ["cmake", "--build", "/work/build-ninja"], True),
        ("make_cold", [], ["cmake", "--build", "/work/build-make", "-j", "2"], True),
        ("make_incremental", [], ["cmake", "--build", "/work/build-make", "-j", "2"], True),
        (
            "failure_unknown_target",
            [],
            ["cmake", "--build", "/work/build-ninja", "--target", "tokenless_nonexistent_target"],
            False,
        ),
    ]
    ctest_runs: list[tuple[str, list[list[str]], list[str], bool]] = [
        (
            "ctest_serial",
            [],
            ["ctest", "--test-dir", "/work/build-ninja", "--output-on-failure"],
            ctest_expect_zero,
        ),
        (
            "ctest_parallel",
            [],
            ["ctest", "--test-dir", "/work/build-ninja", "-j", "2", "--output-on-failure"],
            ctest_expect_zero,
        ),
    ]
    verbose_runs = verbose_runs_for_project(args.project)
    if args.suite == "ctest":
        runs = ctest_runs
    elif args.suite == "build":
        runs = build_runs
    elif args.suite == "verbose":
        runs = verbose_runs
    elif is_native_make:
        runs = verbose_runs
    else:
        runs_by_name = {entry[0]: entry for entry in [*build_runs, *ctest_runs, *verbose_runs]}
        # CTest reads /work/build-ninja produced by ninja_cold, and incremental runs follow cold runs.
        all_suite_order = [
            "ninja_cold",
            "ninja_incremental",
            "ctest_serial",
            "ctest_parallel",
            "make_cold",
            "make_incremental",
            "failure_unknown_target",
            *(entry[0] for entry in verbose_runs),
        ]
        runs = [runs_by_name[name] for name in all_suite_order]

    provenance_runs: dict[str, dict[str, object]] = {}
    for name, per_run_prep, cmd_args, run_expect_zero in runs:
        for step in per_run_prep:
            checked(["docker", "run", *common_flags, *real_mount, *step])
        container_name = f"tokenless-ctest-{args.project}-{name}-{os.getpid()}"
        command = ["docker", "run", *common_flags, "--name", container_name, *real_mount, *cmd_args]
        recorded_command = [*sanitized_prefix, *cmd_args]
        record = capture(command, recorded_command, output / name, container_name=container_name)
        exit_code = int(record["exit_code"])  # type: ignore[arg-type]
        if run_expect_zero and exit_code != 0:
            raise SystemExit(f"{args.project}/{name}: expected exit 0, got {exit_code}")
        if not run_expect_zero and exit_code == 0:
            raise SystemExit(f"{args.project}/{name}: expected non-zero exit, got 0")
        if not run_expect_zero:
            observed_text = (output / name / "observed.txt").read_text(
                encoding="utf-8", errors="replace"
            )
            for marker in FAILURE_DIAGNOSTIC_MARKERS.get(name, ()):
                if marker not in observed_text:
                    raise SystemExit(
                        f"{args.project}/{name}: non-zero exit ({exit_code}) missing expected diagnostic {marker!r}"
                    )
        provenance_runs[name] = build_run_provenance(per_run_prep, cmd_args, record)

    (output / "provenance.json").write_text(
        json.dumps(
            {
                "project": args.project,
                "repository": repo,
                "tag": tag,
                "commit": commit,
                "cmake_options": cmake_opts,
                "image_id": image_id,
                "source_state": "unmodified pinned checkout; no CMakeLists or source edits",
                "runs": provenance_runs,
            },
            indent=2,
        )
        + "\n"
    )
    print(f"{args.project}: captured {len(runs)} runs into {output}", flush=True)


if __name__ == "__main__":
    main()
