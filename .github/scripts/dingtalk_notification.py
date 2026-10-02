#!/usr/bin/env python3
"""Build safe DingTalk issue messages and validate webhook responses."""

import json
import re
import sys
from typing import Any


MARKDOWN_META_RE = re.compile(r"([\\`*_!\[\]\(\)#<>])")


def escape_markdown(value: str) -> str:
    """Escape user-controlled text for a single DingTalk Markdown line."""
    single_line = re.sub(r"[\r\n]+", " ", value)
    return MARKDOWN_META_RE.sub(r"\\\1", single_line)


def build_issue_markdown(
    *,
    issue_number: str,
    issue_title: str,
    issue_url: str,
    component_label: str,
    author: str,
    owners: str,
    assigned: str,
) -> str:
    """Build a DingTalk message whose Issue link cannot be changed by its title."""
    safe_title = escape_markdown(issue_title)
    return (
        "## 🔔 ANOLISA Issue Triage\n\n"
        f"**Issue**: [#{issue_number}]({issue_url})\n\n"
        f"**Title**: {safe_title}\n\n"
        f"**Component**: `{component_label}`\n\n"
        f"**Author**: {author}\n\n"
        f"**Owners notified**: {owners}\n\n"
        f"**Assigned**: {assigned}\n\n"
        "> 🤖 ANOLISA Issue Bot"
    )


def validate_response(raw_response: str) -> None:
    """Require a JSON object whose DingTalk errcode is zero."""
    try:
        response: Any = json.loads(raw_response)
    except json.JSONDecodeError as error:
        raise ValueError("DingTalk response was not valid JSON") from error

    if not isinstance(response, dict):
        raise ValueError("DingTalk response must be a JSON object")
    if response.get("errcode") != 0:
        errcode = response.get("errcode", "missing")
        errmsg = response.get("errmsg", "missing")
        raise ValueError(
            f"DingTalk notification failed: errcode={errcode}, errmsg={errmsg}"
        )


def main() -> int:
    """Validate the response supplied on standard input."""
    try:
        validate_response(sys.stdin.read())
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
