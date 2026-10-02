#!/usr/bin/env python3
"""Regenerate the hook parity goldens under tests/contract/goldens/.

Runs every fixture x agent case through the common hooks against the real
`tokenless` debug binary and records the envelope each hook emits on stdout.
The committed goldens define the current Protocol v2 Common Hook behavior.
``tests/test_hook_parity.py`` replays the same corpus through the current
hooks and diffs against them, so regenerate only when an intentional behavior
change is being re-baselined.
"""

from __future__ import annotations

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import corpus


def write_golden(kind: str, name: str, agent: str, proc) -> None:
    envelope = json.loads(proc.stdout)
    path = corpus.golden_path(kind, name, agent)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        json.dump({"envelope": envelope}, f, ensure_ascii=False, indent=2, sort_keys=True)
        f.write("\n")
    print(f"{kind}/{name}/{agent}: ok")


def main() -> int:
    if not os.path.exists(corpus.DEBUG_TOKENLESS_BIN):
        print(f"missing binary: {corpus.DEBUG_TOKENLESS_BIN} (run `cargo build -p tokenless-cli`)")
        return 1
    for kind, hook, agents in [
        ("post_tool", corpus.RESPONSE_HOOK, corpus.RESPONSE_AGENTS),
        ("before_model", corpus.SCHEMA_HOOK, corpus.SCHEMA_AGENTS),
    ]:
        for name in corpus.fixture_names(kind):
            with open(corpus.fixture_path(kind, name)) as f:
                stdin_text = f.read()
            for agent, env in corpus.agents_for(kind, name, agents).items():
                proc = corpus.run_hook(hook, stdin_text, env)
                if proc.returncode != 0:
                    print(f"{kind}/{name}/{agent}: hook exited {proc.returncode}: {proc.stderr}")
                    return 1
                write_golden(kind, name, agent, proc)
    return 0


if __name__ == "__main__":
    sys.exit(main())
