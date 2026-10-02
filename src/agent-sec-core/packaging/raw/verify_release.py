#!/usr/bin/env python3
"""Validate source metadata and staged output for raw packaging."""

import argparse
import json
import re
import tomllib
from pathlib import Path

SEMVER_PATTERN = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
    r"(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$"
)

NATIVE_PYTHON_PATTERN = re.compile(
    r"(?<![\w./-])(?:[\w./-]+/)?python(?:\d+(?:\.\d+)*)?(?=$|[\s;&|()])"
)

REQUIRED_RUNTIME_DEPENDENCIES = {
    "bubblewrap",
    "gnupg",
    "jq",
    "nodejs",
    "systemd",
}

RPM_RESOURCE_ROOTS = {
    "openclaw": "/opt/agent-sec/openclaw-plugin/",
    "hermes": "/opt/agent-sec/hermes-plugin/src/",
    "codex": "/opt/agent-sec/codex-plugin/hooks-plugin/",
    "qoder": "/opt/agent-sec/qoder-plugin/",
    "qwencode": "/opt/agent-sec/qwen-code-extension/",
    "cosh": "{datadir}/extensions/agent-sec-core/",
}

RAW_HOOK_MANIFESTS = (
    Path("adapters/sec-core/codex/hooks/hooks.json"),
    Path("adapters/sec-core/qoder/hooks/hooks.json"),
    Path("adapters/sec-core/qwencode/qwen-extension.json"),
    Path("adapters/sec-core/cosh/cosh-extension.json"),
)


def read_toml(path: Path) -> dict[str, object]:
    """Read a TOML document from ``path``."""
    with path.open("rb") as stream:
        return tomllib.load(stream)


def read_json_version(path: Path) -> str:
    """Read the top-level version string from a JSON manifest."""
    document = json.loads(path.read_text(encoding="utf-8"))
    version = document.get("version")
    if not isinstance(version, str) or not version:
        raise SystemExit(f"ERROR: {path} has no string version")
    return version


def read_hermes_version(path: Path) -> str:
    """Read the simple top-level version field from the Hermes manifest."""
    match = re.search(
        r"(?m)^version:\s*[\"']?([^\"'\s]+)", path.read_text(encoding="utf-8")
    )
    if match is None:
        raise SystemExit(f"ERROR: {path} has no version")
    return match.group(1)


def verify_contract_metadata(contract: dict[str, object], contract_path: Path) -> None:
    """Verify packaging-owned dependencies and RPM adapter resource roots."""
    component = contract.get("component")
    if not isinstance(component, dict):
        raise SystemExit(f"ERROR: {contract_path} has no [component] table")

    dependencies = component.get("dependencies")
    if not isinstance(dependencies, list):
        raise SystemExit(f"ERROR: {contract_path} has no component dependencies")
    dependencies_by_name = {
        dependency.get("name"): dependency
        for dependency in dependencies
        if isinstance(dependency, dict) and isinstance(dependency.get("name"), str)
    }
    missing_dependencies = sorted(
        REQUIRED_RUNTIME_DEPENDENCIES - dependencies_by_name.keys()
    )
    if missing_dependencies:
        raise SystemExit(
            f"ERROR: {contract_path} is missing runtime dependencies: "
            + ", ".join(missing_dependencies)
        )

    if "python3" in dependencies_by_name:
        raise SystemExit(
            f"ERROR: {contract_path} must not declare the bundled Python as a dependency"
        )

    adapters = contract.get("adapters")
    if not isinstance(adapters, list):
        raise SystemExit(f"ERROR: {contract_path} has no adapters")
    adapters_by_framework = {
        adapter.get("framework"): adapter
        for adapter in adapters
        if isinstance(adapter, dict) and isinstance(adapter.get("framework"), str)
    }
    for framework, expected_root in RPM_RESOURCE_ROOTS.items():
        adapter = adapters_by_framework.get(framework)
        if not isinstance(adapter, dict):
            raise SystemExit(
                f"ERROR: {contract_path} has no {framework!r} adapter declaration"
            )
        backends = adapter.get("backends")
        rpm = backends.get("rpm") if isinstance(backends, dict) else None
        actual_root = rpm.get("resource_root") if isinstance(rpm, dict) else None
        if actual_root != expected_root:
            raise SystemExit(
                f"ERROR: {contract_path} {framework!r} RPM resource root is "
                f"{actual_root!r}, expected {expected_root!r}"
            )


def verify_versions(source_root: Path, contract_path: Path) -> str:
    """Verify the raw contract identity and all packaged component versions."""
    contract = read_toml(contract_path)
    component = contract.get("component")
    if not isinstance(component, dict):
        raise SystemExit(f"ERROR: {contract_path} has no [component] table")
    if component.get("name") != "sec-core":
        raise SystemExit(
            f"ERROR: {contract_path} component name is {component.get('name')!r}, "
            "expected 'sec-core'"
        )
    expected = component.get("version")
    if not isinstance(expected, str) or SEMVER_PATTERN.fullmatch(expected) is None:
        raise SystemExit(
            f"ERROR: {contract_path} component version is not valid SemVer: {expected!r}"
        )
    verify_contract_metadata(contract, contract_path)

    project = read_toml(source_root / "agent-sec-cli" / "pyproject.toml").get("project")
    if not isinstance(project, dict) or not isinstance(project.get("version"), str):
        raise SystemExit("ERROR: agent-sec-cli/pyproject.toml has no project version")

    versions = {
        "agent-sec-cli/pyproject.toml": project["version"],
        "openclaw-plugin/openclaw.plugin.json": read_json_version(
            source_root / "openclaw-plugin" / "openclaw.plugin.json"
        ),
        "openclaw-plugin/package.json": read_json_version(
            source_root / "openclaw-plugin" / "package.json"
        ),
        "hermes-plugin/src/plugin.yaml": read_hermes_version(
            source_root / "hermes-plugin" / "src" / "plugin.yaml"
        ),
        "codex-plugin/hooks-plugin/.codex-plugin/plugin.json": read_json_version(
            source_root
            / "codex-plugin"
            / "hooks-plugin"
            / ".codex-plugin"
            / "plugin.json"
        ),
        "qoder-plugin/.qoder-plugin/plugin.json": read_json_version(
            source_root / "qoder-plugin" / ".qoder-plugin" / "plugin.json"
        ),
        "qwen-code-extension/qwen-extension.json": read_json_version(
            source_root / "qwen-code-extension" / "qwen-extension.json"
        ),
        "cosh-extension/cosh-extension.json": read_json_version(
            source_root / "cosh-extension" / "cosh-extension.json"
        ),
    }
    mismatches = [
        f"{path}={version}" for path, version in versions.items() if version != expected
    ]
    if mismatches:
        raise SystemExit(
            f"ERROR: agent-sec-core release metadata does not match {expected}: "
            + ", ".join(mismatches)
        )
    return expected


def collect_python_hook_commands(
    document: object, location: str = "$"
) -> list[tuple[str, str]]:
    """Collect Python hook commands and native launchers in their arguments."""
    commands: list[tuple[str, str]] = []
    if isinstance(document, dict):
        command = document.get("command")
        args = document.get("args")
        has_python_arg = isinstance(args, list) and any(
            isinstance(arg, str) and ".py" in arg for arg in args
        )
        if isinstance(command, str) and (
            ".py" in command
            or has_python_arg
            or NATIVE_PYTHON_PATTERN.search(command) is not None
        ):
            commands.append((f"{location}.command", command))
        if isinstance(args, list):
            commands.extend(
                (f"{location}.args[{index}]", arg)
                for index, arg in enumerate(args)
                if isinstance(arg, str)
                and NATIVE_PYTHON_PATTERN.search(arg) is not None
            )
        for key, value in document.items():
            commands.extend(collect_python_hook_commands(value, f"{location}.{key}"))
    elif isinstance(document, list):
        for index, value in enumerate(document):
            commands.extend(collect_python_hook_commands(value, f"{location}[{index}]"))
    return commands


def verify_raw_contract(payload_root: Path) -> None:
    """Verify the staged raw contract does not require system Python."""
    contract_path = payload_root / ".anolisa" / "component.toml"
    contract = read_toml(contract_path)
    component = contract.get("component")
    dependencies = (
        component.get("dependencies") if isinstance(component, dict) else None
    )
    if not isinstance(dependencies, list):
        raise SystemExit(f"ERROR: {contract_path} has no component dependencies")
    if any(
        isinstance(dependency, dict) and dependency.get("name") == "python3"
        for dependency in dependencies
    ):
        raise SystemExit(f"ERROR: {contract_path} still requires system python3")


def verify_raw_hook_manifests(payload_root: Path) -> None:
    """Verify every staged Python hook uses the bundled runtime launcher."""
    adapter_root = payload_root / "adapters"
    manifest_paths = sorted(adapter_root.rglob("*.json"))
    expected_paths = {payload_root / relative for relative in RAW_HOOK_MANIFESTS}
    missing_paths = sorted(
        path for path in expected_paths if path not in manifest_paths
    )
    if missing_paths:
        raise SystemExit(
            "ERROR: raw payload is missing hook manifests: "
            + ", ".join(str(path) for path in missing_paths)
        )

    hook_counts = dict.fromkeys(expected_paths, 0)
    for manifest_path in manifest_paths:
        try:
            document = json.loads(manifest_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError as error:
            raise SystemExit(
                f"ERROR: invalid JSON in {manifest_path}: {error}"
            ) from error
        for location, command in collect_python_hook_commands(document):
            if NATIVE_PYTHON_PATTERN.search(command) is not None:
                raise SystemExit(
                    f"ERROR: {manifest_path} {location} bypasses agent-sec-python "
                    f"with native Python: {command!r}"
                )
            if command != "agent-sec-python" and not command.startswith(
                "agent-sec-python "
            ):
                raise SystemExit(
                    f"ERROR: {manifest_path} {location} bypasses agent-sec-python: "
                    f"{command!r}"
                )
            if manifest_path in hook_counts:
                hook_counts[manifest_path] += 1

    empty_manifests = sorted(path for path, count in hook_counts.items() if count == 0)
    if empty_manifests:
        raise SystemExit(
            "ERROR: raw hook manifests declare no Python hooks: "
            + ", ".join(str(path) for path in empty_manifests)
        )


def verify_raw_payload(payload_root: Path) -> None:
    """Verify raw-only contract and adapter launcher invariants."""
    verify_raw_contract(payload_root)
    verify_raw_hook_manifests(payload_root)


def parse_args() -> argparse.Namespace:
    """Parse command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument("source_root", type=Path)
    parser.add_argument("contract", type=Path)
    parser.add_argument("--payload-root", type=Path)
    return parser.parse_args()


def main() -> int:
    """Validate release metadata and print the canonical version."""
    args = parse_args()
    version = verify_versions(args.source_root.resolve(), args.contract.resolve())
    if args.payload_root is not None:
        verify_raw_payload(args.payload_root.resolve())
    print(version)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
