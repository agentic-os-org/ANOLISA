#!/usr/bin/env python3
"""Run benchmark coverage plus AgentSight regression gates and preserve all logs."""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import shlex
import shutil
import subprocess
import sys
import time
import tempfile
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parents[4]
AGENTSIGHT_DIR = REPO_ROOT / "src" / "agentsight"
BENCHMARK_PATH = Path("src/agentsight/scripts/benchmark")
DEFAULT_RUST_TOOLCHAIN = "1.89.0"
CI_EXCLUDED_PACKAGES = ("ebpf-ifc-engine", "actplane-ifc-compiler")
REQUIRED_PYTHON_MODULES = ("coverage", "diff_cover", "h2", "pytest")


def write_benchmark_diff(compare_branch: str, output: Path) -> None:
    """Write tracked and untracked benchmark changes without touching the index."""
    tracked = subprocess.run(
        [
            "git",
            "diff",
            "--no-ext-diff",
            "--unified=0",
            compare_branch,
            "--",
            str(BENCHMARK_PATH),
        ],
        cwd=REPO_ROOT,
        capture_output=True,
        check=False,
    )
    if tracked.returncode != 0:
        message = tracked.stderr.decode(errors="replace").strip()
        raise RuntimeError(f"cannot diff local Git ref {compare_branch}: {message}")
    untracked = subprocess.run(
        [
            "git",
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            str(BENCHMARK_PATH),
        ],
        cwd=REPO_ROOT,
        capture_output=True,
        check=False,
    )
    if untracked.returncode != 0:
        message = untracked.stderr.decode(errors="replace").strip()
        raise RuntimeError(f"cannot list untracked benchmark files: {message}")
    combined = bytearray(tracked.stdout)
    for raw_path in untracked.stdout.split(b"\0"):
        if not raw_path or not raw_path.endswith(b".py"):
            continue
        path = os.fsdecode(raw_path)
        addition = subprocess.run(
            [
                "git",
                "diff",
                "--no-index",
                "--no-ext-diff",
                "--unified=0",
                "--",
                "/dev/null",
                path,
            ],
            cwd=REPO_ROOT,
            capture_output=True,
            check=False,
        )
        if addition.returncode not in (0, 1):
            message = addition.stderr.decode(errors="replace").strip()
            raise RuntimeError(f"cannot diff untracked file {path}: {message}")
        combined.extend(addition.stdout)
    output.write_bytes(combined)


def missing_python_modules() -> list[str]:
    """Return benchmark dependencies absent from the active interpreter."""
    return [
        name
        for name in REQUIRED_PYTHON_MODULES
        if importlib.util.find_spec(name) is None
    ]


def run_check(
    command: list[str], cwd: Path, log_path: Path, checks: list[dict[str, object]]
) -> int:
    """Run one check, append its durable record, and return its exit code.

    A check that cannot be launched at all (a missing ``rustup``/``cargo`` is the
    canonical case for this runner) is recorded as a failure instead of raising:
    the exception would escape before the record is appended, so the run died
    with a traceback, published no report (``write_report`` runs only after the
    first check) and skipped every later gate.
    """
    started = time.time()
    with log_path.open("w", encoding="utf-8") as log:
        try:
            result = subprocess.run(
                command,
                cwd=cwd,
                stdout=log,
                stderr=subprocess.STDOUT,
                text=True,
                check=False,
            )
            returncode = result.returncode
        except OSError as error:
            # 127 is the shell's "command not found"; any launch failure is a
            # failed check, and the reason belongs in its log for the report.
            log.write(f"failed to launch {shlex.join(command)}: {error}\n")
            returncode = 127
    checks.append(
        {
            "command": shlex.join(command),
            "cwd": str(cwd),
            "log": log_path.name,
            "started_at_unix": started,
            "duration_seconds": time.time() - started,
            "exit_code": returncode,
        }
    )
    return returncode


def write_report(
    output: Path,
    checks: list[dict[str, object]],
    *,
    full: bool = False,
    cargo_jobs: int | None = None,
) -> None:
    """Publish complete progress atomically, retaining the previous report on failure."""
    payload = (
        json.dumps(
            {
                "schema_version": 1,
                "full": full,
                "cargo_jobs": cargo_jobs,
                "checks": checks,
            },
            indent=2,
            sort_keys=True,
        )
        + "\n"
    )
    target = output.resolve()
    with tempfile.TemporaryDirectory(
        prefix=f".{target.name}-", dir=target.parent
    ) as staging:
        staged = Path(staging) / target.name
        with staged.open("w", encoding="utf-8") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        if target.exists():
            shutil.copymode(target, staged)
        os.replace(staged, target)


def report_progress(label: str, status: int, log_path: Path) -> None:
    """Print one completed check and expose the useful coverage summary."""
    state = "OK" if status == 0 else "FAIL"
    print(f"[{state}] {label} (log: {log_path})", flush=True)
    if not log_path.exists():
        return
    lines = log_path.read_text(encoding="utf-8", errors="replace").splitlines()
    if label == "coverage-report":
        summary = next(
            (
                line.strip()
                for line in reversed(lines)
                if line.strip().startswith("TOTAL")
            ),
            None,
        )
        if summary:
            print(f"  coverage: {summary}", flush=True)
    elif label == "diff-cover":
        summary = next(
            (line.strip() for line in reversed(lines) if "Coverage:" in line),
            None,
        )
        if summary:
            print(f"  diff coverage: {summary}", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--results", type=Path, required=True)
    parser.add_argument("--compare-branch", default="origin/main")
    parser.add_argument("--full", action="store_true", help="run full Rust gates")
    parser.add_argument(
        "--rust-toolchain",
        default=os.environ.get("AGENTSIGHT_RUST_TOOLCHAIN", DEFAULT_RUST_TOOLCHAIN),
        help="Rust toolchain used by AgentSight CI",
    )
    parser.add_argument(
        "--cargo-jobs",
        type=int,
        default=os.environ.get("AGENTSIGHT_CARGO_JOBS", "2"),
        help="maximum Cargo build and Rust test concurrency (default: 2)",
    )
    args = parser.parse_args()
    if args.cargo_jobs <= 0:
        parser.error("--cargo-jobs must be positive")
    results = args.results.resolve()
    results.mkdir(parents=True, exist_ok=True)
    missing = missing_python_modules()
    if missing:
        raise SystemExit(
            "missing Python benchmark dependencies in the active environment: "
            f"{', '.join(missing)}; activate .venv and install scripts/benchmark/requirements.txt"
        )
    checks: list[dict[str, object]] = []
    report_path = results / "regression.json"
    coverage_xml = results / "coverage.xml"
    coverage_data = results / ".coverage"
    diff_file = results / "benchmark.diff"
    cargo = ["rustup", "run", args.rust_toolchain, "cargo"]
    try:
        write_benchmark_diff(args.compare_branch, diff_file)
    except RuntimeError as error:
        raise SystemExit(str(error)) from error
    commands: list[tuple[str, list[str], Path]] = [
        (
            "python-tests",
            [
                sys.executable,
                "-m",
                "coverage",
                "run",
                "--branch",
                f"--data-file={coverage_data}",
                "-m",
                "pytest",
                "-q",
                "src/agentsight/scripts/benchmark/tests",
            ],
            REPO_ROOT,
        ),
        (
            "coverage-report",
            [
                sys.executable,
                "-m",
                "coverage",
                "report",
                f"--data-file={coverage_data}",
                "--include=src/agentsight/scripts/benchmark/single_run/*.py,src/agentsight/scripts/benchmark/campaign/*.py",
                "--fail-under=85",
            ],
            REPO_ROOT,
        ),
        (
            "coverage-xml",
            [
                sys.executable,
                "-m",
                "coverage",
                "xml",
                f"--data-file={coverage_data}",
                "-o",
                str(coverage_xml),
            ],
            REPO_ROOT,
        ),
        (
            "diff-cover",
            [
                sys.executable,
                "-m",
                "diff_cover.diff_cover_tool",
                str(coverage_xml),
                "--diff-file",
                str(diff_file),
                "--fail-under=85",
            ],
            REPO_ROOT,
        ),
    ]
    if args.full:
        commands.extend(
            [
                (
                    "cargo-fmt",
                    [*cargo, "fmt", "--all", "--", "--check"],
                    AGENTSIGHT_DIR,
                ),
                (
                    "cargo-clippy",
                    [
                        *cargo,
                        "clippy",
                        "--workspace",
                        "--exclude",
                        CI_EXCLUDED_PACKAGES[0],
                        "--exclude",
                        CI_EXCLUDED_PACKAGES[1],
                        "--all-targets",
                        "--locked",
                        "--jobs",
                        str(args.cargo_jobs),
                        "--",
                        "-D",
                        "warnings",
                    ],
                    AGENTSIGHT_DIR,
                ),
                (
                    "cargo-test",
                    [
                        *cargo,
                        "test",
                        "--workspace",
                        "--exclude",
                        CI_EXCLUDED_PACKAGES[0],
                        "--exclude",
                        CI_EXCLUDED_PACKAGES[1],
                        "--locked",
                        "--jobs",
                        str(args.cargo_jobs),
                        "--",
                        "--test-threads",
                        str(args.cargo_jobs),
                    ],
                    AGENTSIGHT_DIR,
                ),
            ]
        )
    else:
        for name in (
            "sustained_load_never_exceeds_byte_budget",
            "concurrent_drain_leaves_no_phantom_reservation",
            "test_decode_chunked_json_binary_body_does_not_panic",
            "test_oversized_request_body_pending_is_evicted",
        ):
            commands.append(
                (
                    f"cargo-{name}",
                    [
                        *cargo,
                        "test",
                        "--jobs",
                        str(args.cargo_jobs),
                        "--lib",
                        name,
                        "--",
                        "--test-threads",
                        str(args.cargo_jobs),
                    ],
                    AGENTSIGHT_DIR,
                )
            )
    failed = False
    for label, command, cwd in commands:
        log_path = results / f"{label}.log"
        status = run_check(command, cwd, log_path, checks)
        report_progress(label, status, log_path)
        failed |= status != 0
        write_report(report_path, checks, full=args.full, cargo_jobs=args.cargo_jobs)
    print("\nRegression artifacts:", flush=True)
    print(f"  result directory: {results}", flush=True)
    print(f"  report: {report_path}", flush=True)
    print(f"  coverage XML: {coverage_xml}", flush=True)
    print(f"  diff: {diff_file}", flush=True)
    print(f"  overall: {'FAIL' if failed else 'PASS'}", flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
