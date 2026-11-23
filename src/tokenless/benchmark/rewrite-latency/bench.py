#!/usr/bin/env python3
"""Compare validated QwenWork PreTool invocations without executing commands."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import random
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

CASES = {
    "git": ("git status", "replace_arguments"),
    "compound": ("git status && ls -la .", "replace_arguments"),
    "native_find": ("git status && ls -la /tmp && find . -name '*.py'", "passthrough"),
    "native_grep": (
        "git status && ls -la /tmp && cat package.json | grep version",
        "passthrough",
    ),
    "unsupported": ("printf hello", "passthrough"),
    "empty": ("", "passthrough"),
}


def pre_tool_request(command: str, call_id: str) -> dict:
    return {
        "protocol_version": 2,
        "operation": "pre_tool",
        "attribution": {
            "agent_id": "qwenwork",
            "session_id": "rewrite-latency",
            "tool_use_id": call_id,
        },
        "input": {
            "tool_name": "Bash",
            "arguments": {"command": command, "timeout": 30},
            "command_field": "command",
            "capabilities": {"replace_arguments": True, "block_and_suggest": False},
        },
    }


def hook_payload(request: dict, cwd: Path) -> dict:
    return {
        "session_id": request["attribution"]["session_id"],
        "tool_use_id": request["attribution"]["tool_use_id"],
        "tool_name": "mcp__qw-builtin__mcp_workspace_bash",
        "cwd": str(cwd),
        "tool_input": request["input"]["arguments"],
    }


def validate_response(layer: str, raw: str, request: dict, expected: str) -> str:
    """Reject fail-open hooks and protocol errors masquerading as fast rewrites."""
    value = json.loads(raw)
    original = request["input"]["arguments"]
    if layer == "hook":
        if expected == "passthrough":
            if value != {}:
                raise ValueError(f"expected a skipped hook, received {value!r}")
            return original["command"]
        envelope = value["hookSpecificOutput"]
        if envelope["hookEventName"] != "PreToolUse":
            raise ValueError("wrong hook event")
        arguments = envelope["updatedInput"]
        if envelope["tool_input"] != {"command": arguments["command"]}:
            raise ValueError("host command fields disagree")
    else:
        if value.get("protocol_version") != 2 or value.get("operation") != "pre_tool":
            raise ValueError("wrong response protocol or operation")
        result = value["result"]
        optimization = "rtk" if expected == "replace_arguments" else "none"
        if (
            result["action"] != expected
            or result["output_optimization"] != optimization
        ):
            raise ValueError(f"expected {expected}/{optimization}, received {result!r}")
        arguments = result["arguments"]
    if {k: v for k, v in arguments.items() if k != "command"} != {
        k: v for k, v in original.items() if k != "command"
    }:
        raise ValueError("non-command arguments changed")
    command = arguments["command"]
    if not isinstance(command, str):
        raise ValueError("command is not a string")
    if expected == "replace_arguments":
        if (
            not command
            or command == original["command"]
            or not any(Path(token).name == "rtk" for token in shlex.split(command))
        ):
            raise ValueError("rewrite was not applied")
    elif arguments != original:
        raise ValueError("passthrough changed the original arguments")
    return command


def shell_path_forms(path: Path) -> list[str]:
    # Core quotes more characters (e.g. '+') and escapes apostrophes differently
    # from shlex. Replace only these known paths, leaving shell syntax intact.
    value = str(path)
    core_quoted = "'" + value.replace("'", "'\\''") + "'"
    return sorted({core_quoted, shlex.quote(value)}, key=len, reverse=True)


def normalize_command(command: str, plugin: Path, state: Path) -> str:
    # v0.9.1 launches a standalone RTK; current versions use the same command
    # through `tokenless rtk`. Preserve its arguments when comparing versions.
    for path, suffix in (
        (plugin / "bin/tokenless", " rtk "),
        (plugin / "bin/rtk", " "),
    ):
        for quoted in shell_path_forms(path):
            command = command.replace(quoted + suffix, "<rtk> ")
    paths = ((plugin / "bin/tokenless", "<tokenless>"), (state, "<state>"))
    for path, replacement in paths:
        for quoted in shell_path_forms(path):
            command = command.replace(quoted, replacement)
    return re.sub(
        r"TOKENLESS_RTK_RUN_ID=[0-9a-f]{32}", "TOKENLESS_RTK_RUN_ID=<run>", command
    )


def version_contract(plugin: Path, version: str) -> tuple[list[str], dict[str, str]]:
    """Keep the known v0.9.1 grep policy distinct from current native passthrough."""
    actions = {name: action for name, (_, action) in CASES.items()}
    if version == "tokenless 0.9.1":
        actions["native_grep"] = "replace_arguments"
        return [str(plugin / "bin/rtk")], actions
    return [str(plugin / "bin/tokenless"), "rtk"], actions


def summarize(values: list[float]) -> dict:
    ordered = sorted(values)
    return {
        "n": len(values),
        "median_ms": statistics.median(values),
        "mean_ms": statistics.mean(values),
        "p90_ms": ordered[math.ceil(len(values) * 0.90) - 1],
        "p95_ms": ordered[math.ceil(len(values) * 0.95) - 1],
        "min_ms": ordered[0],
        "max_ms": ordered[-1],
    }


def run_process(
    args: list[str], data: str, env: dict, cwd: Path
) -> tuple[float, subprocess.CompletedProcess]:
    start = time.perf_counter_ns()
    result = subprocess.run(
        args, input=data, capture_output=True, text=True, env=env, cwd=cwd, timeout=12
    )
    elapsed = (time.perf_counter_ns() - start) / 1_000_000
    return elapsed, result


def fingerprint(plugin: Path) -> dict:
    binary = plugin / "bin/tokenless"
    files = sorted(
        p
        for p in plugin.rglob("*")
        if p.is_file()
        and p.suffix in {".py", ".sh", ".json"}
        and "__pycache__" not in p.parts
    )
    digest = hashlib.sha256()
    for path in files:
        digest.update(
            str(path.relative_to(plugin)).encode() + b"\0" + path.read_bytes()
        )
    result = {
        "plugin": str(plugin),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "adapter_sha256": digest.hexdigest(),
    }
    build = plugin / "build.json"
    if build.is_file():
        result["build"] = json.loads(build.read_text())
    return result


def benchmark(
    variants: dict[str, Path], cwd: Path, samples: int, warmup: int, seed: int
) -> dict:
    report = {
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python": sys.version,
        "hook_python": shutil.which("python3"),
        "cwd": str(cwd),
        "samples": samples,
        "warmup": warmup,
        "seed": seed,
        "cases": {name: {"command": cmd} for name, (cmd, _) in CASES.items()},
        "variants": {},
    }
    # All writes go to private, initially empty databases. Config and project
    # rules remain the caller's; unexpected rewrites/denials invalidate the run.
    with tempfile.TemporaryDirectory(prefix="tokenless-rewrite-bench-") as temp:
        temp_root = Path(temp).resolve()
        contexts = {}
        jobs = []
        timings = {}
        canonical = {}
        for label, plugin in variants.items():
            state = temp_root / label
            state.mkdir()
            env = {
                **os.environ,
                "QODER_PLUGIN_ROOT": str(plugin),
                "PATH": str(plugin / "bin") + os.pathsep + os.environ.get("PATH", ""),
                "TOKENLESS_DATA_DIR": str(state),
                "TOKENLESS_STATS_DB": str(state / "stats.db"),
                "TOKENLESS_STASH_DB": str(state / "stash.db"),
                "TOKENLESS_STATS_ENABLED": "1",
                "TOKENLESS_SLS_ENABLED": "0",
                "TOKENLESS_COMPRESSION_ENABLED": "1",
            }
            for name in ("DATA_DIR", "STATS_DB", "STASH_DB"):
                env.pop(f"DSH_TOKENLESS_{name}", None)
            metadata = fingerprint(plugin)
            _, result = run_process(
                [str(plugin / "bin/tokenless"), "--version"], "", env, cwd
            )
            if result.returncode or result.stderr:
                raise ValueError(f"{label} version: {result.stderr or result.stdout}")
            metadata["version"] = result.stdout.strip()
            rtk, actions = version_contract(plugin, metadata["version"])
            _, result = run_process([*rtk, "--version"], "", env, cwd)
            if result.returncode or result.stderr:
                raise ValueError(
                    f"{label} rtk_version: {result.stderr or result.stdout}"
                )
            metadata["rtk_version"] = result.stdout.strip()
            metadata["rtk_command"] = rtk
            metadata["rtk_binary_sha256"] = hashlib.sha256(
                Path(rtk[0]).read_bytes()
            ).hexdigest()
            metadata["expected_actions"] = actions
            metadata["normalized_commands"] = {}
            contexts[label] = (plugin, state, env, rtk, actions)
            report["variants"][label] = metadata
            for name, action in actions.items():
                layers = ["hook", "pre_tool"]
                if action == "replace_arguments":
                    layers.append("rtk_cli")
                for layer in layers:
                    job = (label, name, layer)
                    jobs.append(job)
                    timings[job] = []

        rng = random.Random(seed)
        for iteration in range(warmup + samples):
            rng.shuffle(jobs)
            for label, name, layer in jobs:
                plugin, state, env, rtk, actions = contexts[label]
                command, _ = CASES[name]
                expected = actions[name]
                # Reuse one ID per case/layer, as the original benchmark did;
                # database cardinality and hook-state scan costs stay constant.
                request = pre_tool_request(command, f"{name}-{layer}")
                binary = str(plugin / "bin/tokenless")
                if layer == "hook":
                    args = [
                        "bash",
                        str(plugin / "hooks/run-hook.sh"),
                        "rewrite_hook.py",
                        "--agent-id",
                        "qwenwork",
                    ]
                    data = json.dumps(hook_payload(request, cwd))
                elif layer == "pre_tool":
                    args, data = [binary, "compress"], json.dumps(request)
                else:
                    args, data = [*rtk, "rewrite", command], ""
                elapsed, result = run_process(args, data, env, cwd)
                allowed_codes = (0, 3) if layer == "rtk_cli" else (0,)
                if result.returncode not in allowed_codes or result.stderr:
                    raise ValueError(
                        f"{label}/{name}/{layer}: "
                        f"exit {result.returncode}: {result.stderr}"
                    )
                if layer == "rtk_cli":
                    rewritten = result.stdout.strip()
                    if not rewritten or rewritten == command:
                        raise ValueError(f"{label}/{name}: RTK did not rewrite")
                else:
                    rewritten = validate_response(
                        layer, result.stdout, request, expected
                    )
                    rewritten = normalize_command(rewritten, plugin, state)
                    # The correlation IDs differ only by entrypoint; compare
                    # the actual rewritten command across layers and builds.
                    rewritten = rewritten.replace(f"{name}-{layer}", f"{name}-<layer>")
                # A policy change (e.g. grep becoming native) is a separate
                # behavior, not an equivalent rewrite becoming faster.
                key = (name, expected, "rtk_cli" if layer == "rtk_cli" else "pre_tool")
                previous = canonical.setdefault(key, rewritten)
                if previous != rewritten:
                    raise ValueError(
                        f"{label}/{name}/{layer}: "
                        "rewrite differs from the comparison baseline"
                    )
                report["variants"][label]["normalized_commands"][
                    f"{name}/{layer}"
                ] = rewritten
                if iteration >= warmup:
                    timings[(label, name, layer)].append(elapsed)
            if (iteration + 1) % 5 == 0:
                print(
                    f"Completed {iteration + 1}/{warmup + samples} rounds",
                    file=sys.stderr,
                )
        for label in variants:
            report["variants"][label]["measurements"] = {
                f"{name}/{layer}": {**summarize(values), "samples_ms": values}
                for (variant, name, layer), values in timings.items()
                if variant == label
            }
    return report


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--variant", action="append", required=True, metavar="LABEL=PLUGIN_ROOT"
    )
    parser.add_argument("--cwd", type=Path, default=Path.cwd())
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=5)
    parser.add_argument("--seed", type=int, default=1)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if args.samples < 1 or args.warmup < 0:
        parser.error("samples must be positive and warmup non-negative")
    variants = {}
    for value in args.variant:
        label, separator, directory = value.partition("=")
        if (
            not separator
            or not re.fullmatch(r"[A-Za-z0-9_-]+", label)
            or label in variants
        ):
            parser.error("variants require unique LABEL=PLUGIN_ROOT entries")
        root = Path(directory).resolve(strict=True)
        required = (
            "bin/tokenless",
            "hooks/run-hook.sh",
            "hooks/shared/rewrite_hook.py",
        )
        for name in required:
            if not (root / name).is_file():
                parser.error(f"missing plugin resource: {root / name}")
        variants[label] = root
    report = benchmark(
        variants, args.cwd.resolve(strict=True), args.samples, args.warmup, args.seed
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print("variant\tcase/layer\tmedian_ms\tp90_ms")
    for label, variant in report["variants"].items():
        for name, result in variant["measurements"].items():
            print(f"{label}\t{name}\t{result['median_ms']:.2f}\t{result['p90_ms']:.2f}")


if __name__ == "__main__":
    main()
