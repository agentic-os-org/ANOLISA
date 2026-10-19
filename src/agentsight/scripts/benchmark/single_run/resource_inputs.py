"""Freeze Git or binary inputs without changing the caller's checkout or index."""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import importlib.util
import math
import os
import shutil
import site
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Callable


def arguments(component: Path) -> argparse.Namespace:
    """Expose common load controls and explicit Git/prebuilt-binary comparison sources."""
    parser = argparse.ArgumentParser(
        prog="resource_run.sh", description="Measure AgentSight RSS and CPU with optional Git A/B."
    )
    parser.add_argument("--ab", action="store_true", help="run before/after sequentially")
    parser.add_argument("--baseline", help="baseline commit, branch or tag; required with --ab")
    parser.add_argument(
        "--candidate",
        default="working-tree",
        help="Git ref or working-tree (default, includes uncommitted changes)",
    )
    parser.add_argument("--baseline-bin", type=Path, help="prebuilt baseline instead of a Git ref")
    parser.add_argument(
        "--candidate-bin",
        "--agentsight-bin",
        type=Path,
        help="prebuilt candidate instead of a Git ref",
    )
    parser.add_argument("--config", type=Path, default=component / "agentsight.json")
    parser.add_argument(
        "--cpu-budget-percent",
        type=int,
        help="cooperative process CPU target; 0 disables, 100%% = one logical CPU (default: config or 25)",
    )
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument(
        "--connection-capacity", type=int, help="HTTP connection slots (default: agent count)"
    )
    parser.add_argument(
        "--ring-buffer-mb", type=int, help="ring buffer MiB; power of two (default: --config value)"
    )
    parser.add_argument(
        "--no-sudo",
        action="store_true",
        help="run with current user; requires eBPF permissions/capabilities",
    )
    for name, kind, default in (
        ("agents", int, 32),
        ("tokens-per-second", float, 100),
        ("duration", float, 60),
        ("tokens-per-response", int, 1000),
        ("payload-kb", int, 4),
        ("startup-delay", float, 5),
        ("rate-tolerance", float, 0.05),
        ("port", int, 8443),
        ("validation-wait-seconds", float, 30),
        ("ready-timeout", float, 30),
        ("max-results-gb", int, 30),
        ("min-free-disk-gb", int, 5),
        ("min-available-memory-mb", int, 2048),
        ("max-agentsight-rss-mb", int, 1536),
    ):
        parser.add_argument(f"--{name}", type=kind, default=default, help=f"default: {default}")
    parser.add_argument("--worker", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.worker:
        return args
    if args.connection_capacity is None:
        args.connection_capacity = args.agents
    if args.ab and not (args.baseline or args.baseline_bin):
        parser.error("--ab requires --baseline REF or --baseline-bin PATH")
    if not args.ab and (args.baseline or args.baseline_bin):
        parser.error("baseline options require --ab")
    for name, value in vars(args).items():
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            minimum = (
                0
                if name in ("startup_delay", "validation_wait_seconds", "cpu_budget_percent")
                else 1e-12
            )
            if not math.isfinite(value) or value < minimum:
                parser.error(f"--{name.replace('_', '-')} must be finite and >= {minimum}")
    if args.rate_tolerance >= 1 or args.port > 65535:
        parser.error("rate-tolerance must be < 1 and port must be <= 65535")
    if args.cpu_budget_percent is not None and args.cpu_budget_percent > 100:
        parser.error("--cpu-budget-percent must be between 0 and 100")
    if args.ring_buffer_mb is not None and args.ring_buffer_mb & (args.ring_buffer_mb - 1):
        parser.error("--ring-buffer-mb must be a positive power of two")
    return args


def ensure_python(
    cache: Path,
    harness: Path,
    arguments: list[str],
    run: Callable[..., int],
    *,
    require_venv: bool = False,
) -> None:
    """Use installed dependencies or prepare an isolated reusable virtual environment."""
    try:
        version = tuple(int(part) for part in importlib.metadata.version("tiktoken").split(".")[:2])
        isolated = sys.prefix != sys.base_prefix and not site.ENABLE_USER_SITE
        if (
            (0, 8) <= version < (1, 0)
            and importlib.util.find_spec("tiktoken") is not None
            and (not require_venv or isolated)
        ):
            return
    except (importlib.metadata.PackageNotFoundError, ValueError):
        pass
    print("Preparing benchmark Python dependencies...", flush=True)
    cache.mkdir(parents=True, exist_ok=True)
    interpreter = cache / "venv/bin/python3"
    command = [str(interpreter), "-B", str(harness / "resource_run.py"), *arguments]
    # Recheck a cached venv before installing; user-site packages disappear under sudo.
    if interpreter.exists() and Path(sys.prefix).resolve() != interpreter.parents[1].resolve():
        os.execv(str(interpreter), command)
    if not interpreter.exists():
        if run([sys.executable, "-m", "venv", str(cache / "venv")], cache / "setup.log"):
            raise RuntimeError(f"cannot create Python venv; see {cache / 'setup.log'}")
    if run(
        [str(interpreter), "-m", "pip", "install", "-r", str(harness.parent / "requirements.txt")],
        cache / "setup.log",
    ):
        raise RuntimeError(f"cannot install Python dependencies; see {cache / 'setup.log'}")
    os.execv(str(interpreter), command)


def digest(path: Path) -> str:
    """Hash a binary without retaining it in memory."""
    result = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            result.update(chunk)
    return result.hexdigest()


class Inputs:
    """Own temporary detached worktrees and a private working-tree snapshot index."""

    def __init__(self, repo: Path, output: Path) -> None:
        self.repo = repo
        self.output = output
        self.temporary = tempfile.TemporaryDirectory(prefix="agentsight-resource-inputs-")
        self.worktrees: list[Path] = []

    def git(self, *arguments: str, env: dict[str, str] | None = None) -> str:
        """Run Git with argument boundaries preserved, surfacing its failure reason."""
        result = subprocess.run(
            ["git", "-C", str(self.repo), *arguments], capture_output=True, text=True, env=env
        )
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or f"git failed: {arguments}")
        return result.stdout.strip()

    def prepare(self, label: str, ref: str, binary: Path | None) -> dict[str, Any]:
        """Freeze a prebuilt binary or materialize a commit/working-tree source."""
        destination = self.output / label
        destination.mkdir()
        copied_binary = destination / "agentsight"
        if binary is not None:
            binary = binary.expanduser().resolve()
            if not binary.exists():
                raise RuntimeError(f"AgentSight binary does not exist: {binary}")
            if not binary.is_file():
                raise RuntimeError(f"AgentSight binary is not a regular file: {binary}")
            if not os.access(binary, os.X_OK):
                raise RuntimeError(f"AgentSight binary is not executable: {binary}")
            shutil.copy2(binary, copied_binary)
            return {"kind": "binary", "path": str(binary), "binary": str(copied_binary)}

        commit = self.git(
            "rev-parse",
            "--verify",
            "--end-of-options",
            f"{'HEAD' if ref == 'working-tree' else ref}^{{commit}}",
        )
        tree = self.git("rev-parse", f"{commit}^{{tree}}")
        dirty = False
        if ref == "working-tree":
            env = dict(os.environ, GIT_INDEX_FILE=str(Path(self.temporary.name) / f"{label}.index"))
            self.git("read-tree", commit, env=env)
            paths = ["."]
            if self.output.is_relative_to(self.repo):
                relative = self.output.relative_to(self.repo).as_posix()
                paths.extend([f":(exclude){relative}", f":(exclude){relative}/**"])
            self.git("add", "-A", "--", *paths, env=env)
            snapshot = self.git("write-tree", env=env)
            dirty = snapshot != tree
            tree = snapshot

        worktree = Path(self.temporary.name) / label
        self.git("worktree", "add", "--quiet", "--detach", str(worktree), commit)
        self.worktrees.append(worktree)
        if ref == "working-tree":
            self.git("-C", str(worktree), "read-tree", "--reset", "-u", tree)
        component = worktree / "src/agentsight"
        if not (component / "Cargo.toml").is_file():
            raise RuntimeError(f"{ref} has no src/agentsight/Cargo.toml")
        return {
            "kind": "git",
            "ref": ref,
            "commit": commit,
            "tree": tree,
            "dirty": dirty,
            "component": str(component),
            "binary": str(copied_binary),
        }

    def build(self, source: dict[str, Any], run: Callable[..., int], cache: Path) -> None:
        """Build every Git input with the same release profile, caching dependencies."""
        if source["kind"] == "git":
            env = dict(os.environ, CARGO_TARGET_DIR=str(cache / "target"))
            for key in ("DESTDIR", "MAKEFLAGS", "MFLAGS", "MAKEOVERRIDES"):
                env.pop(key, None)
            if "LIBBPF_SYS_LIBRARY_PATH" not in env and shutil.which("pkg-config"):
                result = subprocess.run(
                    ["pkg-config", "--variable=libdir", "libbpf"],
                    capture_output=True,
                    text=True,
                )
                if result.returncode == 0 and result.stdout.strip():
                    env["LIBBPF_SYS_LIBRARY_PATH"] = result.stdout.strip()
            command = ["cargo", "build", "--release", "--locked", "--bin", "agentsight"]
            log = Path(source["binary"]).parent / "build.log"
            if run(command, log, cwd=source["component"], env=env):
                raise RuntimeError(f"release build failed; see {log}")
            shutil.copy2(cache / "target/release/agentsight", source["binary"])
            source["build_command"] = command
        source["sha256"] = digest(Path(source["binary"]))

    def close(self) -> None:
        """Remove only worktrees created by this invocation."""
        for worktree in reversed(self.worktrees):
            self.git("worktree", "remove", "--force", str(worktree))
        self.temporary.cleanup()
