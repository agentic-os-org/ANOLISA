#!/usr/bin/env python3

# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""MCP server bridging sandbox tools (Bash, Read, Write, etc.) to a Docker container.

Exposes sandbox tools as MCP tools and routes each call to the container's
HTTP API. Used by openclaw agent to interact with sandbox containers.

Usage:
    python3 mcp_sandbox_tools.py \
        --sandbox-url http://localhost:32778 \
        --task-yaml /path/to/task.yaml
"""

from __future__ import annotations

import argparse
import json
import math
import sys

from mcp.types import ImageContent, TextContent

try:
    import httpx
except ImportError:
    print("httpx required: pip install httpx", file=sys.stderr)
    sys.exit(1)


# Sandbox tool name -> container HTTP path
_PATH_MAP = {
    "Bash": "/exec",
    "Read": "/read",
    "Write": "/write",
    "Edit": "/edit",
    "Glob": "/glob",
    "Grep": "/grep",
    "BrowserScreenshot": "/screenshot",
    "ReadMedia": "/read_media",
    "Download": "/download",
}


def _translate_payload(tool_name: str, params: dict) -> dict:
    """Translate MCP tool parameters to container HTTP API parameters."""
    payload = dict(params)
    if tool_name == "Bash":
        if "timeout" in payload:
            payload["timeout_seconds"] = max(1, payload.pop("timeout") // 1000)
        payload.pop("description", None)
        payload.pop("run_in_background", None)
    elif tool_name in ("Read", "Write", "Edit", "ReadMedia", "Download"):
        if "file_path" in payload:
            payload["path"] = payload.pop("file_path")
    elif tool_name == "Grep":
        # Pass through as-is; container API accepts same param names
        pass
    return payload


_TRANSIENT_ERRORS = (
    httpx.ConnectError,
    httpx.ConnectTimeout,
    httpx.RemoteProtocolError,
    ConnectionRefusedError,
    ConnectionResetError,
)

_MAX_RETRIES = 2
_BACKOFF_SECONDS = (1, 2)


def _call_container(sandbox_url: str, tool_name: str, params: dict) -> str:
    """Call the container HTTP API and return JSON response.

    Retries up to _MAX_RETRIES times on transient connection errors
    (e.g. container being replaced on same port) with exponential backoff.
    """
    import time as _time

    path = _PATH_MAP.get(tool_name)
    if not path:
        return json.dumps({"error": f"Unknown sandbox tool: {tool_name}"})

    endpoint = f"{sandbox_url}{path}"
    payload = _translate_payload(tool_name, params)

    last_error: Exception | None = None
    for attempt in range(_MAX_RETRIES + 1):
        try:
            with httpx.Client(timeout=120.0) as client:
                resp = client.post(endpoint, json=payload)
                body = resp.json()
                return json.dumps(body, ensure_ascii=False, indent=2)
        except _TRANSIENT_ERRORS as e:
            last_error = e
            if attempt < _MAX_RETRIES:
                wait = _BACKOFF_SECONDS[attempt]
                print(
                    f"[mcp-sandbox] transient error on {tool_name} "
                    f"(attempt {attempt + 1}/{_MAX_RETRIES + 1}): {e}; "
                    f"retrying in {wait}s",
                    file=sys.stderr,
                )
                _time.sleep(wait)
        except Exception as e:
            return json.dumps({"error": f"Container call failed: {e}"})

    return json.dumps({"error": f"Container call failed after retries: {last_error}"})


def _build_media_result(raw: str) -> list | None:
    """Parse a container JSON response and split it into ImageContent + TextContent.

    Returns ``None`` if the JSON cannot be parsed, indicates an error, or has no
    extractable image frames -- caller should fall back to plain text in that case.

    For successful media responses we:
      - emit one MCP ``ImageContent`` per ``frames[*].image_b64`` (using the frame's
        ``mime_type`` if provided, else ``image/png``);
      - strip ``image_b64`` from the text summary so we only keep light metadata
        (index, timestamp, mime_type, frame_count, ...).
    """
    try:
        data = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return None

    if not isinstance(data, dict):
        return None
    # Treat explicit error responses as plain text so the model still sees the cause.
    if data.get("error") or data.get("status") == "error":
        return None

    frames = data.get("frames")
    if not isinstance(frames, list) or not frames:
        return None

    image_blocks: list = []
    summary_frames: list = []
    for frame in frames:
        if not isinstance(frame, dict):
            continue
        b64 = frame.get("image_b64")
        if b64:
            mime = frame.get("mime_type") or "image/png"
            image_blocks.append(ImageContent(type="image", data=b64, mimeType=mime))
        summary_frames.append({k: v for k, v in frame.items() if k != "image_b64"})

    if not image_blocks:
        return None

    summary = {k: v for k, v in data.items() if k != "frames"}
    summary["frame_count"] = len(frames)
    summary["frames"] = summary_frames

    text_block = TextContent(
        type="text",
        text=json.dumps(summary, ensure_ascii=False, indent=2),
    )
    return [*image_blocks, text_block]


# Safety cap to avoid token blow-up when auto-aligning the sampling window.
_MAX_SAFE_FRAMES = 120


def _align_max_frames(max_frames: int, fps: float, start_time: float,
                      end_time: float | None) -> int:
    """Raise ``max_frames`` so sampling spreads across the requested window.

    Only applies when ``end_time`` is given and the requested window needs more
    frames than ``max_frames`` to be covered at ``fps``. The result is capped at
    ``_MAX_SAFE_FRAMES`` to prevent token blow-up.
    """
    if end_time is None or fps <= 0:
        return max_frames
    span = end_time - start_time
    if span <= 0:
        return max_frames
    needed = math.ceil(span * fps)
    if needed <= max_frames:
        return max_frames
    return min(needed, _MAX_SAFE_FRAMES)


def _coverage_warning(raw: str, requested_end_time: float | None,
                      max_frames: int, fps: float) -> str | None:
    """Return a truncation warning when frames don't reach ``requested_end_time``.

    Detects the "frame window silently truncated" case: when the caller asked for
    a window up to ``requested_end_time`` but the container stopped early after
    hitting ``max_frames``. Returns ``None`` when no truncation is detected or the
    response can't be inspected.
    """
    if requested_end_time is None:
        return None
    try:
        data = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return None
    if not isinstance(data, dict):
        return None

    frames = data.get("frames")
    if not isinstance(frames, list) or not frames:
        return None

    timestamps = [
        f.get("timestamp_s") for f in frames
        if isinstance(f, dict) and isinstance(f.get("timestamp_s"), (int, float))
    ]
    if not timestamps:
        return None

    last_ts = max(timestamps)
    tol = (1.0 / fps) if fps > 0 else 1.5
    if last_ts >= requested_end_time - tol:
        return None

    return (
        f"WARNING: max_frames ({max_frames}) limit reached. "
        f"Actual coverage 0.0-{last_ts}s, but you requested up to "
        f"{requested_end_time}s. Increase max_frames or narrow the time window "
        f"to cover the full range."
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--sandbox-url", required=True,
                        help="Sandbox container HTTP URL (e.g. http://localhost:32778)")
    parser.add_argument("--task-yaml", default=None,
                        help="Path to task.yaml (for context in MCP server name)")
    args = parser.parse_args()

    sandbox_url = args.sandbox_url

    # Derive task ID for MCP server naming
    task_id = "sandbox"
    if args.task_yaml:
        try:
            import yaml
            with open(args.task_yaml) as f:
                task_data = yaml.safe_load(f)
            task_id = task_data.get("task_id", "sandbox")
        except Exception:
            pass

    print(f"[mcp-sandbox] Starting MCP sandbox bridge", file=sys.stderr)
    print(f"[mcp-sandbox] Container URL: {sandbox_url}", file=sys.stderr)
    print(f"[mcp-sandbox] Task: {task_id}", file=sys.stderr)

    from mcp.server.fastmcp import FastMCP

    mcp = FastMCP(
        name=f"claw-eval-sandbox-{task_id}",
        instructions=(
            f"Sandbox tools for task {task_id}. "
            f"Use these tools to execute commands, read/write files, and search "
            f"within the sandbox environment."
        ),
    )

    # --- Bash tool ---
    @mcp.tool(
        name="Bash",
        description="Execute a bash command in the sandbox container and return its output.",
    )
    async def bash(command: str, description: str = "", timeout: int = 120000) -> str:
        return _call_container(sandbox_url, "Bash", {
            "command": command,
            "description": description,
            "timeout": timeout,
        })

    # --- Read tool ---
    @mcp.tool(
        name="Read",
        description="Read a file from the sandbox filesystem. Returns file content.",
    )
    async def read(file_path: str, offset: int = 0, limit: int = 0):
        params: dict = {"file_path": file_path}
        if offset:
            params["offset"] = offset
        if limit:
            params["limit"] = limit
        raw = _call_container(sandbox_url, "Read", params)
        # If the container returned image/PDF frames (image_b64), surface them
        # as ImageContent so the model can actually see the pixels.
        media = _build_media_result(raw)
        if media is not None:
            return media
        return raw

    # --- Write tool ---
    @mcp.tool(
        name="Write",
        description="Write content to a file in the sandbox. Creates parent directories if needed.",
    )
    async def write(file_path: str, content: str) -> str:
        return _call_container(sandbox_url, "Write", {
            "file_path": file_path,
            "content": content,
        })

    # --- Edit tool ---
    @mcp.tool(
        name="Edit",
        description="Perform exact string replacement in a file in the sandbox.",
    )
    async def edit(file_path: str, old_string: str, new_string: str,
                   replace_all: bool = False) -> str:
        return _call_container(sandbox_url, "Edit", {
            "file_path": file_path,
            "old_string": old_string,
            "new_string": new_string,
            "replace_all": replace_all,
        })

    # --- Glob tool ---
    @mcp.tool(
        name="Glob",
        description="Find files matching a glob pattern in the sandbox.",
    )
    async def glob(pattern: str, path: str = "/workspace") -> str:
        return _call_container(sandbox_url, "Glob", {
            "pattern": pattern,
            "path": path,
        })

    # --- Grep tool ---
    @mcp.tool(
        name="Grep",
        description="Search for a regex pattern in files within the sandbox.",
    )
    async def grep(pattern: str, path: str = "/workspace",
                   glob: str = "", output_mode: str = "files_with_matches",
                   case_insensitive: bool = False) -> str:
        params = {"pattern": pattern, "path": path, "output_mode": output_mode}
        if glob:
            params["glob"] = glob
        if case_insensitive:
            params["case_insensitive"] = case_insensitive
        return _call_container(sandbox_url, "Grep", params)

    # --- BrowserScreenshot tool ---
    @mcp.tool(
        name="BrowserScreenshot",
        description=(
            "Capture screenshots of a web page over time. "
            "Opens the URL in a headless browser, then takes multiple screenshots "
            "at regular intervals to show animation progress. "
            "Use this to preview and verify your generated web pages and animations."
        ),
    )
    async def browser_screenshot(
        url: str,
        wait_seconds: float = 2.0,
        frame_count: int = 4,
    ):
        raw = _call_container(sandbox_url, "BrowserScreenshot", {
            "url": url,
            "wait_seconds": wait_seconds,
            "frame_count": frame_count,
        })
        try:
            data = json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return [TextContent(type="text", text=raw)]

        result: list = []
        text_parts = []
        if data.get("url"):
            text_parts.append(f"URL: {data['url']}")
        if data.get("title"):
            text_parts.append(f"Title: {data['title']}")
        if data.get("body_text"):
            text_parts.append(f"Page text:\n{data['body_text']}")
        if text_parts:
            result.append(TextContent(type="text", text="\n\n".join(text_parts)))

        for frame in data.get("frames", []):
            b64 = frame.get("image_b64")
            if b64:
                result.append(ImageContent(
                    type="image", data=b64, mimeType="image/png",
                ))

        return result if result else [TextContent(type="text", text=raw)]

    # --- ReadMedia tool ---
    @mcp.tool(
        name="ReadMedia",
        description=(
            "Read and preview a media file (image, video, or PDF). "
            "Extracts frames at specified intervals for videos. "
            "Renders pages as images for PDFs. "
            "Returns metadata and base64-encoded frame images."
        ),
    )
    async def read_media(
        path: str,
        media_type: str = "auto",
        max_frames: int = 8,
        fps: float = 1.0,
        start_time: float = 0.0,
        end_time: float | None = None,
        screen_size: str | None = None,
    ):
        # Align sampling to the requested window so frames spread across the
        # full range instead of bunching at the start (capped for token safety).
        effective_max_frames = _align_max_frames(
            max_frames, fps, start_time, end_time)
        params: dict = {
            "path": path,
            "media_type": media_type,
            "max_frames": effective_max_frames,
            "fps": fps,
            "start_time": start_time,
        }
        if end_time is not None:
            params["end_time"] = end_time
        if screen_size is not None:
            params["screen_size"] = screen_size
        raw = _call_container(sandbox_url, "ReadMedia", params)
        # Detect a silently-truncated frame window so the agent doesn't assume
        # it scanned the whole requested range.
        warning = _coverage_warning(raw, end_time, effective_max_frames, fps)
        # Wrap frames as ImageContent so vision models actually see the pixels;
        # fall back to the raw text on error / missing-frame responses.
        media = _build_media_result(raw)
        if media is not None:
            if warning and media and isinstance(media[-1], TextContent):
                media[-1] = TextContent(
                    type="text",
                    text=f"{warning}\n\n{media[-1].text}",
                )
            return media
        if warning:
            return f"{warning}\n\n{raw}"
        return raw

    # --- Download tool ---
    @mcp.tool(
        name="Download",
        description=(
            "Download a file as binary (base64-encoded). "
            "Use for retrieving generated files (mp4, gif, html, etc.)."
        ),
    )
    async def download(
        path: str,
        max_bytes: int = 50_000_000,
    ) -> str:
        return _call_container(sandbox_url, "Download", {
            "path": path,
            "max_bytes": max_bytes,
        })

    print(f"[mcp-sandbox] MCP server running with {len(_PATH_MAP)} tools", file=sys.stderr)

    try:
        mcp.run()
    except Exception as e:
        print(f"[mcp-sandbox] Error: {e}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
