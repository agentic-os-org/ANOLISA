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

"""One-click analysis: generate trial reports + summary table from a trace directory.

Workflow:
  1. Read batch_results.json from the trace directory
  2. Generate per-trial LLM failure reports (if not already present)
  3. Produce summary tables in both TXT and CSV formats
  4. Save everything under results/<trace-name>/

Usage:
    # Minimal: uses config YAML for judge model, latest trace dir
    python scripts/analyze.py --config claw-eval/config_general.yaml

    # Specify trace and output explicitly
    python scripts/analyze.py \
        --trace-dir claw-eval/traces/qwen3.6-plus_26-04-24-09-36 \
        --output-dir results/

    # Override judge model
    python scripts/analyze.py \
        --trace-dir claw-eval/traces/qwen3.6-plus_26-04-24-09-36 \
        --output-dir results/ \
        --judge-model qwen-plus --judge-api-key sk-xxx
"""

import argparse
import csv
import glob
import io
import json
import os
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
    "search_unavailable",
    "tool_loop",
    "tool_error",
    "safety_violation",
    "incomplete_response",
    "incorrect_execution",
    "timeout_exceeded",
    "refusal_to_answer",
    "other",
]


# ── Config loading ──────────────────────────────────────────────────────────

def load_config(config_path: str | None, cli_args) -> dict:
    """Load settings from config YAML with CLI overrides."""
    defaults = {
        "trace_dir": None,
        "tasks_dir": None,
        "judge_api_key": "",
        "judge_base_url": "",
        "judge_model_id": "",
    }

    if config_path:
        cfg_file = Path(config_path)
        with open(cfg_file) as f:
            cfg = yaml.safe_load(f) or {}

        judge = cfg.get("judge", {})
        dflts = cfg.get("defaults", {})
        claw_eval_dir = cfg_file.parent

        defaults["judge_api_key"] = judge.get("api_key", "")
        defaults["judge_base_url"] = judge.get("base_url", "")
        defaults["judge_model_id"] = judge.get("model_id", "")

        defaults["trace_dir"] = str(claw_eval_dir / dflts.get("trace_dir", "traces"))
        defaults["tasks_dir"] = str(claw_eval_dir / dflts.get("tasks_dir", "tasks"))

    # CLI overrides
    if cli_args.trace_dir:
        defaults["trace_dir"] = cli_args.trace_dir
    if cli_args.tasks_dir:
        defaults["tasks_dir"] = cli_args.tasks_dir
    if cli_args.judge_model:
        defaults["judge_model_id"] = cli_args.judge_model
    if cli_args.judge_base_url:
        defaults["judge_base_url"] = cli_args.judge_base_url
    if cli_args.judge_api_key:
        defaults["judge_api_key"] = cli_args.judge_api_key

    if defaults["trace_dir"] is None:
        defaults["trace_dir"] = str(REPO_DIR / "claw-eval" / "traces")
    if defaults["tasks_dir"] is None:
        defaults["tasks_dir"] = str(REPO_DIR / "claw-eval" / "tasks")

    return defaults


# ── Trial report generation ─────────────────────────────────────────────────

def load_task_info(task_id: str, tasks_dir: str) -> dict:
    yaml_path = os.path.join(tasks_dir, task_id, "task.yaml")
    if not os.path.exists(yaml_path):
        return {"task_id": task_id, "error": "task.yaml not found"}
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


def infer_failure_reason(grading: dict, scores: dict) -> list:
    reasons = []
    for jc in grading.get("judge_calls", []):
        if jc.get("score", 1.0) < 0.5:
            reasons.append({
                "judge_rubric": jc.get("rubric_preview", "")[:80],
                "score": jc.get("score"),
                "reasoning": jc.get("reasoning", ""),
            })
    low_dims = {k: v for k, v in scores.items()
                if isinstance(v, (int, float)) and v < 0.5
                and k not in ("efficiency_turns", "efficiency_tokens", "efficiency_wall_time_s")}
    if low_dims:
        reasons.append({"low_dimension_scores": low_dims})
    reasons.append({"task_score": grading.get("task_score", 0)})
    return reasons


def llm_classify_failure(task_info: dict, grading: dict, trace_end: dict,
                         api_key: str, base_url: str, model_id: str) -> dict:
    scores = grading.get("scores", {})
    task_score = grading.get("task_score", 0)

    judge_reasons = []
    for jc in grading.get("judge_calls", []):
        if jc.get("score", 1.0) < 0.5:
            judge_reasons.append({
                "rubric": jc.get("rubric_preview", "")[:100],
                "score": jc.get("score"),
                "reasoning": jc.get("reasoning", ""),
            })

    low_dims = {k: v for k, v in scores.items()
                if isinstance(v, (int, float)) and v < 0.5
                and k not in ("efficiency_turns", "efficiency_tokens", "efficiency_wall_time_s")}

    wall_time = trace_end.get("wall_time_s", 0) if trace_end else 0
    total_turns = trace_end.get("total_turns", 0) if trace_end else 0

    prompt = f"""You are analyzing why an AI agent trial failed. Provide a failure classification and a one-sentence key reason.

## Task Context
- Task: {task_info.get('task_id', '')} ({task_info.get('task_name', '')})
- Category: {task_info.get('category', '')}
- Prompt: {task_info.get('prompt', '')[:200]}
- Scoring rubric: {task_info.get('judge_rubric', '')[:200]}

## Trial Results
- Task score: {task_score}
- Dimension scores: {json.dumps(scores, ensure_ascii=False)}
- Total turns: {total_turns}
- Wall time: {wall_time:.0f}s

## Judge Reasoning for Low Scores
{json.dumps(judge_reasons, ensure_ascii=False, indent=2)}

## Low Dimension Scores
{json.dumps(low_dims, ensure_ascii=False)}

## Available Failure Categories
{json.dumps(FAILURE_CATEGORIES)}

## Instructions
1. Pick the SINGLE most appropriate category from the list above.
2. Write a ONE-sentence key failure reason in Chinese, concise and specific.
3. Return ONLY a valid JSON object with exactly these two fields:
   - "category": string (one of the categories above)
   - "key_reason_zh": string (one sentence in Chinese)

Do NOT include any other text or explanation. Only output the JSON."""

    try:
        client = OpenAI(api_key=api_key, base_url=base_url)
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


def resolve_task_id(trace_filename: str) -> str:
    base = trace_filename.replace(".jsonl", "")
    parts = base.rsplit("_", 1)
    return parts[0] if len(parts) == 2 else base


def generate_reports(trace_dir: str, tasks_dir: str, report_dir: str,
                     judge_api_key: str, judge_base_url: str, judge_model_id: str):
    """Generate per-trial JSON reports. Returns count of processed files."""
    trace_files = sorted(glob.glob(os.path.join(trace_dir, "*.jsonl")))
    if not trace_files:
        print(f"No .jsonl trace files found in: {trace_dir}", file=sys.stderr)
        return 0

    os.makedirs(report_dir, exist_ok=True)

    succ_count = 0
    fail_count = 0
    error_count = 0

    for i, trace_path in enumerate(trace_files, 1):
        trace_filename = os.path.basename(trace_path)
        task_id = resolve_task_id(trace_filename)
        task_info = load_task_info(task_id, tasks_dir)
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
        elif not passed:
            report["failure_reason"] = infer_failure_reason(grading, scores)
            report["task_scoring_components"] = task_info.get("scoring_components", [])
            report["task_judge_rubric"] = task_info.get("judge_rubric", "")[:500]
            report["task_primary_dimensions"] = task_info.get("primary_dimensions", [])
            if trace_end:
                report["total_turns"] = trace_end.get("total_turns")
                report["wall_time_s"] = trace_end.get("wall_time_s")
            classification = llm_classify_failure(
                task_info, grading, trace_end,
                judge_api_key, judge_base_url, judge_model_id,
            )
            report["failure_classification"] = classification

        out_path = os.path.join(report_dir, trace_filename.replace(".jsonl", ".json"))
        with open(out_path, "w") as f:
            json.dump(report, f, indent=2, ensure_ascii=False)

        if report["status"] == "succ":
            succ_count += 1
        elif report["status"] == "fail":
            fail_count += 1
        else:
            error_count += 1

        if i % 20 == 0 or i == len(trace_files):
            print(f"  [{i}/{len(trace_files)}] succ={succ_count}, fail={fail_count}, err={error_count}")

    print(f"Reports generated: {report_dir}")
    return len(trace_files)


# ── Summary table ───────────────────────────────────────────────────────────

def fmt(val):
    if val is None:
        return "N/A"
    if isinstance(val, bool):
        return "Y" if val else "N"
    if isinstance(val, float):
        if val == int(val) and abs(val) < 1e6:
            return str(int(val))
        return f"{val:.2f}"
    return str(val)


def extract_failure_info(report: dict) -> str:
    if not report:
        return "N/A"
    if report.get("status") == "succ":
        return "-"
    fc = report.get("failure_classification", {})
    category = fc.get("category", "")
    reason = fc.get("key_reason_zh", "")
    if category and reason:
        result = f"{category} | {reason}"
    elif category:
        result = category
    elif reason:
        result = reason
    else:
        reasons = report.get("failure_reason", [])
        if reasons:
            snippets = []
            for r in reasons:
                if isinstance(r, dict):
                    msg = r.get("reasoning", r.get("message", ""))
                    if msg:
                        snippets.append(str(msg)[:80])
            result = "; ".join(snippets[:2]) if snippets else "unknown"
        else:
            return "unknown"
    # Replace commas with semicolons to avoid CSV parsing issues
    return result.replace(",", ";").replace("，", "；")


def extract_trial_hash(trace_field: str) -> str:
    base = os.path.basename(trace_field).replace(".jsonl", "")
    parts = base.rsplit("_", 1)
    return parts[1] if len(parts) == 2 else ""


def build_summary_table(data: list, reports: dict) -> tuple:
    sorted_data = sorted(data, key=lambda t: t["task_id"])
    has_reports = bool(reports)

    trial_cols = [
        "Trial", "Trial ID", "Input Toks", "Output Toks",
        "Model Time(s)", "Tool Time(s)", "Other Time(s)", "Wall Time(s)",
        "Completion", "Robustness", "Communication", "Safety",
        "Task Score", "Passed",
    ]
    header = ["Task ID", "Task Name", "Difficulty"] + trial_cols + ["Avg Score", "Pass@1", "PassHatK", "Overall"]
    if has_reports:
        header.append("Failure Reason")

    rows = [header]
    for task in sorted_data:
        task_id = task["task_id"]
        task_name = task["task_name"]
        difficulty = task["difficulty"]
        avg_score = task.get("avg_score")
        pass_at_1 = task.get("pass_at_1")
        pass_hat_k = task.get("pass_hat_k")
        avg_passed = task.get("avg_passed")

        for i, trial in enumerate(task.get("trials", []), 1):
            trace_field = trial.get("trace", "")
            trace_basename = os.path.basename(trace_field) if trace_field else ""
            trial_hash = extract_trial_hash(trace_field)
            report = reports.get(trace_basename) if reports else None
            failure_info = extract_failure_info(report) if has_reports else ""

            row = [
                task_id if i == 1 else "",
                task_name if i == 1 else "",
                difficulty if i == 1 else "",
                f"#{i}",
                trial_hash,
                fmt(trial.get("input_tokens")),
                fmt(trial.get("output_tokens")),
                fmt(trial.get("model_time_s")),
                fmt(trial.get("tool_time_s")),
                fmt(trial.get("other_time_s")),
                fmt(trial.get("wall_time_s")),
                fmt(trial.get("completion")),
                fmt(trial.get("robustness")),
                fmt(trial.get("communication")),
                fmt(trial.get("safety")),
                fmt(trial.get("task_score")),
                fmt(trial.get("passed")),
                fmt(avg_score) if i == 1 else "",
                fmt(pass_at_1) if i == 1 else "",
                fmt(pass_hat_k) if i == 1 else "",
                fmt(avg_passed) if i == 1 else "",
            ]
            if has_reports:
                row.append(failure_info)
            rows.append(row)

    return rows


def render_table(rows: list) -> str:
    widths = [0] * len(rows[0])
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(str(cell)))
    sep = "+" + "+".join("-" * (w + 2) for w in widths) + "+"

    def format_row(row):
        return "|" + "|".join(f" {str(cell):<{widths[i]}} " for i, cell in enumerate(row)) + "|"

    lines = [sep, format_row(rows[0]), sep]
    for row in rows[1:]:
        lines.append(format_row(row))
        lines.append(sep)
    return "\n".join(lines)


def render_csv(rows: list) -> str:
    buf = io.StringIO()
    writer = csv.writer(buf)
    for row in rows:
        writer.writerow(row)
    return buf.getvalue()


def load_reports(report_dir: str) -> dict:
    reports = {}
    rd = Path(report_dir)
    if not rd.is_dir():
        return reports
    for rp in rd.glob("*.json"):
        try:
            with open(rp) as f:
                report = json.load(f)
            trace_file = report.get("trace_file", "")
            if trace_file:
                reports[trace_file] = report
        except (json.JSONDecodeError, KeyError):
            continue
    return reports


def build_structured_data(data: list, reports: dict, trace_name: str) -> dict:
    """Build structured JSON for AI consumption."""
    sorted_data = sorted(data, key=lambda t: t["task_id"])

    total_tasks = len(data)
    passed_tasks = sum(1 for t in data if t.get("avg_passed"))
    total_trials = sum(len(t.get("trials", [])) for t in data)
    passed_trials = sum(
        sum(1 for tr in t.get("trials", []) if tr.get("passed"))
        for t in data
    )

    # Aggregate token/time stats from batch data
    total_input = sum(tr.get("input_tokens", 0) for t in data for tr in t.get("trials", []))
    total_output = sum(tr.get("output_tokens", 0) for t in data for tr in t.get("trials", []))
    total_time = sum(tr.get("wall_time_s", 0) for t in data for tr in t.get("trials", []))

    # Build task list
    tasks = []
    failure_dist = {}

    for task in sorted_data:
        task_entry = {
            "task_id": task["task_id"],
            "task_name": task["task_name"],
            "difficulty": task["difficulty"],
            "avg_score": task.get("avg_score"),
            "pass_at_1": task.get("pass_at_1"),
            "pass_hat_k": task.get("pass_hat_k"),
            "overall_passed": task.get("avg_passed"),
            "trials": [],
        }

        for trial in task.get("trials", []):
            trace_field = trial.get("trace", "")
            trace_basename = os.path.basename(trace_field)
            trial_hash = extract_trial_hash(trace_field)
            report = reports.get(trace_basename) if reports else None

            trial_entry = {
                "trial_id": trial_hash,
                "input_tokens": trial.get("input_tokens"),
                "output_tokens": trial.get("output_tokens"),
                "model_time_s": trial.get("model_time_s"),
                "tool_time_s": trial.get("tool_time_s"),
                "other_time_s": trial.get("other_time_s"),
                "wall_time_s": trial.get("wall_time_s"),
                "completion": trial.get("completion"),
                "robustness": trial.get("robustness"),
                "communication": trial.get("communication"),
                "safety": trial.get("safety"),
                "task_score": trial.get("task_score"),
                "passed": trial.get("passed"),
            }

            if report and report.get("status") != "succ":
                fc = report.get("failure_classification", {})
                cat = fc.get("category", "")
                reason = fc.get("key_reason_zh", "")
                # Replace commas for consistency
                if reason:
                    reason = reason.replace(",", ";").replace("，", "；")
                if cat:
                    trial_entry["failure_category"] = cat
                    failure_dist[cat] = failure_dist.get(cat, 0) + 1
                if reason:
                    trial_entry["failure_reason"] = reason
            elif report and report.get("status") == "succ":
                trial_entry["failure_category"] = None
                trial_entry["failure_reason"] = None

            task_entry["trials"].append(trial_entry)

        tasks.append(task_entry)

    return {
        "trace_name": trace_name,
        "summary": {
            "total_tasks": total_tasks,
            "passed_tasks": passed_tasks,
            "pass_rate": round(passed_tasks / total_tasks * 100, 1) if total_tasks else 0,
            "total_trials": total_trials,
            "passed_trials": passed_trials,
            "trial_pass_rate": round(passed_trials / total_trials * 100, 1) if total_trials else 0,
            "total_input_tokens": total_input,
            "total_output_tokens": total_output,
            "total_tokens": total_input + total_output,
            "total_time_s": round(total_time, 2),
        },
        "tasks": tasks,
        "failure_distribution": dict(sorted(failure_dist.items(), key=lambda x: -x[1])),
    }


# ── Main orchestration ──────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="One-click analysis: generate trial reports + summary table from trace directory.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Use config YAML for judge settings, latest trace dir
  python scripts/analyze.py --config claw-eval/config_general.yaml

  # Specify trace and output dirs
  python scripts/analyze.py \\
      --trace-dir claw-eval/traces/qwen3.6-plus_26-04-24-09-36 \\
      --output-dir results/

  # Override judge model
  python scripts/analyze.py \\
      --trace-dir claw-eval/traces/qwen3.6-plus_26-04-24-09-36 \\
      --output-dir results/ \\
      --judge-model qwen-plus --judge-api-key sk-xxx
        """,
    )
    parser.add_argument("--config", "-c", default=None,
                        help="Path to claw-eval config YAML (provides judge model settings)")
    parser.add_argument("--trace-dir", "-t", default=None,
                        help="Directory containing .jsonl trace files and batch_results.json")
    parser.add_argument("--tasks-dir", default=None,
                        help="Directory containing task YAML definitions")
    parser.add_argument("--output-dir", "-o", default=None,
                        help="Output base directory (results saved under <output-dir>/<trace-name>/)")
    parser.add_argument("--judge-model", default=None,
                        help="Override judge model ID")
    parser.add_argument("--judge-base-url", default=None,
                        help="Override judge API base URL")
    parser.add_argument("--judge-api-key", default=None,
                        help="Override judge API key")
    args = parser.parse_args()

    settings = load_config(args.config, args)

    # Resolve trace dir
    trace_dir = settings["trace_dir"]
    if args.trace_dir:
        trace_dir = args.trace_dir
    elif settings["trace_dir"] and os.path.isdir(settings["trace_dir"]):
        # If trace_dir points to the parent "traces" dir, find latest
        trace_dirs = [d for d in Path(settings["trace_dir"]).iterdir() if d.is_dir()]
        if trace_dirs:
            trace_dir = str(max(trace_dirs, key=lambda d: d.stat().st_mtime))
            print(f"Using latest trace dir: {trace_dir}", file=sys.stderr)

    if not os.path.isdir(trace_dir):
        print(f"Error: trace directory not found: {trace_dir}", file=sys.stderr)
        sys.exit(1)

    # Validate judge config
    if not settings["judge_api_key"]:
        print("Error: judge api_key not configured. Use --config or --judge-api-key.", file=sys.stderr)
        sys.exit(1)
    if not settings["judge_model_id"]:
        print("Error: judge model_id not configured. Use --config or --judge-model.", file=sys.stderr)
        sys.exit(1)

    # Setup output directory: use trace directory name under output_base
    output_base = args.output_dir or "results"
    trace_name = os.path.basename(os.path.normpath(trace_dir))
    output_dir = os.path.join(output_base, trace_name)

    # Step 1: Check for existing reports in output_base (from previous runs)
    skip_generation = False
    existing_runs = [d for d in Path(output_base).iterdir() if d.is_dir()] if Path(output_base).is_dir() else []
    if existing_runs:
        latest_run = max(existing_runs, key=lambda d: d.stat().st_mtime)
        existing_reports = list((latest_run / "reports").glob("*.json"))
        if len(existing_reports) > 0:
            report_dir = str(latest_run / "reports")
            skip_generation = True
            print(f"Found {len(existing_reports)} existing reports, skipping generation.", file=sys.stderr)

    if not skip_generation:
        report_dir = os.path.join(output_dir, "reports")
        os.makedirs(report_dir, exist_ok=True)
        print(f"Step 1: Generating trial reports...", file=sys.stderr)
        generate_reports(
            trace_dir=trace_dir,
            tasks_dir=settings["tasks_dir"],
            report_dir=report_dir,
            judge_api_key=settings["judge_api_key"],
            judge_base_url=settings["judge_base_url"],
            judge_model_id=settings["judge_model_id"],
        )
    else:
        os.makedirs(output_dir, exist_ok=True)

    # Step 2: Load batch results
    batch_results_path = os.path.join(trace_dir, "batch_results.json")
    if not os.path.exists(batch_results_path):
        print(f"Error: batch_results.json not found in: {trace_dir}", file=sys.stderr)
        sys.exit(1)

    print(f"Step 2: Reading {batch_results_path}", file=sys.stderr)
    with open(batch_results_path) as f:
        data = json.load(f)

    # Step 3: Load reports and build summary
    print(f"Step 3: Building summary table...", file=sys.stderr)
    reports = load_reports(report_dir)
    rows = build_summary_table(data, reports)

    # Render output — always generate both txt and csv
    table_txt = render_table(rows)
    total_tasks = len(data)
    passed_tasks = sum(1 for t in data if t.get("avg_passed"))
    total_trials = sum(len(t.get("trials", [])) for t in data)
    passed_trials = sum(
        sum(1 for tr in t.get("trials", []) if tr.get("passed"))
        for t in data
    )
    summary_text = (
        table_txt
        + f"\n\nSummary: {total_tasks} tasks, {passed_tasks} passed ({passed_tasks/total_tasks*100:.1f}%)\n"
        + f"Trials: {total_trials} total, {passed_trials} passed ({passed_trials/total_trials*100:.1f}%)\n"
    )
    summary_csv = render_csv(rows)

    # Build structured JSON for AI consumption
    structured = build_structured_data(data, reports, trace_name)

    # Save all formats
    txt_path = os.path.join(output_dir, "summary.txt")
    csv_path = os.path.join(output_dir, "summary.csv")
    json_path = os.path.join(output_dir, "report.json")
    with open(txt_path, "w") as f:
        f.write(summary_text)
    with open(csv_path, "w") as f:
        f.write(summary_csv)
    with open(json_path, "w") as f:
        json.dump(structured, f, indent=2, ensure_ascii=False)

    # Save batch_summary.json copy
    batch_summary_src = os.path.join(trace_dir, "batch_summary.json")
    if os.path.exists(batch_summary_src):
        import shutil
        shutil.copy2(batch_summary_src, os.path.join(output_dir, "batch_summary.json"))

    print(f"\nResults saved to: {output_dir}/")
    print(f"  Summary:  {txt_path}")
    print(f"            {csv_path}")
    print(f"  Report:   {json_path}")
    print(f"  Reports:  {report_dir}/ ({len(reports)} files)")
    print()
    print(summary_text)


if __name__ == "__main__":
    main()
