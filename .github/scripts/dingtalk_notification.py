#!/usr/bin/env python3
"""Build safe DingTalk issue messages and validate webhook responses."""

import base64
import hashlib
import hmac
import json
import os
import re
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from typing import Any

MARKDOWN_META_RE = re.compile(r"([\\`*_!\[\]\(\)#<>])")


class DeliveryRejected(ValueError):
    """DingTalk explicitly rejected the request before accepting a delivery."""


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
    if type(response.get("errcode")) is not int:
        raise ValueError("DingTalk response must contain a numeric errcode")
    if response.get("errcode") != 0:
        errcode = response.get("errcode", "missing")
        errmsg = response.get("errmsg", "missing")
        raise DeliveryRejected(f"DingTalk notification failed: errcode={errcode}, errmsg={errmsg}")


def build_analysis_markdown(
    *,
    issue_number: int,
    issue_title: str,
    issue_url: str,
    component_label: str,
    priority: str,
    conclusion: str,
    impact: str,
    next_step: str,
    owners: str,
    report_url: str,
    needs_human: bool,
) -> str:
    """Render a bounded analysis card from the controller's structured result."""
    heading = "ANOLISA Issue — needs review" if needs_human else "ANOLISA Issue Analysis"
    fields = {
        "Title": issue_title,
        "Component": component_label,
        "Priority": priority,
        "Conclusion": conclusion,
        "Impact": impact,
        "Next step": next_step,
        "Triagers": owners,
    }
    lines = [f"## {heading}", f"**Issue**: [#{issue_number}]({issue_url})"]
    lines.extend(
        f"**{label}**: {escape_markdown(value[:240]).replace('@', '＠')}"
        for label, value in fields.items()
    )
    lines.extend(
        [
            f"[Report]({report_url})",
            "Source analysis; runtime checks are listed in the verification plan.",
        ]
    )
    return "\n\n".join(lines)


def send_markdown(*, webhook: str, secret: str, title: str, text: str) -> None:
    """Send one signed webhook request with a bounded network timeout."""
    timestamp = str(round(time.time() * 1000))
    signature = hmac.new(
        secret.encode(), (timestamp + "\n" + secret).encode(), hashlib.sha256
    ).digest()
    parts = urllib.parse.urlsplit(webhook)
    query = urllib.parse.parse_qsl(parts.query, keep_blank_values=True)
    query.extend(
        [
            ("timestamp", timestamp),
            ("sign", base64.b64encode(signature).decode()),
        ]
    )
    url = urllib.parse.urlunsplit(parts._replace(query=urllib.parse.urlencode(query)))
    payload = json.dumps(
        {
            "msgtype": "markdown",
            "markdown": {"title": title, "text": text},
        }
    ).encode()
    request = urllib.request.Request(
        url, data=payload, headers={"Content-Type": "application/json"}, method="POST"
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            raw = response.read(65537)
    except urllib.error.HTTPError as error:
        raise RuntimeError("DingTalk delivery outcome is unknown") from error
    except (OSError, urllib.error.URLError) as error:
        raise RuntimeError("DingTalk delivery outcome is unknown") from error
    if len(raw) > 65536:
        raise RuntimeError("DingTalk response exceeded the size limit; outcome is unknown")
    try:
        validate_response(raw.decode())
    except DeliveryRejected as error:
        raise DeliveryRejected("DingTalk rejected notification") from error


def send_analysis() -> int:
    """Emit a delivery classification without logging webhook credentials."""
    try:
        data = json.load(sys.stdin)
        send_markdown(
            webhook=os.environ["DINGTALK_WEBHOOK"],
            secret=os.environ.get("DINGTALK_SECRET", ""),
            title=f"Issue Analysis #{data['issue_number']}",
            text=build_analysis_markdown(**data),
        )
    except DeliveryRejected:
        print(json.dumps({"status": "rejected"}))
        return 1
    except Exception:
        print(json.dumps({"status": "unknown"}))
        return 1
    print(json.dumps({"status": "delivered"}))
    return 0


def main() -> int:
    """Validate the response supplied on standard input."""
    try:
        validate_response(sys.stdin.read())
    except ValueError as error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(send_analysis() if sys.argv[1:] == ["send-analysis"] else main())
