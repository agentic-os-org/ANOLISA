"""Demonstrate a shared boolean policy, without executing any tool input."""

import json
import sys


def main() -> None:
    event = json.load(sys.stdin)
    matched = event["name"] == "tool.after" or "AW_DENY_TEST" in json.dumps(
        event["tool"]["input"], ensure_ascii=False
    )
    print(json.dumps(matched))


if __name__ == "__main__":
    main()
