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

_ASCII_ID_LOWER = str.maketrans("ABCDEFGHIJKLMNOPQRSTUVWXYZ", "abcdefghijklmnopqrstuvwxyz")

# ─── JSONL → ATIF conversion ─────────────────────────────────────────────────


def extract_text(content, *, include_tool_results: bool = True) -> str:
    """Extract plain text from message content (string or list of blocks)."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, dict):
                if block.get("type") == "text":
                    parts.append(block.get("text", ""))
                elif block.get("type") == "tool_result" and include_tool_results:
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
            result = {
                "source_call_id": block.get("tool_use_id"),
                "content": str(result_content)[:2000],  # truncate
            }
            if isinstance(block.get("is_error"), bool):
                result["extra"] = {"is_error": block["is_error"]}
            results.append(result)
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


def _call_id_keys(identifier: str) -> tuple[tuple[str, str], tuple[str, str]]:
    """Index the shared agentsight_atif::same_call_id echo contract."""
    case_key = ("case", identifier.translate(_ASCII_ID_LOWER))
    kind = "separated" if "_" in identifier or "-" in identifier else "plain"
    normalized = "".join(char.lower() for char in identifier if char not in "_-")
    return case_key, (kind, normalized)


def usage_to_metrics(usage) -> dict:
    """Map a provider usage object onto ATIF metric names."""
    if not isinstance(usage, dict):
        return {}
    counters = {
        key: value for key, value in usage.items() if type(value) is int and value >= 0
    }
    metrics = {}
    for source, target in (
        ("input_tokens", "prompt_tokens"),
        ("output_tokens", "completion_tokens"),
        ("cache_read_input_tokens", "cached_tokens"),
    ):
        if source in counters:
            metrics[target] = counters[source]
    if "prompt_tokens" in metrics:
        # Anthropic reports uncached, read, and created input separately.
        metrics["prompt_tokens"] += counters.get("cache_read_input_tokens", 0)
        metrics["prompt_tokens"] += counters.get("cache_creation_input_tokens", 0)
    return metrics


def jsonl_to_atif(filepath: Path) -> Optional[dict]:
    """Convert a Claude/Qoder conversation JSONL to ATIF v1.7 dict."""
    entries = []
    try:
        with open(filepath, "r", encoding="utf-8-sig", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    parsed = json.loads(line)
                except json.JSONDecodeError:
                    continue
                # json.loads also accepts scalars and arrays; only objects
                # carry event fields, so anything else is skipped exactly like
                # a decode error instead of aborting the whole batch.
                if isinstance(parsed, dict):
                    entries.append(parsed)
    except OSError:
        return None

    # Extract session_id
    session_id = filepath.stem
    agent_name = "unknown"
    model_name = None

    steps = []
    tool_call_steps = {}
    tool_call_aliases = {}
    usage_records = []
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
            # Tool replies can arrive after other assistant or user records.
            tool_results = extract_tool_results(content)
            unmatched_results = []
            for result in tool_results:
                call_id = result.get("source_call_id")
                owner = tool_call_steps.get(call_id) if isinstance(call_id, str) else None
                if owner is None and isinstance(call_id, str):
                    case_key, (kind, normalized) = _call_id_keys(call_id)
                    other_kind = "plain" if kind == "separated" else "separated"
                    matches = tool_call_aliases.get(case_key, set()) | tool_call_aliases.get(
                        (other_kind, normalized), set()
                    )
                    # An exact ID wins; an ambiguous echo must not steal a sibling's result.
                    if len(matches) == 1:
                        owner = tool_call_steps[next(iter(matches))]
                if owner is not None:
                    owner.setdefault("observation", {"results": []})["results"].append(result)
                else:
                    unmatched_results.append(result)

            # A mixed reply still contains a genuine user turn. Pure orphan
            # results have no issuing step and must not become human input.
            text = extract_text(content, include_tool_results=False)
            if text.strip():
                step_id += 1
                step = {
                    "step_id": step_id,
                    "timestamp": timestamp,
                    "source": "user",
                    "message": text[:5000],
                }
                if unmatched_results:
                    step["observation"] = {"results": unmatched_results}
                steps.append(step)

        elif role == "assistant" or entry_type == "assistant":
            text = extract_text(content)
            tool_calls = extract_tool_calls(content)
            thinking = extract_thinking(content)

            # Thinking is part of the turn even when the record carries no
            # text or tool call: Claude Code can stream it in its own record,
            # and dropping the step also dropped the reasoning_content.
            if not text.strip() and not tool_calls and not thinking:
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
                for call in tool_calls:
                    call_id = call.get("tool_call_id")
                    if isinstance(call_id, str) and call_id:
                        tool_call_steps[call_id] = step
                        for key in _call_id_keys(call_id):
                            tool_call_aliases.setdefault(key, set()).add(call_id)
            if thinking:
                step["reasoning_content"] = thinking[:3000]

            # Usage is attached after the walk: Claude Code splits one API
            # message into several assistant records that repeat (or
            # cumulatively advance) the same usage, so counting each record
            # doubles the cost input.
            usage = msg.get("usage")
            if isinstance(usage, dict):
                message_id = msg.get("id")
                if not isinstance(message_id, str) or not message_id:
                    message_id = None
                usage_records.append((step, message_id, usage))

            steps.append(step)

    # One message id may span several records; the last snapshot is the most
    # complete. Records without an id keep their own usage, as before.
    last_usage_index = {}
    for index, (_, message_id, _) in enumerate(usage_records):
        if message_id is not None:
            last_usage_index[message_id] = index
    for index, (step, message_id, usage) in enumerate(usage_records):
        if message_id is not None and last_usage_index[message_id] != index:
            continue
        metrics = usage_to_metrics(usage)
        if metrics:
            step["metrics"] = metrics

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
            mode="w", encoding="utf-8", suffix=".json", prefix="atif-", delete=False
        ) as tmp:
            json.dump(atif, tmp, ensure_ascii=False)
            tmp_path = tmp.name

        try:
            try:
                result = subprocess.run(
                    analyze_cmd + [tmp_path, "--dim", args.dim],
                    stdout=subprocess.PIPE,
                    stderr=subprocess.PIPE,
                    universal_newlines=True,
                    timeout=300,
                )
            except OSError as error:
                print(f"  FAIL: cannot run analyzer: {error}", file=sys.stderr)
                failed += 1
                continue
            if result.returncode == 0:
                # Output result with file attribution
                output = json.loads(result.stdout)
                if not isinstance(output, dict):
                    print("  FAIL: expected a JSON object from analyzer", file=sys.stderr)
                    failed += 1
                    continue
                output["_source_file"] = str(filepath)
                output["_session_id"] = atif["session_id"]
                output["_steps"] = n_steps
                print(json.dumps(output, ensure_ascii=False, indent=2))
                print(f"  OK ({args.dim})", file=sys.stderr)
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
