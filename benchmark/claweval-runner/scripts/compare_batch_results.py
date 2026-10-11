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

"""Compare recorded CE task outcomes without running agents or grading."""

import argparse
import csv
import json
import math
import os
import sys
import tempfile
from dataclasses import dataclass
from decimal import Context, Decimal, localcontext
from pathlib import Path
from typing import Any, TextIO

COLUMNS = (
    "task_id",
    "outcome_change",
    "baseline_status",
    "candidate_status",
    "baseline_score",
    "candidate_score",
    "score_delta",
    "baseline_trials",
    "candidate_trials",
    "baseline_error_trials",
    "candidate_error_trials",
)


class BatchComparisonError(ValueError):
    """Recorded batch inputs cannot produce a valid comparison."""


@dataclass(frozen=True)
class Outcome:
    status: str
    score: Decimal | None
    trials: int | None
    error_trials: int | None


def _score(value: Any) -> Decimal | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ValueError("avg_score must be a number or null")
    try:
        number = float(value)
    except OverflowError:
        raise ValueError("avg_score must be finite and nonnegative") from None
    if not math.isfinite(number) or number < 0:
        raise ValueError("avg_score must be finite and nonnegative")
    return Decimal(str(value))


def load_batch(path: Path) -> dict[str, Outcome]:
    """Validate and project recorded aggregates without recomputing grades."""
    try:
        data = json.loads(path.read_text(encoding="utf-8-sig"))
        if not isinstance(data, list):
            raise ValueError("batch root must be a JSON array")
        outcomes = {}
        for item in data:
            if not isinstance(item, dict):
                raise ValueError("task records must be JSON objects")
            task_id = item.get("task_id")
            if not isinstance(task_id, str) or not task_id.strip():
                raise ValueError("task_id must be a non-empty string")
            task_id.encode("utf-8")
            if task_id in outcomes:
                raise ValueError(f"duplicate task_id: {task_id}")
            passed = item.get("avg_passed")
            if passed is not None and not isinstance(passed, bool):
                raise ValueError(f"avg_passed must be a boolean or null: {task_id}")
            error = item.get("error")
            if error is not None and not isinstance(error, str):
                raise ValueError(f"error must be a string or null: {task_id}")
            status = (
                "error"
                if error
                else "unknown"
                if passed is None
                else "pass"
                if passed
                else "fail"
            )
            trial_data = item.get("trials")
            trials = error_trials = None
            if trial_data is not None:
                if not isinstance(trial_data, list) or any(
                    not isinstance(trial, dict) for trial in trial_data
                ):
                    raise ValueError(
                        f"trials must be an array of objects or null: {task_id}"
                    )
                trials = len(trial_data)
                error_trials = sum(bool(trial.get("error")) for trial in trial_data)
            outcomes[task_id] = Outcome(
                status, _score(item.get("avg_score")), trials, error_trials
            )
        return outcomes
    except (OSError, ValueError) as error:
        raise BatchComparisonError(f"Invalid batch input {path}: {error}") from error


def _number(value: Decimal | int | None) -> str:
    if value is None:
        return ""
    if isinstance(value, int):
        return str(value)
    if value == 0:
        return "0"
    with localcontext(Context(prec=700)):
        formatted = format(value.normalize(), ".12g")
    mantissa, marker, exponent = formatted.partition("e")
    if "." in mantissa:
        mantissa = mantissa.rstrip("0").rstrip(".")
    return mantissa + marker + exponent


def comparison_rows(
    baseline: dict[str, Outcome], candidate: dict[str, Outcome]
) -> list[dict[str, str]]:
    """Join exact task IDs; score changes remain separate from pass transitions."""
    rows = []
    for task_id in sorted(baseline.keys() | candidate.keys()):
        before, after = baseline.get(task_id), candidate.get(task_id)
        left, right = (
            before.status if before else "missing",
            after.status if after else "missing",
        )
        if before is None:
            change = "added"
        elif after is None:
            change = "removed"
        elif "unknown" in (left, right):
            change = "unknown"
        elif left == "pass" and right != "pass":
            change = "regressed"
        elif left != "pass" and right == "pass":
            change = "improved"
        else:
            change = "unchanged" if left == right else "changed"
        delta = None
        if (
            before is not None
            and after is not None
            and before.score is not None
            and after.score is not None
        ):
            # Float-range input exponents bound the exact difference precision.
            with localcontext(Context(prec=700)):
                delta = after.score - before.score
        rows.append(
            {
                "task_id": task_id,
                "outcome_change": change,
                "baseline_status": left,
                "candidate_status": right,
                "baseline_score": _number(before.score if before else None),
                "candidate_score": _number(after.score if after else None),
                "score_delta": _number(delta),
                "baseline_trials": _number(before.trials if before else None),
                "candidate_trials": _number(after.trials if after else None),
                "baseline_error_trials": _number(
                    before.error_trials if before else None
                ),
                "candidate_error_trials": _number(
                    after.error_trials if after else None
                ),
            }
        )
    return rows


def _write_rows(stream: TextIO, rows: list[dict[str, str]]) -> None:
    writer = csv.DictWriter(stream, fieldnames=COLUMNS)
    writer.writeheader()
    writer.writerows(rows)


def write_comparison(
    output: Path, sources: tuple[Path, Path], rows: list[dict[str, str]]
) -> None:
    """Publish a complete UTF-8 comparison through an owned temporary file."""
    try:
        aliases_source = any(
            output.resolve() == source.resolve()
            or (output.exists() and output.samefile(source))
            for source in sources
        )
    except (OSError, RuntimeError) as error:
        raise BatchComparisonError(
            f"Cannot resolve output path {output}: {error}"
        ) from error
    if aliases_source:
        raise BatchComparisonError(f"Output aliases a batch input: {output}")
    output.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            newline="",
            dir=output.parent,
            prefix=".batch-comparison-",
            suffix=".tmp",
            delete=False,
        ) as stream:
            temporary = Path(stream.name)
            _write_rows(stream, rows)
        os.replace(temporary, output)
    finally:
        if temporary is not None:
            try:
                temporary.unlink(missing_ok=True)
            except OSError as error:
                print(
                    f"Warning: cannot remove temporary comparison {temporary}: {error}",
                    file=sys.stderr,
                )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--baseline", type=Path, required=True, help="Baseline batch_results.json"
    )
    parser.add_argument(
        "--candidate", type=Path, required=True, help="Candidate batch_results.json"
    )
    parser.add_argument(
        "--output", "-o", type=Path, help="Optional CSV path; default: UTF-8 stdout"
    )
    args = parser.parse_args(argv)
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", newline="")
    try:
        rows = comparison_rows(load_batch(args.baseline), load_batch(args.candidate))
        if args.output is None:
            _write_rows(sys.stdout, rows)
        else:
            write_comparison(args.output, (args.baseline, args.candidate), rows)
    except (BatchComparisonError, OSError) as error:
        print(f"Cannot compare batches: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
