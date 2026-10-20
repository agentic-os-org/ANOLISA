"""Shared target definitions for prebuilt release packages."""

from __future__ import annotations


TARGETS = {
    "agent-sec-core": (
        {"target-os": "linux", "target-arch": "x86_64", "profile": "gnu2.28-x86_64"},
    ),
    "os-skills": (
        {"target-os": "linux", "target-arch": "x86_64", "profile": "static-linux"},
        {"target-os": "linux", "target-arch": "aarch64", "profile": "static-linux"},
    ),
    "anolisa": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.17-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.17-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "aarch64",
            "profile": "darwin11-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "x86_64",
            "profile": "darwin11-x86_64",
        },
    ),
    "ws-ckpt": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.28-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.28-aarch64",
        },
    ),
    "skillfs": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.28-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.28-aarch64",
        },
    ),
    "cosh-ng": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.28-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.28-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "aarch64",
            "profile": "darwin11-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "x86_64",
            "profile": "darwin11-x86_64",
        },
    ),
    "agent-memory": (
        {"target-os": "linux", "target-arch": "x86_64", "profile": "gnu2.28-x86_64"},
        {"target-os": "linux", "target-arch": "aarch64", "profile": "gnu2.28-aarch64"},
    ),
    "agentsight": (
        {"target-os": "linux", "target-arch": "x86_64", "profile": "gnu2.28-x86_64"},
        {"target-os": "linux", "target-arch": "aarch64", "profile": "gnu2.28-aarch64"},
        {"target-os": "macos", "target-arch": "aarch64", "profile": "darwin11-aarch64"},
    ),
    "tokenless": (
        {
            "target-os": "linux",
            "target-arch": "x86_64",
            "profile": "gnu2.17-x86_64",
        },
        {
            "target-os": "linux",
            "target-arch": "aarch64",
            "profile": "gnu2.17-aarch64",
        },
        {
            "target-os": "macos",
            "target-arch": "aarch64",
            "profile": "darwin11-aarch64",
        },
    ),
}
