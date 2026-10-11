"""Shared readiness check for the guide's existing isolated Python environment."""

import subprocess
from pathlib import Path


def dependencies_ready(interpreter: Path) -> bool:
    """Probe imports without creating an environment or installing packages."""
    if not interpreter.exists():
        return False
    try:
        result = subprocess.run(
            [str(interpreter), "-c", "import requests, bs4, markdownify"],
            capture_output=True,
            timeout=5,
        )
        return result.returncode == 0
    except Exception:
        return False
