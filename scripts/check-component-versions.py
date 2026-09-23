#!/usr/bin/env python3
from __future__ import annotations
"""Fail when component metadata drifts from its authoritative version."""

import json
import re
import subprocess
import sys
from pathlib import Path

try:
    import tomllib
except ImportError:
    tomllib = None  # type: ignore[assignment]


ROOT = Path(__file__).resolve().parent.parent
VERSION_RE = re.compile(
    r'^\s*(?:"version"|version)\s*[:=]\s*"([^"]+)"\s*,?\s*$', re.MULTILINE
)
TOML_CONTRACTS = (
    ("src/agent-memory/Cargo.toml", "distribution/anolisa/manifests/components/agent-memory/component.toml"),
    ("src/agent-sec-core/openclaw-plugin/package.json", "src/agent-sec-core/.anolisa/component.toml"),
    ("src/agentsight/Cargo.toml", "src/agentsight/component.toml"),
    ("src/agentsight/Cargo.toml", "src/agentsight/.anolisa/component.toml"),
    ("src/agentsight/Cargo.toml", "src/agentsight/.anolisa/component.macos.toml"),
    ("deprecated/copilot-shell/package.json", "deprecated/copilot-shell/component.toml"),
    ("src/cosh-ng/Cargo.toml", "src/cosh-ng/component.toml"),
    ("src/cosh-ng/Cargo.toml", "src/cosh-ng/.anolisa/component.toml"),
    ("src/cosh-ng/Cargo.toml", "src/cosh-ng/.anolisa/component.macos.toml"),
    ("src/skillfs/Cargo.toml", "src/skillfs/component.toml"),
    ("src/tokenless/Cargo.toml", "distribution/anolisa/manifests/components/tokenless/component.toml"),
    ("src/ws-ckpt/src/Cargo.toml", "src/ws-ckpt/component.toml"),
    ("src/ws-ckpt/src/Cargo.toml", "distribution/anolisa/manifests/components/ws-ckpt/component.toml"),
)
VERSION_TEMPLATES = (
    ("src/agent-memory/Cargo.toml", "src/agent-memory/.anolisa/component.toml.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/.anolisa/component.toml.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/adapters/tokenless/manifest.json.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/adapters/tokenless/openclaw/package.json.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/adapters/tokenless/openclaw/openclaw.plugin.json.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/adapters/tokenless/hermes/plugin.yaml.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/adapters/tokenless/qoder/.qoder-plugin/plugin.json.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/adapters/tokenless/claude-code/.claude-plugin/plugin.json.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/adapters/tokenless/codex/.codex-plugin/plugin.json.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/adapters/tokenless/qwencode/qwen-extension.json.in"),
    ("src/tokenless/Cargo.toml", "src/tokenless/adapters/tokenless/qwenpaw/plugin.json.in"),
)
AGENT_MEMORY_JSON = (
    "src/agent-memory/adapters/agent-memory/manifest.json",
    "src/agent-memory/adapters/agent-memory/openclaw/package.json",
    "src/agent-memory/adapters/agent-memory/openclaw/openclaw.plugin.json",
    "src/agent-memory/config/mcp-server.json",
)
GENERATED_CONTRACTS = (
    "src/agent-memory/.anolisa/component.toml",
    "src/tokenless/.anolisa/component.toml",
)


def read_text(path: str) -> str:
    try:
        return (ROOT / path).read_text()
    except FileNotFoundError:
        raise ValueError(f"{path}: contract file not found in repository") from None


def read_toml_version(path: str) -> str:
    text = read_text(path)
    if tomllib is not None:
        try:
            data = tomllib.loads(text)
            version = data.get("version")
            if isinstance(version, str):
                return version
        except tomllib.TOMLDecodeError:
            pass
    match = VERSION_RE.search(text)
    if not match:
        raise ValueError(f"no version field in {path} (component: {path.rsplit('/', 1)[0]})")
    return match.group(1)


def read_json_version(path: str) -> str:
    version = json.loads(read_text(path)).get("version")
    if not isinstance(version, str):
        raise ValueError(f"no JSON version field in {path} (component: {path.rsplit('/', 1)[0]})")
    return version


def read_version(path: str) -> str:
    return read_json_version(path) if path.endswith(".json") else read_toml_version(path)


def check_equal(errors: list[str], source: str, target: str) -> None:
    expected = read_version(source)
    actual = read_version(target)
    if actual != expected:
        errors.append(f"{target}: expected {expected}, found {actual} (source: {source})")


def check_template(errors: list[str], source: str, template: str) -> None:
    expected = read_version(source)
    content = read_text(template)
    if content.count("@VERSION@") != 1:
        errors.append(f"{template}: expected exactly one @VERSION@ placeholder")
        return
    rendered = content.replace("@VERSION@", expected)
    match = VERSION_RE.search(rendered)
    if not match or match.group(1) != expected:
        errors.append(f"{template}: does not render component.version={expected}")


def check_agent_memory_lock(errors: list[str], expected: str) -> None:
    path = "src/agent-memory/adapters/agent-memory/openclaw/package-lock.json"
    lock = json.loads(read_text(path))
    root_version = lock.get("version")
    if root_version != expected:
        errors.append(f"{path}: expected root version {expected}, found {root_version}")
    packages_root = lock.get("packages", {}).get("")
    if packages_root is not None:
        pkg_version = packages_root.get("version")
        if pkg_version != expected:
            errors.append(f"{path}: expected packages root version {expected}, found {pkg_version}")


def check_generated_contracts_untracked(errors: list[str]) -> None:
    if not (ROOT / ".git").exists():
        print("warning: skipping untracked-contracts check (no .git found)", file=sys.stderr)
        return
    for path in GENERATED_CONTRACTS:
        try:
            result = subprocess.run(
                ["git", "ls-files", "--error-unmatch", path],
                cwd=ROOT,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                check=False,
            )
        except OSError:
            print("warning: skipping untracked-contracts check (git not available)", file=sys.stderr)
            return
        if result.returncode == 0:
            errors.append(f"{path}: generated component contract must not be tracked")


def main() -> int:
    errors: list[str] = []
    try:
        for source, target in TOML_CONTRACTS:
            check_equal(errors, source, target)
        for source, template in VERSION_TEMPLATES:
            check_template(errors, source, template)

        agent_memory_version = read_toml_version("src/agent-memory/Cargo.toml")
        for path in AGENT_MEMORY_JSON:
            actual = read_json_version(path)
            if actual != agent_memory_version:
                errors.append(f"{path}: expected {agent_memory_version}, found {actual}")
        check_agent_memory_lock(errors, agent_memory_version)
        check_generated_contracts_untracked(errors)
    except (OSError, ValueError, json.JSONDecodeError) as error:
        errors.append(str(error))

    if errors:
        print("Component version check failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1

    print("Component version metadata is synchronized.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
