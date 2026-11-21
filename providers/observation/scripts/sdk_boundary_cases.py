"""Compare syscall denial and tool-entry injection on pinned SDK tool fixtures."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
from typing import Any


def verify_artifact(marker: Path, mode: str, entered: bool, denied: bool) -> bool:
    """Check exact fixture effects, including absence and empty inherited files."""
    if mode == "inherited_fd" and not entered:
        assert marker.is_file() and marker.read_bytes() == b"", "blocked inherited file changed"
        return False
    if entered and mode != "memory" and not denied:
        assert (
            marker.is_file() and marker.read_bytes() == b"synthetic"
        ), "expected exact fixture bytes"
        return True
    assert not os.path.lexists(marker), "blocked operation left a filesystem artifact"
    return False


def run_case(
    root: Path,
    sources: Path,
    python: Path,
    npm: Path,
    sdk: str,
    mode: str,
    policy: str,
    module_path: Path | None,
) -> dict[str, Any]:
    case = root / f"{sdk}-{mode}-{policy}"
    case.mkdir()
    marker = case / "effect"
    descriptor = os.open(marker, os.O_CREAT | os.O_RDWR, 0o600) if mode == "inherited_fd" else -1
    environment = {
        "PATH": "/usr/bin:/bin",
        "HOME": str(root / "home"),
        "PYTHONNOUSERSITE": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
    }
    if module_path is not None:
        environment["PYTHONPATH"] = str(module_path.resolve())
    if sdk == "qoder":
        node = shutil.which("node")
        if node is None:
            raise RuntimeError("Node.js is required")
        application = [
            node,
            str(sources / "fixtures/sdk_boundary_qoder.mjs"),
            str(npm),
            mode,
            str(marker),
            str(descriptor),
        ]
    else:
        application = [
            str(python),
            str(sources / "fixtures/sdk_boundary_python.py"),
            sdk,
            mode,
            str(marker),
            str(descriptor),
        ]
    if policy.startswith("runtime_"):
        command = [
            str(python),
            str(sources / "scripts/sdk_boundary_inject.py"),
            "--decision",
            policy[8:],
            *application[1:],
        ]
    else:
        command = [sys.executable, str(sources / "scripts/sdk_boundary_launcher.py")]
        if policy == "os_deny":
            command += ["--deny"]
        command += ["--", *application]
    try:
        result = subprocess.run(
            command,
            cwd=case,
            env=environment,
            pass_fds=(descriptor,) if descriptor >= 0 else (),
            capture_output=True,
            text=True,
            timeout=30,
        )
    finally:
        if descriptor >= 0:
            os.close(descriptor)
    (case / "stdout").write_text(result.stdout)
    (case / "stderr").write_text(result.stderr)
    if result.returncode != 0:
        raise AssertionError(f"{case.name}: process exit {result.returncode}")
    observed = json.loads(result.stdout.strip())
    expected_entered = int(policy != "runtime_block" or sdk == "langchain_direct")
    assert observed["entered"] == expected_entered, case.name
    expected_error = (
        "EPERM" if policy == "os_deny" and mode in ("exec", "file", "partial") else None
    )
    assert observed["operation_error"] == expected_error, case.name
    assert observed["memory_effect"] == int(
        mode in ("memory", "partial") and expected_entered
    ), case.name
    effect = verify_artifact(marker, mode, bool(expected_entered), expected_error is not None)
    metadata = json.loads(result.stderr) if result.stderr.strip() else {}
    if policy == "os_deny":
        assert observed["no_new_privs"] is True, case.name
        assert metadata["initial_exec_allowed"] is True, case.name
        assert metadata["exec_denied"] == int(mode == "exec"), case.name
    if policy.startswith("runtime_"):
        assert len(metadata["checks"]) == int(sdk != "langchain_direct"), case.name
    observed.pop("dispatch_result", None)
    return {
        "case": case.name,
        "status": "PASS",
        "policy": policy,
        "side_effect": effect,
        **observed,
        **metadata,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--python", type=Path, required=True)
    parser.add_argument("--module-path", type=Path)
    parser.add_argument("--npm", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    (root / "home").mkdir()
    sources = Path(__file__).resolve().parents[1]
    cases = []
    for sdk in ("langchain", "claude", "qoder"):
        for mode in ("exec", "file", "memory", "partial", "inherited_fd"):
            for policy in ("os_allow", "os_deny"):
                cases.append((sdk, mode, policy))
    for sdk in ("langchain", "langchain_async"):
        for mode in ("exec", "file", "memory", "partial", "inherited_fd"):
            for policy in ("runtime_allow", "runtime_block"):
                cases.append((sdk, mode, policy))
    cases.append(("langchain_direct", "memory", "runtime_block"))
    results = []
    for sdk, mode, policy in cases:
        try:
            results.append(
                run_case(
                    root,
                    sources,
                    args.python.absolute(),
                    args.npm.resolve(),
                    sdk,
                    mode,
                    policy,
                    args.module_path,
                )
            )
        except Exception as error:
            results.append(
                {
                    "case": f"{sdk}-{mode}-{policy}",
                    "status": "FAIL",
                    "error_type": type(error).__name__,
                    "error": str(error),
                }
            )
        (root / "results.json").write_text(json.dumps(results, indent=2) + "\n")
        print(json.dumps(results[-1]), flush=True)
    if any(result["status"] != "PASS" for result in results):
        sys.exit(1)


if __name__ == "__main__":
    main()
