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
MAX_RESPONSE_BYTES = 65536


class DeliveryRejected(ValueError):
    """DingTalk explicitly rejected the request before accepting a delivery."""


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Refuse redirects so the signed webhook URL cannot leak elsewhere."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(
            req.full_url, code, "redirect not allowed", headers, fp
        )


# No proxies: the environment must not see the access_token / sign in the URL.
_OPENER = urllib.request.build_opener(
    urllib.request.ProxyHandler({}), _NoRedirect()
)


def escape_markdown(value: str) -> str:
    """Escape user-controlled text for a single DingTalk Markdown line."""
    single_line = re.sub(r"[\r\n]+", " ", value)
    return MARKDOWN_META_RE.sub(r"\\\1", single_line)


def _safe_url(url: str) -> str:
    """Validate that a URL used inside a Markdown link is http(s)."""
    parts = urllib.parse.urlsplit(url)
    if parts.scheme not in ("http", "https"):
        raise ValueError("link URL must be http(s)")
    return urllib.parse.urlunsplit(parts)


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
    safe_number = escape_markdown(str(issue_number))
    safe_title = escape_markdown(issue_title)
    safe_url = _safe_url(issue_url)
    return (
        "## 🔔 ANOLISA Issue Triage\n\n"
        f"**Issue**: [#{safe_number}]({safe_url})\n\n"
        f"**Title**: {safe_title}\n\n"
        f"**Component**: {escape_markdown(component_label)}\n\n"
        f"**Author**: {escape_markdown(author)}\n\n"
        f"**Owners notified**: {escape_markdown(owners)}\n\n"
        f"**Assigned**: {escape_markdown(assigned)}\n\n"
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
        raise DeliveryRejected(
            f"DingTalk notification failed: errcode={errcode}, errmsg={errmsg}"
        )


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
    heading = (
        "ANOLISA Issue — needs review" if needs_human else "ANOLISA Issue Analysis"
    )
    fields = {
        "Title": issue_title,
        "Component": component_label,
        "Priority": priority,
        "Conclusion": conclusion,
        "Impact": impact,
        "Next step": next_step,
        "Triagers": owners,
    }
    safe_number = escape_markdown(str(issue_number))
    lines = [
        f"## {heading}",
        f"**Issue**: [#{safe_number}]({_safe_url(issue_url)})",
    ]
    lines.extend(
        f"**{label}**: {escape_markdown(value[:240]).replace('@', '＠')}"
        for label, value in fields.items()
    )
    lines.extend(
        [
            f"[Report]({_safe_url(report_url)})",
            "Source analysis; runtime checks are listed in the verification plan.",
        ]
    )
    return "\n\n".join(lines)


def _read_bounded(stream) -> bytes:
    """Read at most MAX_RESPONSE_BYTES + 1 bytes, so oversize is detectable."""
    return stream.read(MAX_RESPONSE_BYTES + 1)


def send_markdown(*, webhook: str, secret: str, title: str, text: str) -> None:
    """Send one signed webhook request with a bounded network timeout."""
    parts = urllib.parse.urlsplit(webhook)
    if parts.scheme not in ("http", "https"):
        raise ValueError("DINGTALK_WEBHOOK must be http(s)")

    query = urllib.parse.parse_qsl(parts.query, keep_blank_values=True)
    if secret:
        timestamp = str(round(time.time() * 1000))
        signature = hmac.new(
            secret.encode(), (timestamp + "\n" + secret).encode(), hashlib.sha256
        ).digest()
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
        with _OPENER.open(request, timeout=30) as response:
            raw = _read_bounded(response)
    except urllib.error.HTTPError as error:
        # A non-2xx response may still carry a DingTalk JSON body.
        try:
            body = _read_bounded(error)
        except OSError:
            body = b""
        if body and len(body) <= MAX_RESPONSE_BYTES:
            try:
                validate_response(body.decode())
            except DeliveryRejected:
                raise
            except (ValueError, UnicodeDecodeError):
                pass
        raise RuntimeError("DingTalk delivery outcome is unknown") from error
    except OSError as error:
        raise RuntimeError("DingTalk delivery outcome is unknown") from error

    if len(raw) > MAX_RESPONSE_BYTES:
        raise RuntimeError(
            "DingTalk response exceeded the size limit; outcome is unknown"
        )
    try:
        decoded = raw.decode()
    except UnicodeDecodeError as error:
        raise RuntimeError(
            "DingTalk response was not valid UTF-8; outcome is unknown"
        ) from error
    validate_response(decoded)


def send_analysis() -> int:
    """Emit a delivery classification without logging webhook credentials."""
    try:
        data = json.load(sys.stdin)
        if not isinstance(data, dict):
            raise TypeError("analysis input must be a JSON object")
        data["issue_number"] = int(data["issue_number"])
        send_markdown(
            webhook=os.environ["DINGTALK_WEBHOOK"],
            secret=os.environ.get("DINGTALK_SECRET", ""),
            title=f"Issue Analysis #{data['issue_number']}",
            text=build_analysis_markdown(**data),
        )
    except DeliveryRejected as error:
        print(json.dumps({"status": "rejected", "detail": str(error)}))
        return 1
    except (KeyError, json.JSONDecodeError, TypeError, ValueError) as error:
        print(
            json.dumps({"status": "invalid_input", "detail": str(error)}),
            file=sys.stderr,
        )
        return 2
    except RuntimeError as error:
        print(json.dumps({"status": "unknown", "detail": str(error)}), file=sys.stderr)
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
    if len(sys.argv) == 2 and sys.argv[1] == "send-analysis":
        exit_code = send_analysis()
    elif len(sys.argv) == 1:
        exit_code = main()
    else:
        print("usage: script.py [send-analysis]", file=sys.stderr)
        exit_code = 2
    raise SystemExit(exit_code)
