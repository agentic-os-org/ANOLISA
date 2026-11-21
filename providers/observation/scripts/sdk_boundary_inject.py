"""Inject a version-pinned LangChain tool-entry guard before loading a fixture."""

import argparse
from importlib.metadata import version
import json
import runpy
import sys
from typing import Any

PIN = "1.7.6"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--decision", choices=["allow", "block"], required=True)
    parser.add_argument("fixture")
    parser.add_argument("arguments", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if version("langchain-core") != PIN:
        raise RuntimeError("unsupported LangChain version for experimental injection")
    from langchain_core.tools import BaseTool

    original = BaseTool.run
    original_async = BaseTool.arun
    checks = []

    def check(tool: BaseTool) -> bool:
        checks.append({"tool": tool.name, "decision": args.decision})
        return args.decision == "allow"

    def guarded(tool: BaseTool, *positional: Any, **keywords: Any) -> Any:
        if not check(tool):
            return "blocked_by_experimental_entry_guard"
        return original(tool, *positional, **keywords)

    async def guarded_async(tool: BaseTool, *positional: Any, **keywords: Any) -> Any:
        if not check(tool):
            return "blocked_by_experimental_entry_guard"
        return await original_async(tool, *positional, **keywords)

    BaseTool.run = guarded
    BaseTool.arun = guarded_async
    sys.argv = [args.fixture, *args.arguments]
    try:
        runpy.run_path(args.fixture, run_name="__main__")
    finally:
        print(
            json.dumps({"runtime_injection": "langchain_base_tool", "checks": checks}),
            file=sys.stderr,
        )


if __name__ == "__main__":
    main()
