"""Freeze benchmark inputs and collect reproducibility metadata."""

from __future__ import annotations

import hashlib
import json
import platform
import subprocess
import time
from pathlib import Path
from typing import Any


def read_json(path: Path) -> dict[str, Any]:
    """Read one JSON object."""
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise TypeError(f"expected a JSON object: {path}")
    return value


def sha256(path: Path) -> str | None:
    """Return a file digest, preserving missing artifacts as missing."""
    if not path.is_file():
        return None
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def observes_mock_server(config_path: Path) -> bool:
    """Return whether AgentSight will attach to the Python/OpenSSL mock server."""
    if not config_path.is_file():
        return False
    config = read_json(config_path)
    allow = config.get("cmdline", {}).get("allow", [])
    return any(
        "mock_llm_server.py" in str(pattern)
        for entry in allow
        if isinstance(entry, dict)
        for pattern in entry.get("rule", [])
    )


def command_version(command: list[str]) -> str | None:
    """Capture the first version line without failing environment collection."""
    try:
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=5, check=False
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    text = (result.stdout or result.stderr).strip()
    return text.splitlines()[0] if text else None


def command_output(command: list[str]) -> str | None:
    """Capture complete command output for structured host metadata."""
    try:
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=5, check=False
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    text = (result.stdout or result.stderr).strip()
    return text or None


def frozen_inputs(campaign_path: Path, campaign: dict[str, Any]) -> dict[str, Any]:
    """Build the immutable campaign identity used to detect configuration drift."""
    versions: dict[str, Any] = {}
    for name, version in campaign["versions"].items():
        binary = Path(version["binary"])
        config = Path(version["config"])
        if not binary.is_file() or not config.is_file():
            raise FileNotFoundError(f"{name} binary/config must exist before freeze")
        if not observes_mock_server(config):
            raise ValueError(
                f"{name} config must discover *mock_llm_server.py* for OpenSSL capture"
            )
        versions[name] = {
            "commit": version.get("commit"),
            "binary": str(binary.resolve()),
            "binary_sha256": sha256(binary),
            "binary_capabilities": command_version(["getcap", str(binary)]),
            "config": str(config.resolve()),
            "config_sha256": sha256(config),
            "source_config": (
                str(Path(version["source_config"]).resolve())
                if version.get("source_config")
                else None
            ),
            "source_config_sha256": (
                sha256(Path(version["source_config"]))
                if version.get("source_config")
                else None
            ),
            "db": str(Path(version["db"]).resolve()),
            "metrics_file": (
                str(Path(version["metrics_file"]).resolve())
                if version.get("metrics_file")
                else None
            ),
            "log_file": (
                str(Path(version["log_file"]).resolve())
                if version.get("log_file")
                else None
            ),
        }
    return {
        "campaign": str(campaign_path.resolve()),
        "campaign_sha256": sha256(campaign_path),
        "comparison_mode": campaign.get("comparison_mode", "ab_comparison"),
        "versions": versions,
        "thresholds": campaign["thresholds"],
    }


def environment_manifest(
    campaign_path: Path, campaign: dict[str, Any]
) -> dict[str, Any]:
    """Collect host, tool, binary, and frozen-threshold evidence."""
    governor_path = Path("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
    mem_total = next(
        (
            line.split(":", 1)[1].strip()
            for line in Path("/proc/meminfo").read_text().splitlines()
            if line.startswith("MemTotal:")
        ),
        None,
    )
    return {
        "schema_version": 1,
        "collected_at_unix": time.time(),
        "frozen": frozen_inputs(campaign_path, campaign),
        "host": {
            "node": platform.node(),
            "machine": platform.machine(),
            "kernel": platform.release(),
            "platform": platform.platform(),
            "cpu": command_output(["lscpu", "--json"]),
            "memory_total": mem_total,
            "cpu_governor": (
                governor_path.read_text().strip() if governor_path.exists() else None
            ),
            "cgroup": Path("/proc/self/cgroup").read_text().strip(),
            "btf_available": Path("/sys/kernel/btf/vmlinux").is_file(),
        },
        "tools": {
            "python": platform.python_version(),
            "cargo": command_version(["cargo", "--version"]),
            "k6": command_version(["k6", "version"]),
            "h2load": command_version(["h2load", "--version"]),
            "openssl": command_version(["openssl", "version"]),
        },
    }


def ensure_manifest(
    results_root: Path, campaign_path: Path, campaign: dict[str, Any]
) -> None:
    """Create the manifest once and reject later binary/config drift."""
    results_root.mkdir(parents=True, exist_ok=True)
    path = results_root / "manifest.json"
    current = environment_manifest(campaign_path, campaign)
    if path.exists():
        existing = read_json(path)
        if existing.get("frozen") != current["frozen"]:
            raise ValueError(
                "campaign, binary, config, or thresholds changed after freeze"
            )
        return
    path.write_text(
        json.dumps(current, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
