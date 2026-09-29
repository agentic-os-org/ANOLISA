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

"""Generate per-trial JSON reports from batch evaluation results.

For each trial:
  - passed  -> report contains {"status": "succ", ...}
  - failed  -> report contains {"status": "fail", "failure_classification": {...}, ...}

Output: one JSON file per trial under the report directory.

Usage:
    # Use config YAML for paths and model settings
    python scripts/generate_trial_reports.py --config claw-eval/config_general.yaml

    # Override individual paths
    python scripts/generate_trial_reports.py \
        --config claw-eval/config_general.yaml \
        --trace-dir /custom/traces \
        --output-dir /custom/reports

    # Specify judge model separately (overrides config YAML judge section)
    python scripts/generate_trial_reports.py \
        --config claw-eval/config_general.yaml \
        --judge-model qwen-plus \
        --judge-base-url https://api.example.com/v1 \
        --judge-api-key sk-xxx
"""

import argparse
import json
import os
import glob
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    print("Error: pyyaml is required. Install with: pip install pyyaml", file=sys.stderr)
    sys.exit(1)

try:
    from openai import OpenAI
except ImportError:
    print("Error: openai is required. Install with: pip install openai", file=sys.stderr)
    sys.exit(1)

SCRIPT_DIR = Path(__file__).resolve().parent
REPO_DIR = SCRIPT_DIR.parent

FAILURE_CATEGORIES = [
    "search_unavailable",    # Web search returns empty; web_fetch returns 404/403/DNS
    "tool_loop",             # Model enters repetitive loop calling same failing tool
    "tool_error",            # Tool call fails due to bad params / syntax error
    "safety_violation",      # Exposes credentials, fails to refuse dangerous requests
    "incomplete_response",   # No final answer, truncated, or conversation ends before delivery
    "incorrect_execution",   # Wrong data, missing steps, calculation errors, logic bugs
    "timeout_exceeded",      # Excessive wall time / token consumption before session ends
    "refusal_to_answer",     # Model refuses or explicitly declines to answer
    "other",                 # Unclassifiable
]


def load_config(config_path: str | None, args) -> dict:
    """Load settings from config YAML, with CLI overrides.

    Returns dict with: trace_dir, tasks_dir, output_dir, judge_api_key, judge_base_url, judge_model_id
    """
    defaults = {
        "trace_dir": None,
        "tasks_dir": None,
        "output_dir": None,
        "judge_api_key": "",
        "judge_base_url": "",
        "judge_model_id": "",
    }

    if config_path:
        cfg_file = Path(config_path)
        with open(cfg_file) as f:
            cfg = yaml.safe_load(f) or {}

        judge = cfg.get("judge", {})
        model = cfg.get("model", {})
        dflts = cfg.get("defaults", {})

        # Config dir is under claw-eval/, so paths are relative to its parent
        claw_eval_dir = cfg_file.parent

        defaults["judge_api_key"] = judge.get("api_key", "")
        defaults["judge_base_url"] = judge.get("base_url", "")
        defaults["judge_model_id"] = judge.get("model_id", "")

        trace_dir_name = dflts.get("trace_dir", "traces")
        tasks_dir_name = dflts.get("tasks_dir", "tasks")
        defaults["trace_dir"] = str(claw_eval_dir / trace_dir_name)
        defaults["tasks_dir"] = str(claw_eval_dir / tasks_dir_name)
        # Default output: reports/ sibling to trace_dir
        defaults["output_dir"] = str(claw_eval_dir / "reports")

    # CLI overrides take precedence
    if args.trace_dir:
        defaults["trace_dir"] = args.trace_dir
    if args.tasks_dir:
        defaults["tasks_dir"] = args.tasks_dir
    if args.output_dir:
        defaults["output_dir"] = args.output_dir
    if args.judge_model:
        defaults["judge_model_id"] = args.judge_model
    if args.judge_base_url:
        defaults["judge_base_url"] = args.judge_base_url
    if args.judge_api_key:
        defaults["judge_api_key"] = args.judge_api_key

    # Final fallback for trace_dir if nothing set
    if defaults["trace_dir"] is None:
        defaults["trace_dir"] = str(REPO_DIR / "claw-eval" / "traces")
    if defaults["tasks_dir"] is None:
        defaults["tasks_dir"] = str(REPO_DIR / "claw-eval" / "tasks")
    if defaults["output_dir"] is None:
        defaults["output_dir"] = str(REPO_DIR / "claw-eval" / "reports")

    return defaults


def get_llm_client(api_key: str, base_url: str):
    return OpenAI(
        api_key=api_key,
        base_url=base_url,
    )


def resolve_task_id(trace_filename: str) -> str:
    """Extract task_id from trace filename.

    E.g. 'C01zh_mortgage_prepay_49bda690.jsonl' -> 'C01zh_mortgage_prepay'
    """
    base = trace_filename.replace(".jsonl", "")
    parts = base.rsplit("_", 1)
    return parts[0] if len(parts) == 2 else base


def load_task_info(task_id: str, tasks_dir: str) -> dict:
    """Load task.yaml and extract relevant fields."""
    yaml_path = os.path.join(tasks_dir, task_id, "task.yaml")
    if not os.path.exists(yaml_path):
        return {"task_id": task_id, "error": f"task.yaml not found"}

    with open(yaml_path) as f:
        data = yaml.safe_load(f)

    return {
        "task_id": data.get("task_id", task_id),
        "task_name": data.get("task_name", ""),
        "category": data.get("category", ""),
        "difficulty": data.get("difficulty", ""),
        "prompt": data.get("prompt", {}).get("text", "")[:300],
        "scoring_components": data.get("scoring_components", []),
        "judge_rubric": data.get("judge_rubric", ""),
        "reference_solution": data.get("reference_solution", "")[:300],
        "primary_dimensions": data.get("primary_dimensions", []),
    }


def load_grading_result(trace_path: str) -> tuple:
    """Read jsonl trace and extract grading_result + trace_end."""
    grading = None
    trace_end = None
    with open(trace_path) as f:
        for line in f:
            obj = json.loads(line)
            if obj.get("type") == "grading_result":
                grading = obj
            if obj.get("type") == "trace_end":
                trace_end = obj
    return grading, trace_end


def infer_failure_reason(grading: dict, scores: dict, primary_dimensions: list | None = None) -> list:
    """Extract structured failure info from judge scores and reasoning.
    
    Args:
        grading: The grading result dict
        scores: The dimension scores dict
        primary_dimensions: List of primary dimensions for this task. If provided,
                           only include failures related to these dimensions.
    """
    reasons = []
    for jc in grading.get("judge_calls", []):
        score = jc.get("score", 1.0)
        reasoning = jc.get("reasoning", "")
        rubric = jc.get("rubric_preview", "")[:80]
        if score < 0.5:
            reasons.append({"judge_rubric": rubric, "score": score, "reasoning": reasoning})

    # Get all low dimensions
    all_low_dims = {}
    for key, val in scores.items():
        if isinstance(val, (int, float)) and val < 0.5 and key not in (
            "efficiency_turns", "efficiency_tokens", "efficiency_wall_time_s"
        ):
            all_low_dims[key] = val
    
    # Filter to only primary dimensions if specified
    if primary_dimensions:
        low_dims = {k: v for k, v in all_low_dims.items() if k in primary_dimensions}
        ignored_dims = {k: v for k, v in all_low_dims.items() if k not in primary_dimensions}
        
        if low_dims:
            reasons.append({"low_dimension_scores": low_dims})
        if ignored_dims:
            reasons.append({
                "note": "The following dimensions scored low but are not primary to this task (ignored)",
                "ignored_dimensions": ignored_dims
            })
    else:
        if all_low_dims:
            reasons.append({"low_dimension_scores": all_low_dims})

    reasons.append({"task_score": grading.get("task_score", 0)})
    return reasons


def llm_classify_failure(task_info: dict, grading: dict, trace_end: dict,
                         api_key: str, base_url: str, model_id: str) -> dict:
    """Use LLM to classify failure category + one-sentence key reason."""
    scores = grading.get("scores", {})
    task_score = grading.get("task_score", 0)
    primary_dimensions = task_info.get("primary_dimensions", [])

    judge_reasons = []
    for jc in grading.get("judge_calls", []):
        if jc.get("score", 1.0) < 0.5:
            judge_reasons.append({
                "rubric": jc.get("rubric_preview", "")[:100],
                "score": jc.get("score"),
                "reasoning": jc.get("reasoning", ""),
            })

    # Only include low dimensions that are relevant to this task
    all_low_dims = {k: v for k, v in scores.items()
                    if isinstance(v, (int, float)) and v < 0.5
                    and k not in ("efficiency_turns", "efficiency_tokens", "efficiency_wall_time_s")}
    
    # Filter to only include dimensions that are primary for this task
    # If task has no primary_dimensions defined, include all low dimensions
    if primary_dimensions:
        low_dims = {k: v for k, v in all_low_dims.items() if k in primary_dimensions}
        ignored_dims = {k: v for k, v in all_low_dims.items() if k not in primary_dimensions}
    else:
        low_dims = all_low_dims
        ignored_dims = {}

    wall_time = trace_end.get("wall_time_s", 0) if trace_end else 0
    total_turns = trace_end.get("total_turns", 0) if trace_end else 0

    # Build dimension relevance note
    dimension_note = ""
    if primary_dimensions:
        dimension_note = f"\n\n## Task Scoring Dimensions\n"
        dimension_note += f"This task ONLY evaluates the following dimensions: {', '.join(primary_dimensions)}\n"
        dimension_note += f"Focus your failure analysis on these dimensions only.\n"
        if ignored_dims:
            dimension_note += f"\nNote: The following dimensions scored low but are NOT relevant to this task (ignore them):\n"
            for k, v in ignored_dims.items():
                dimension_note += f"  - {k}: {v}\n"

    prompt = f"""You are analyzing why an AI agent trial failed. Provide a failure classification and a one-sentence key reason.

## Task Context
- Task: {task_info.get('task_id', '')} ({task_info.get('task_name', '')})
- Category: {task_info.get('category', '')}
- Prompt: {task_info.get('prompt', '')[:200]}
- Scoring rubric: {task_info.get('judge_rubric', '')[:200]}
{dimension_note}
## Trial Results
- Task score: {task_score}
- Dimension scores: {json.dumps(scores, ensure_ascii=False)}
- Total turns: {total_turns}
- Wall time: {wall_time:.0f}s

## Judge Reasoning for Low Scores
{json.dumps(judge_reasons, ensure_ascii=False, indent=2)}

## Low Dimension Scores (Only Relevant Dimensions)
{json.dumps(low_dims, ensure_ascii=False)}

## Available Failure Categories
{json.dumps(FAILURE_CATEGORIES)}

## Instructions
1. Pick the SINGLE most appropriate category from the list above.
2. Write a ONE-sentence key failure reason in Chinese, concise and specific.
3. IMPORTANT: Only analyze failures related to the task's primary dimensions: {primary_dimensions if primary_dimensions else 'all dimensions'}
4. DO NOT mention or analyze dimensions that are not primary to this task (e.g., if communication is not a primary dimension, ignore it).
5. Return ONLY a valid JSON object with exactly these two fields:
   - "category": string (one of the categories above)
   - "key_reason_zh": string (one sentence in Chinese)

Do NOT include any other text or explanation. Only output the JSON."""

    try:
        client = get_llm_client(api_key, base_url)
        resp = client.chat.completions.create(
            model=model_id,
            messages=[{"role": "user", "content": prompt}],
            temperature=0,
            max_tokens=256,
        )
        text = resp.choices[0].message.content.strip()
        if text.startswith("```"):
            text = text.split("\n", 1)[1].rsplit("```", 1)[0].strip()
        return json.loads(text)
    except Exception as e:
        return {"category": "other", "key_reason_zh": f"LLM error: {str(e)[:100]}"}


def process_one_trace(trace_path: str, settings: dict) -> tuple:
    """Process a single trace file and return (filename, report)."""
    trace_filename = os.path.basename(trace_path)
    task_id = resolve_task_id(trace_filename)
    task_info = load_task_info(task_id, settings["tasks_dir"])
    grading, trace_end = load_grading_result(trace_path)

    scores = grading.get("scores", {}) if grading else {}
    passed = grading.get("passed", False) if grading else False
    task_score = grading.get("task_score", 0) if grading else 0
    trial_id = trace_filename.replace(".jsonl", "").rsplit("_", 1)[-1]

    report = {
        "trace_file": trace_filename,
        "task_id": task_id,
        "task_name": task_info.get("task_name", ""),
        "trial_id": trial_id,
        "status": "succ" if passed else "fail",
        "task_score": task_score,
        "scores": scores,
    }

    if not grading:
        report["status"] = "error"
        report["failure_reason"] = [{"message": "No grading_result found in trace"}]
        return trace_filename, report

    if not passed:
        primary_dims = task_info.get("primary_dimensions", [])
        report["failure_reason"] = infer_failure_reason(grading, scores, primary_dims)
        report["task_scoring_components"] = task_info.get("scoring_components", [])
        report["task_judge_rubric"] = task_info.get("judge_rubric", "")[:500]
        report["task_primary_dimensions"] = primary_dims
        if trace_end:
            report["total_turns"] = trace_end.get("total_turns")
            report["wall_time_s"] = trace_end.get("wall_time_s")

        # LLM classification
        classification = llm_classify_failure(
            task_info, grading, trace_end,
            settings["judge_api_key"], settings["judge_base_url"], settings["judge_model_id"],
        )
        report["failure_classification"] = classification

    return trace_filename, report


def main():
    parser = argparse.ArgumentParser(
        description="Generate per-trial JSON reports from batch evaluation results.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--config", "-c", default=None,
                        help="Path to claw-eval config YAML (provides paths + judge model)")
    parser.add_argument("--trace-dir", default=None,
                        help="Directory containing .jsonl trace files")
    parser.add_argument("--tasks-dir", default=None,
                        help="Directory containing task YAML definitions")
    parser.add_argument("--output-dir", "-o", default=None,
                        help="Directory to write per-trial JSON reports")
    parser.add_argument("--judge-model", default=None,
                        help="Override judge model ID (from config YAML)")
    parser.add_argument("--judge-base-url", default=None,
                        help="Override judge API base URL")
    parser.add_argument("--judge-api-key", default=None,
                        help="Override judge API key")
    args = parser.parse_args()

    settings = load_config(args.config, args)

    trace_dir = settings["trace_dir"]
    output_dir = settings["output_dir"]

    # Validate judge config
    if not settings["judge_api_key"]:
        print("Error: judge api_key is not configured. Set it via --config YAML or --judge-api-key.", file=sys.stderr)
        sys.exit(1)
    if not settings["judge_model_id"]:
        print("Error: judge model_id is not configured. Set it via --config YAML or --judge-model.", file=sys.stderr)
        sys.exit(1)

    os.makedirs(output_dir, exist_ok=True)

    trace_files = sorted(glob.glob(os.path.join(trace_dir, "*.jsonl")))
    print(f"Found {len(trace_files)} trace files")

    succ_count = 0
    fail_count = 0
    error_count = 0
    llm_ok = 0
    llm_err = 0

    for i, trace_path in enumerate(trace_files, 1):
        trace_filename, report = process_one_trace(trace_path, settings)

        if report["status"] == "succ":
            succ_count += 1
        elif report["status"] == "fail":
            fail_count += 1
            fc = report.get("failure_classification", {})
            if fc.get("category", "other") != "other" or "key_reason_zh" in fc:
                llm_ok += 1
            else:
                llm_err += 1
        else:
            error_count += 1

        out_name = trace_filename.replace(".jsonl", ".json")
        out_path = os.path.join(output_dir, out_name)
        with open(out_path, "w") as f:
            json.dump(report, f, indent=2, ensure_ascii=False)

        if i % 20 == 0:
            print(f"  Processed {i}/{len(trace_files)} (succ={succ_count}, fail={fail_count}, err={error_count})")

    print(f"\nDone: {succ_count} succ, {fail_count} fail, {error_count} error")
    print(f"LLM classification: {llm_ok} ok, {llm_err} errors")
    print(f"Reports written to: {output_dir}")


if __name__ == "__main__":
    main()
