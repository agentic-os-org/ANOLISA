"""Options shared by the V2 pytest suites."""

import os
from pathlib import Path

import pytest


def pytest_collection_modifyitems(items):
    # These suites change host nftables, systemd units and system accounts, so they
    # only run where those changes are allowed.
    if os.environ.get("CI", "").lower() not in {"true", "1"}:
        return
    root = Path(__file__).parent
    excluded = {
        root / "systemd" / "test_gateway.py",
        root / "gateway" / "test_addon_blackbox.py",
    }
    for item in items:
        if item.path in excluded:
            item.add_marker(
                pytest.mark.skip(reason="gateway activation E2E is disabled in CI")
            )


def pytest_addoption(parser):
    parser.addoption(
        "--require-systemd",
        action="store_true",
        help="Fail instead of skipping system-manager tests when root/PID 1 systemd is absent",
    )
