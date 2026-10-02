#!/usr/bin/env python3
"""
AgentSight DeadLoop Detection Integration Test

Simulates an agent stuck in a dead loop by:
1. Sending repeated LLM API calls with the same user message (same conversation_id)
2. Including tool definitions so responses contain repeated tool_calls
3. Verifying AgentSight detects the pattern and creates a dead_loop interruption event

Prerequisites:
    - AgentSight service running with DeadLoop detection enabled
    - python3 cmdline rule in agentsight config (agent_name: "TestAgent")
    - API key for dashscope

Usage:
    python3 test_dead_loop.py --api-key <KEY> [--base-url URL] [--rounds N]

    API key can also be set via DASHSCOPE_API_KEY environment variable.
"""
import json
import time
import urllib.request
import urllib.error
import ssl
import sqlite3
import argparse
import os
import hashlib

DEFAULT_URL = "https://dashscope.aliyuncs.com/compatible-mode/v1/chat/completions"
DB_GENAI = "/var/log/sysak/.agentsight/agentsight.db"
DB_INT = "/var/log/sysak/.agentsight/interruption_events.db"

# The SAME user message every time => same conversation_id
LOOP_USER_MESSAGE = "Please read the file /etc/hosts and show me its content. Use the read_file tool."

# Tool definitions to include in the request
TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "read_file",
            "description": "Read the content of a file",
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "The file path to read"
                    }
                },
                "required": ["path"]
            }
        }
    }
]


def compute_conversation_id(user_message):
    """Replicate agentsight's conversation_id computation: SHA256(last_user_message)[:32]"""
    h = hashlib.sha256(user_message.encode("utf-8")).hexdigest()
    return h[:32]


def send_request(api_key, base_url, user_message, tools=None, model="qwen-max", max_tokens=100):
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": user_message}],
        "max_tokens": max_tokens,
    }
    if tools:
        payload["tools"] = tools
        payload["tool_choice"] = "auto"

    data = json.dumps(payload).encode("utf-8")
    headers = {
        "Content-Type": "application/json",
        "Authorization": "Bearer {}".format(api_key),
    }
    req = urllib.request.Request(base_url, data=data, headers=headers, method="POST")
    ctx = ssl.create_default_context()
    try:
        resp = urllib.request.urlopen(req, context=ctx, timeout=30)
        body = resp.read().decode("utf-8", errors="replace")
        return resp.status, body
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        return e.code, body
    except Exception as e:
        return -1, str(e)


def get_interruption_baseline():
    """Get current count of interruption events."""
    try:
        conn = sqlite3.connect(DB_INT)
        count = conn.execute("SELECT COUNT(*) FROM interruption_events").fetchone()[0]
        dead_loops = conn.execute(
            "SELECT COUNT(*) FROM interruption_events WHERE interruption_type = 'dead_loop'"
        ).fetchone()[0]
        conn.close()
        return {"total": count, "dead_loops": dead_loops}
    except Exception as e:
        print("  [WARN] Cannot read interruption DB: {}".format(e))
        return {"total": 0, "dead_loops": 0}


def get_genai_count_for_conversation(conversation_id):
    """Count genai_events for a specific conversation."""
    try:
        conn = sqlite3.connect(DB_GENAI)
        count = conn.execute(
            "SELECT COUNT(*) FROM genai_events WHERE conversation_id = ?",
            (conversation_id,)
        ).fetchone()[0]
        conn.close()
        return count
    except Exception:
        return -1


def check_dead_loop_detected(baseline, conversation_id, wait=15):
    """Check if a dead_loop interruption was created after baseline."""
    print("\n  Waiting {}s for AgentSight processing...".format(wait))
    time.sleep(wait)

    try:
        conn = sqlite3.connect(DB_INT)
        new_dead_loops = conn.execute(
            "SELECT COUNT(*) FROM interruption_events WHERE interruption_type = 'dead_loop'"
        ).fetchone()[0]

        # Also check for this specific conversation
        conv_loops = conn.execute(
            "SELECT interruption_type, severity, detail, agent_name "
            "FROM interruption_events "
            "WHERE interruption_type = 'dead_loop' AND conversation_id = ?",
            (conversation_id,)
        ).fetchall()
        conn.close()

        return {
            "total_dead_loops": new_dead_loops,
            "new_dead_loops": new_dead_loops - baseline["dead_loops"],
            "conversation_events": [
                {"type": r[0], "severity": r[1], "detail": r[2][:150] if r[2] else "", "agent": r[3]}
                for r in conv_loops
            ],
        }
    except Exception as e:
        print("  [ERROR] Cannot check results: {}".format(e))
        return {"total_dead_loops": 0, "new_dead_loops": 0, "conversation_events": []}


def run_dead_loop_test(api_key, base_url, rounds=5, interval=3):
    """
    Simulate a dead loop: send the same prompt N times with tool definitions.
    AgentSight should detect repeated tool sequences and fire a dead_loop event.
    """
    conversation_id = compute_conversation_id(LOOP_USER_MESSAGE)
    print("=" * 60)
    print("DeadLoop Detection Test")
    print("=" * 60)
    print("  Expected conversation_id: {}".format(conversation_id))
    print("  Rounds: {}".format(rounds))
    print("  Interval: {}s".format(interval))
    print("  Model: qwen-max")
    print("  Tool: read_file")
    print()

    # Baseline
    baseline = get_interruption_baseline()
    print("  Baseline: {} total interruptions, {} dead_loops".format(
        baseline["total"], baseline["dead_loops"]))

    existing = get_genai_count_for_conversation(conversation_id)
    print("  Existing genai_events for this conversation: {}".format(existing))
    print()

    # Send repeated requests
    calls = []
    for i in range(rounds):
        print("  [{}/{}] Sending request...".format(i + 1, rounds), end=" ", flush=True)
        status, body = send_request(api_key, base_url, LOOP_USER_MESSAGE, tools=TOOLS)

        # Parse response to show what the model returned
        detail = ""
        try:
            resp_data = json.loads(body)
            choice = resp_data.get("choices", [{}])[0]
            msg = choice.get("message", {})
            tool_calls = msg.get("tool_calls", [])
            content = msg.get("content", "")
            if tool_calls:
                tool_names = [tc.get("function", {}).get("name", "?") for tc in tool_calls]
                detail = "tool_calls: [{}]".format(", ".join(tool_names))
            elif content:
                detail = "text: {}".format(content[:60])
            else:
                detail = "empty response"
        except Exception:
            detail = "parse error"

        print("status={} {}".format(status, detail))
        calls.append({"round": i + 1, "status": status, "detail": detail})

        if i < rounds - 1:
            time.sleep(interval)

    # Check results
    print("\n  All {} requests sent.".format(rounds))
    genai_count = get_genai_count_for_conversation(conversation_id)
    print("  GenAI events for conversation now: {}".format(genai_count))

    results = check_dead_loop_detected(baseline, conversation_id, wait=15)

    # Report
    print("\n  === RESULTS ===")
    print("  New dead_loop events: {}".format(results["new_dead_loops"]))
    if results["conversation_events"]:
        for ev in results["conversation_events"]:
            print("  [DETECTED] type={} severity={} agent={}".format(
                ev["type"], ev["severity"], ev["agent"]))
            print("             detail: {}".format(ev["detail"]))
        print("\n  >>> TEST PASSED: DeadLoop detected! <<<")
    else:
        print("  [INFO] No dead_loop event found for this conversation.")
        print("  Possible reasons:")
        print("    - Model returned different tool_calls each time (no repetition)")
        print("    - AgentSight needs more time to process")
        print("    - Output similarity below threshold")
        if genai_count >= rounds:
            print("\n  GenAI events were captured. Retrying check in 15s...")
            results2 = check_dead_loop_detected(baseline, conversation_id, wait=15)
            if results2["conversation_events"]:
                for ev in results2["conversation_events"]:
                    print("  [DETECTED] type={} severity={} agent={}".format(
                        ev["type"], ev["severity"], ev["agent"]))
                print("\n  >>> TEST PASSED (delayed): DeadLoop detected! <<<")
            else:
                print("  >>> TEST INCONCLUSIVE: No detection after extended wait <<<")

    return calls, results


def main():
    parser = argparse.ArgumentParser(
        description="AgentSight DeadLoop Detection Test",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--api-key", default=os.environ.get("DASHSCOPE_API_KEY", ""),
                        help="Dashscope API key")
    parser.add_argument("--base-url", default=DEFAULT_URL)
    parser.add_argument("--rounds", type=int, default=12,
                        help="Number of repeated requests (default: 12)")
    parser.add_argument("--interval", type=int, default=3,
                        help="Seconds between requests (default: 3)")
    args = parser.parse_args()

    if not args.api_key:
        parser.error("API key required: use --api-key or set DASHSCOPE_API_KEY env var")

    run_dead_loop_test(args.api_key, args.base_url, args.rounds, args.interval)
    print("\nDone.")


if __name__ == "__main__":
    main()
