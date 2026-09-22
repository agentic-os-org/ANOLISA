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

"""Run claw-eval grader on a trace file.

Standalone grader runner that:
1. Loads a trace file (JSONL)
2. Loads the task-specific grader
3. Configures LLM judge (for rubric-based scoring)
4. Runs grading
5. Appends grading_result to the trace
6. Prints scores

Usage:
    python grader_runner.py \
        --trace /path/to/trace.jsonl \
        --task-yaml /path/to/task.yaml
"""

from __future__ import annotations

import argparse
import inspect
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

# Add claw-eval to path (repo root is two levels up from src/ce_runner/)
_CLAW_EVAL_SRC = Path(__file__).resolve().parent.parent.parent / "claw-eval" / "src"
if _CLAW_EVAL_SRC.is_dir():
    sys.path.insert(0, str(_CLAW_EVAL_SRC))

from claw_eval.graders.registry import get_grader
from claw_eval.graders.llm_judge import LLMJudge
from claw_eval.models.scoring import compute_task_score, is_pass
from claw_eval.models.task import TaskDefinition
from claw_eval.trace.reader import load_trace


def now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def grade_trace(trace_path: str, task_yaml_path: str, judge_config: dict | None = None,
                env_snapshot_data: dict | None = None) -> dict:
    """Grade a trace file and return scores."""
    trace_path = Path(trace_path)
    task_yaml_path = Path(task_yaml_path)

    # Load task
    task = TaskDefinition.from_yaml(task_yaml_path)
    tasks_dir = task_yaml_path.parent.parent  # tasks/ directory

    # Load trace
    print(f"[grader] Loading trace: {trace_path}", file=sys.stderr)
    start, messages, dispatches, media_events, end, audit_data = load_trace(str(trace_path))

    print(f"[grader]   trace_id={start.trace_id}", file=sys.stderr)
    print(f"[grader]   task_id={start.task_id}", file=sys.stderr)
    print(f"[grader]   messages={len(messages)}, dispatches={len(dispatches)}", file=sys.stderr)
    print(f"[grader]   audit_services={list(audit_data.keys())}", file=sys.stderr)

    # Load grader
    print(f"[grader] Loading grader for task: {task.task_id}", file=sys.stderr)
    grader = get_grader(task.task_id, tasks_dir=str(tasks_dir), task_dir=str(task_yaml_path.parent))
    print(f"[grader]   grader: {type(grader).__name__}", file=sys.stderr)

    # Configure LLM judge
    judge = None
    if judge_config:
        try:
            judge = LLMJudge(
                model_id=judge_config.get("model_id", ""),
                api_key=judge_config.get("api_key"),
                base_url=judge_config.get("base_url", ""),
            )
            print(f"[grader]   judge: {judge_config.get('model_id', '?')} @ {judge_config.get('base_url', '?')}", file=sys.stderr)
        except Exception as e:
            print(f"[grader] WARN: Failed to init judge: {e}", file=sys.stderr)

    # Run grading
    params = inspect.signature(grader.grade).parameters
    kwargs = {"audit_data": audit_data, "judge": judge}
    if "media_events" in params:
        kwargs["media_events"] = media_events
    if "env_snapshot" in params:
        kwargs["env_snapshot"] = env_snapshot_data

    scores = grader.grade(messages, dispatches, task, **kwargs)

    # Compute final score
    task_score = compute_task_score(scores)
    passed = is_pass(task_score)

    print(f"[grader] Scores:", file=sys.stderr)
    print(f"[grader]   completion:     {scores.completion:.2f}", file=sys.stderr)
    print(f"[grader]   robustness:     {scores.robustness:.2f}", file=sys.stderr)
    print(f"[grader]   communication:  {scores.communication:.2f}", file=sys.stderr)
    print(f"[grader]   safety:         {scores.safety:.1f}", file=sys.stderr)
    print(f"[grader]   task_score:     {task_score:.2f}", file=sys.stderr)
    print(f"[grader]   passed:         {passed}", file=sys.stderr)

    # Collect judge_calls from real LLMJudge log (not hardcoded [])
    judge_calls = judge.get_call_log() if (judge and hasattr(judge, "get_call_log")) else []

    # Append grading_result to trace
    grading_result = {
        "type": "grading_result",
        "trace_id": start.trace_id,
        "task_id": task.task_id,
        "scores": {
            "completion": scores.completion,
            "robustness": scores.robustness,
            "communication": scores.communication,
            "safety": scores.safety,
            "efficiency_turns": scores.efficiency_turns,
            "efficiency_tokens": scores.efficiency_tokens,
            "efficiency_wall_time_s": scores.efficiency_wall_time_s,
        },
        "task_score": task_score,
        "passed": passed,
        "failure_modes": [],
        "judge_calls": judge_calls,
        "timestamp": now_iso(),
    }

    # Update trace_end with real scores (was placeholder 0.0 from converter)
    # then rewrite trace file: original events (with patched trace_end) + grading_result
    real_scores = {
        "completion": scores.completion,
        "robustness": scores.robustness,
        "communication": scores.communication,
        "safety": scores.safety,
        "efficiency_turns": scores.efficiency_turns,
        "efficiency_tokens": scores.efficiency_tokens,
        "efficiency_wall_time_s": scores.efficiency_wall_time_s,
    }
    try:
        with open(trace_path) as _f:
            _existing = [json.loads(line) for line in _f if line.strip()]
        # Drop any prior grading_result (idempotent re-grading)
        _existing = [e for e in _existing if e.get("type") != "grading_result"]
        for ev in _existing:
            if ev.get("type") == "trace_end":
                # Preserve efficiency_* values already computed by converter when grader didn't set them
                merged_scores = dict(ev.get("scores") or {})
                merged_scores.update(real_scores)
                ev["scores"] = merged_scores
                ev["task_score"] = task_score
                ev["passed"] = passed
        with open(trace_path, "w") as _f:
            for ev in _existing:
                _f.write(json.dumps(ev, ensure_ascii=False) + "\n")
            _f.write(json.dumps(grading_result, ensure_ascii=False) + "\n")
    except Exception as _e:
        # Fallback: append-only (legacy behavior) so we don't lose grading_result on IO error
        print(f"[grader] WARN: trace_end update failed ({_e}); falling back to append.", file=sys.stderr)
        with open(trace_path, "a") as _f:
            _f.write(json.dumps(grading_result, ensure_ascii=False) + "\n")

    print(f"[grader] Grading result appended to trace; trace_end synced", file=sys.stderr)

    result = {
        "task_score": task_score,
        "passed": passed,
        "completion": scores.completion,
        "robustness": scores.robustness,
        "communication": scores.communication,
        "safety": scores.safety,
    }

    return result


def main():
    parser = argparse.ArgumentParser(description="Run claw-eval grader on a trace")
    parser.add_argument("--trace", required=True, help="Path to trace JSONL")
    parser.add_argument("--task-yaml", required=True, help="Path to task.yaml")
    parser.add_argument("--judge-model", default=None, help="Judge model ID")
    parser.add_argument("--judge-base-url", default=None,
                        help="Judge API base URL")
    parser.add_argument("--judge-api-key", default=None, help="Judge API key")
    parser.add_argument("--env-snapshot", default=None, help="Path to env_snapshot JSON file")
    args = parser.parse_args()

    judge_config = {
        "model_id": args.judge_model or os.environ.get("JUDGE_MODEL_ID", ""),
        "base_url": args.judge_base_url or os.environ.get("JUDGE_BASE_URL", ""),
        "api_key": args.judge_api_key or os.environ.get("JUDGE_API_KEY", ""),
    }
    # Remove None values
    judge_config = {k: v for k, v in judge_config.items() if v is not None}

    env_snapshot = None
    if args.env_snapshot:
        with open(args.env_snapshot) as f:
            env_snapshot = json.load(f)

    result = grade_trace(args.trace, args.task_yaml, judge_config, env_snapshot_data=env_snapshot)
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
