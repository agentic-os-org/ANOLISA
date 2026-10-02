"""Shared corpus and hook runner for the adapter contract and parity suites.

Defines the fixture x agent matrix for the common-hooks family and a hermetic
hook runner: every invocation gets a fresh HOME (stash/stats state, version
caches) and a private bin directory containing the `tokenless` binary under
test plus a fake `claude` whose version satisfies the replacement gate.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile

CONTRACT_DIR = os.path.dirname(os.path.abspath(__file__))
REPO_ROOT = os.path.dirname(os.path.dirname(CONTRACT_DIR))
HOOKS_DIR = os.path.join(REPO_ROOT, "adapters", "tokenless", "common", "hooks")
FIXTURES_DIR = os.path.join(CONTRACT_DIR, "fixtures")
GOLDENS_DIR = os.path.join(CONTRACT_DIR, "goldens")

RESPONSE_HOOK = os.path.join(HOOKS_DIR, "compress_response_hook.py")
SCHEMA_HOOK = os.path.join(HOOKS_DIR, "compress_schema_hook.py")
PRE_TOOL_HOOK = os.path.join(HOOKS_DIR, "rewrite_hook.py")

DEBUG_TOKENLESS_BIN = os.path.join(REPO_ROOT, "target", "debug", "tokenless")

# Claude Code >= 2.1.121 supports updatedToolOutput; the fake binary reports
# a version above the gate so replacement-path goldens are exercised.
FAKE_CLAUDE_VERSION = "2.1.130"

# Agent key -> env vars, mirroring each manifest's declaration channel
# (hooks.json / extension manifests set TOKENLESS_AGENT_ID; Cosh-NG is
# detected through its own runtime env vars, which win over the manifest).
RESPONSE_AGENTS = {
    "claude-code": {"TOKENLESS_AGENT_ID": "claude-code"},
    "qoder-cli": {"TOKENLESS_AGENT_ID": "qoder-cli"},
    "opencode": {"TOKENLESS_AGENT_ID": "opencode"},
    "qwencode": {"TOKENLESS_AGENT_ID": "qwencode"},
    "cosh-ng": {"COSH_NG_VERSION": "0.5.0"},
}
SCHEMA_AGENTS = {
    "qwencode": {"TOKENLESS_AGENT_ID": "qwencode"},
    "cosh-ng": {"COSH_NG_VERSION": "0.5.0"},
}


# Fixtures modeling one host's private wire contract run only against that
# host's agent: the llmContent wrapper is Cosh-NG's shape, and feeding it to
# other agents exercises combinations no real host produces.
FIXTURE_AGENTS = {
    ("post_tool", "cosh_wrapper"): ["cosh-ng"],
}

def agents_for(kind: str, name: str, agents: dict) -> dict:
    """The agent matrix for one fixture, honoring FIXTURE_AGENTS."""
    allowed = FIXTURE_AGENTS.get((kind, name))
    if allowed is None:
        return agents
    return {agent: env for agent, env in agents.items() if agent in allowed}


def fixture_names(kind: str) -> list[str]:
    """Sorted fixture basenames (without .json) for `post_tool`/`before_model`."""
    directory = os.path.join(FIXTURES_DIR, kind)
    return sorted(
        name[: -len(".json")] for name in os.listdir(directory) if name.endswith(".json")
    )


def fixture_path(kind: str, name: str) -> str:
    return os.path.join(FIXTURES_DIR, kind, name + ".json")


def golden_path(kind: str, name: str, agent: str) -> str:
    return os.path.join(GOLDENS_DIR, kind, name, agent + ".json")


def run_hook(
    hook_script: str,
    stdin_text: str,
    agent_env: dict,
    tokenless_bin: str | None = DEBUG_TOKENLESS_BIN,
    extra_env: dict | None = None,
    timeout: int = 60,
    tokenless_on_path: bool = True,
) -> subprocess.CompletedProcess:
    """Run one hook hermetically and return the completed process.

    `tokenless_bin=None` leaves the bin directory without a `tokenless`
    entry (the binary-missing contract case). `extra_env` overrides after
    the standard environment is assembled.
    """
    with tempfile.TemporaryDirectory() as tmp:
        home = os.path.join(tmp, "home")
        bindir = os.path.join(tmp, "bin")
        os.makedirs(home)
        os.makedirs(bindir)
        if tokenless_bin:
            if tokenless_on_path:
                os.symlink(tokenless_bin, os.path.join(bindir, "tokenless"))
            else:
                # The first supported off-PATH layout must win before any
                # host-level FHS installation can influence this fixture.
                fallback_dir = os.path.join(home, ".local", "bin")
                os.makedirs(fallback_dir)
                os.symlink(tokenless_bin, os.path.join(fallback_dir, "tokenless"))
        claude = os.path.join(bindir, "claude")
        with open(claude, "w") as f:
            f.write(f'#!/bin/sh\necho "{FAKE_CLAUDE_VERSION} (Claude Code)"\n')
        os.chmod(claude, 0o755)
        os.symlink(sys.executable, os.path.join(bindir, "python3"))

        env = {
            "HOME": home,
            "PATH": bindir,
            "LC_ALL": "C.UTF-8",
            "TOKENLESS_DATA_DIR": os.path.join(home, ".tokenless"),
            # Keep runs hermetic: no stats/SLS side channels; compression on.
            "TOKENLESS_STATS_ENABLED": "0",
            "TOKENLESS_SLS_ENABLED": "0",
        }
        env.update(agent_env)
        if extra_env:
            env.update(extra_env)
        return subprocess.run(
            [sys.executable, hook_script],
            input=stdin_text,
            capture_output=True,
            text=True,
            env=env,
            timeout=timeout,
        )
