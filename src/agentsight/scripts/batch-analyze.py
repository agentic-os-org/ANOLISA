#!/usr/bin/env python3
"""Batch trajectory analysis: find conversation JSONL files in ~/.claude and
~/.qoder, convert to ATIF v1.7, and run the `analyze` CLI on each.

Usage:
    python3 scripts/batch-analyze.py --dim perf [--limit 5] [--min-steps 3]
    python3 scripts/batch-analyze.py --dim cost --dir ~/.claude/projects
    python3 scripts/batch-analyze.py --dim accuracy --file path/to/session.jsonl

Environment variables (forwarded to analyze binary):
    OPENAI_API_KEY, OPENAI_BASE_URL, OPENAI_MODEL
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import List, Optional

# ─── Directories to scan ──────────────────────────────────────────────────────

DEFAULT_DIRS = [
    Path.home() / ".claude" / "projects",
    Path.home() / ".qoder" / "projects",
]

# ─── JSONL → ATIF conversion ─────────────────────────────────────────────────


def extract_text(content) -> str:
    """Extract plain text from message content (string or list of blocks)."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, dict):
                if block.get("type") == "text":
                    parts.append(block.get("text", ""))
                elif block.get("type") == "tool_result":
                    parts.append(block.get("content", ""))
            elif isinstance(block, str):
                parts.append(block)
        return "\n".join(parts)
    return ""


def extract_tool_calls(content) -> list:
    """Extract tool_use blocks as ATIF tool_calls."""
    calls = []
    if not isinstance(content, list):
        return calls
    for block in content:
        if isinstance(block, dict) and block.get("type") == "tool_use":
            calls.append({
                "tool_call_id": block.get("id", ""),
                "function_name": block.get("name", ""),
                "arguments": block.get("input", {}),
            })
    return calls


def extract_tool_results(content) -> list:
    """Extract tool_result blocks as ATIF observation results."""
    results = []
    if not isinstance(content, list):
        return results
    for block in content:
        if isinstance(block, dict) and block.get("type") == "tool_result":
            result_content = block.get("content", "")
            if isinstance(result_content, list):
                result_content = " ".join(
                    b.get("text", "") for b in result_content if isinstance(b, dict)
                )
            results.append({
                "source_call_id": block.get("tool_use_id"),
                "content": str(result_content)[:2000],  # truncate
            })
    return results


def extract_thinking(content) -> Optional[str]:
    """Extract thinking/reasoning content."""
    if not isinstance(content, list):
        return None
    parts = []
    for block in content:
        if isinstance(block, dict) and block.get("type") == "thinking":
            parts.append(block.get("thinking", ""))
    return "\n".join(parts) if parts else None


def jsonl_to_atif(filepath: Path) -> Optional[dict]:
    """Convert a Claude/Qoder conversation JSONL to ATIF v1.7 dict."""
    entries = []
    try:
        with open(filepath, "r", encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    entries.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
    except OSError:
        return None

    # Extract session_id
    session_id = filepath.stem
    agent_name = "unknown"
    model_name = None

    steps = []
    step_id = 0

    for entry in entries:
        entry_type = entry.get("type", "")

        # Detect agent name from runtime-config
        if entry_type == "runtime-config":
            model_name = entry.get("model")
            continue

        # Detect mode/config entries (skip)
        if entry_type in ("mode", "permission-mode", "file-history-snapshot", "summary"):
            continue

        # Get message content
        msg = entry.get("message", {})
        if not isinstance(msg, dict):
            continue
        role = msg.get("role", entry_type)
        content = msg.get("content", "")
        timestamp = entry.get("timestamp")

        # Detect model from assistant messages
        if role == "assistant" and msg.get("model"):
            model_name = msg.get("model")

        if role == "user" or entry_type == "user":
            # Check if this is a tool result (observation)
            tool_results = extract_tool_results(content)
            if tool_results and steps and steps[-1]["source"] == "agent":
                # Attach as observation to previous agent step
                steps[-1]["observation"] = {"results": tool_results}
            else:
                text = extract_text(content)
                if text.strip():
                    step_id += 1
                    steps.append({
                        "step_id": step_id,
                        "timestamp": timestamp,
                        "source": "user",
                        "message": text[:5000],
                    })

        elif role == "assistant" or entry_type == "assistant":
            text = extract_text(content)
            tool_calls = extract_tool_calls(content)
            thinking = extract_thinking(content)

            if not text.strip() and not tool_calls:
                continue

            step_id += 1
            step = {
                "step_id": step_id,
                "timestamp": timestamp,
                "source": "agent",
                "message": text[:5000] if text else None,
                "model_name": msg.get("model") or model_name,
            }
            if tool_calls:
                step["tool_calls"] = tool_calls
            if thinking:
                step["reasoning_content"] = thinking[:3000]

            # Extract usage metrics if present
            usage = msg.get("usage", {})
            if usage:
                metrics = {}
                if usage.get("input_tokens"):
                    metrics["prompt_tokens"] = usage["input_tokens"]
                if usage.get("output_tokens"):
                    metrics["completion_tokens"] = usage["output_tokens"]
                if usage.get("cache_read_input_tokens"):
                    metrics["cached_tokens"] = usage["cache_read_input_tokens"]
                if metrics:
                    step["metrics"] = metrics

            steps.append(step)

    if not steps:
        return None

    # Detect agent name from path
    path_str = str(filepath)
    if ".claude" in path_str:
        agent_name = "claude-code"
    elif ".qoder" in path_str:
        agent_name = "qoder"

    return {
        "schema_version": "ATIF-v1.7",
        "session_id": session_id,
        "agent": {
            "name": agent_name,
            "version": "",
            "model_name": model_name,
        },
        "steps": steps,
    }


# ─── File discovery ───────────────────────────────────────────────────────────


def find_conversation_files(dirs: List[Path], min_size: int = 1024) -> List[Path]:
    """Find JSONL conversation files in given directories."""
    files = []
    for d in dirs:
        if not d.exists():
            continue
        for f in d.rglob("*.jsonl"):
            # Skip subagent files and small files
            if "subagent" in str(f):
                continue
            try:
                if f.stat().st_size >= min_size:
                    files.append(f)
            except OSError:
                continue
    # Sort by modification time (newest first)
    files.sort(key=lambda f: f.stat().st_mtime, reverse=True)
    return files


# ─── Main ─────────────────────────────────────────────────────────────────────


def main():
    parser = argparse.ArgumentParser(
        description="Batch analyze agent trajectories from ~/.claude and ~/.qoder"
    )
    parser.add_argument(
        "--dim",
        required=True,
        choices=["perf", "cost", "accuracy"],
        help="Analysis dimension",
    )
    parser.add_argument(
        "--dir",
        action="append",
        help="Directory to scan (can repeat; default: ~/.claude/projects + ~/.qoder/projects)",
    )
    parser.add_argument(
        "--file",
        help="Analyze a single JSONL file directly",
    )
    parser.add_argument(
        "--limit",
        type=int,
        default=10,
        help="Max number of files to analyze (default: 10)",
    )
    parser.add_argument(
        "--min-steps",
        type=int,
        default=3,
        help="Skip conversations with fewer steps (default: 3)",
    )
    parser.add_argument(
        "--binary",
        default=None,
        help="Path to analyze binary (default: cargo run -p agentsight-opt --bin analyze)",
    )
    args = parser.parse_args()

    # Determine analyze command
    if args.binary:
        analyze_cmd = [args.binary]
    else:
        analyze_cmd = ["cargo", "run", "-q", "-p", "agentsight-opt", "--bin", "analyze", "--"]

    # Collect files
    if args.file:
        files = [Path(args.file)]
    else:
        dirs = [Path(d).expanduser() for d in args.dir] if args.dir else DEFAULT_DIRS
        print(f"Scanning directories: {[str(d) for d in dirs]}", file=sys.stderr)
        files = find_conversation_files(dirs)
        print(f"Found {len(files)} conversation files", file=sys.stderr)

    # Analyze each file
    analyzed = 0
    skipped = 0
    failed = 0

    for filepath in files[: args.limit]:
        print(f"\n{'='*60}", file=sys.stderr)
        print(f"File: {filepath}", file=sys.stderr)

        # Convert to ATIF
        atif = jsonl_to_atif(filepath)
        if atif is None:
            print(f"  SKIP: cannot parse or empty", file=sys.stderr)
            skipped += 1
            continue

        n_steps = len(atif["steps"])
        if n_steps < args.min_steps:
            print(f"  SKIP: only {n_steps} steps (< {args.min_steps})", file=sys.stderr)
            skipped += 1
            continue

        print(
            f"  Session: {atif['session_id']} | Agent: {atif['agent']['name']} | Steps: {n_steps}",
            file=sys.stderr,
        )

        # Write ATIF to temp file and run analyze
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".json", prefix="atif-", delete=False
        ) as tmp:
            json.dump(atif, tmp, ensure_ascii=False)
            tmp_path = tmp.name

        try:
            result = subprocess.run(
                analyze_cmd + [tmp_path, "--dim", args.dim],
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                universal_newlines=True,
                timeout=300,
            )
            if result.returncode == 0:
                print(f"  OK ({args.dim})", file=sys.stderr)
                # Output result with file attribution
                output = json.loads(result.stdout)
                output["_source_file"] = str(filepath)
                output["_session_id"] = atif["session_id"]
                output["_steps"] = n_steps
                print(json.dumps(output, ensure_ascii=False, indent=2))
                analyzed += 1
            else:
                print(f"  FAIL: {result.stderr.strip()[:200]}", file=sys.stderr)
                failed += 1
        except subprocess.TimeoutExpired:
            print(f"  TIMEOUT (300s)", file=sys.stderr)
            failed += 1
        except json.JSONDecodeError:
            print(f"  FAIL: invalid JSON output", file=sys.stderr)
            failed += 1
        finally:
            os.unlink(tmp_path)

    # Summary
    print(f"\n{'='*60}", file=sys.stderr)
    print(
        f"Done: {analyzed} analyzed, {skipped} skipped, {failed} failed",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
