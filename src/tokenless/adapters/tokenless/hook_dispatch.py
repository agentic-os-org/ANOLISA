"""Select host-owned hook adapters for the historical command entrypoints."""

from __future__ import annotations

import importlib.util
import os
import sys
from pathlib import Path
from types import ModuleType

_ADAPTERS = {
    "claude-code": "claude-code",
    "qoder-cli": "qoder",
    "qwenwork": "qwenwork",
    "opencode": "opencode",
    "cosh-ng": "cosh-ng",
    "copilot-shell": "copilot-shell",
    "codex": "codex",
    "qwencode": "qwencode",
    "workbuddy": "workbuddy",
}


def load_adapter(agent_id: str) -> ModuleType:
    directory = _ADAPTERS.get(agent_id, "unknown")
    spec = importlib.util.spec_from_file_location(
        "tokenless_hook_" + directory.replace("-", "_"),
        Path(__file__).parent / directory / "hook_adapter.py",
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def _agent_id_from_argv(argv: list[str] | None) -> str | None:
    """Return an explicit ``--agent-id`` value from hook command arguments.

    Adapters declare the agent id in ``hooks.json``. The host always honours
    the hook ``command`` string, but does not reliably propagate the declared
    ``env`` map to every hook execution context — PreToolUse rewrite and
    sub-agent task runs have been observed dropping it, collapsing attribution
    to the fallback. An id passed as a command argument is therefore the robust
    channel. Accepts both ``--agent-id VALUE`` and ``--agent-id=VALUE``;
    unrelated arguments are ignored so hooks that take no other options are
    unaffected.
    """
    args = sys.argv[1:] if argv is None else argv
    for index, arg in enumerate(args):
        if arg == "--agent-id":
            following = args[index + 1] if index + 1 < len(args) else ""
            return following or None
        if arg.startswith("--agent-id="):
            return arg[len("--agent-id=") :] or None
    return None


def resolve_agent_id(default: str = "unknown", argv: list[str] | None = None) -> str:
    """Resolve the agent ID used for stats attribution.

    Precedence, highest first:

    1. **Cosh-NG runtime detection.** The shared extension manifest sets
       ``TOKENLESS_AGENT_ID=copilot-shell``, and since Cosh-NG honours hook
       ``env`` (#1617) an unconditional read would attribute Cosh-NG sessions
       to copilot-shell. ``COSH_RUNTIME`` / ``COSH_NG_VERSION`` are injected by
       the host after the manifest env, so they win.
    2. **``--agent-id`` command argument** — the robust declared channel, since
       the host always honours the hook ``command`` string.
    3. **``TOKENLESS_AGENT_ID`` environment variable** — the legacy declared
       channel, kept for backward compatibility but not always propagated.
    4. **``default``** — a visible ``"unknown"`` sentinel rather than the tool's
       own name, so an attribution gap is observable in stats instead of
       masquerading as a real ``tokenless`` agent.

    Host selection is cooperative, not an authorization boundary. A manifest
    controls its own command and environment, so identity cannot grant access.
    """
    if os.environ.get("COSH_NG_VERSION") or os.environ.get("COSH_RUNTIME") == "cosh-ng":
        return "cosh-ng"
    return _agent_id_from_argv(argv) or os.environ.get("TOKENLESS_AGENT_ID") or default
