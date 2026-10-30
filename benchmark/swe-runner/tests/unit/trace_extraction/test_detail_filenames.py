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

import csv
import json
from hashlib import sha256
from pathlib import Path

import pytest

from swe_runner.trace_extraction.export import write_trace_analysis_csvs


def _traces(root: Path, names: list[str]) -> None:
    for index, name in enumerate(names):
        directory = root / name
        directory.mkdir(parents=True)
        (directory / "trace1.json").write_text(
            json.dumps({"session_id": name, "total_input_tokens": index + 1}), encoding="utf-8"
        )


def _details(output: Path) -> dict[str, list[dict[str, str]]]:
    result = {}
    for path in (output / "trace_details").glob("*.csv"):
        with path.open(encoding="utf-8", newline="") as source:
            result[path.name] = list(csv.DictReader(source))
    return result


@pytest.mark.parametrize("names", [["case one", "case_one"], ["甲乙", "丙丁"], ["Case_1", "case_1"]])
def test_distinct_case_details_survive_filename_collisions(tmp_path: Path, names: list[str]) -> None:
    root = tmp_path / "traces"
    output = tmp_path / "report"
    _traces(root, names)
    write_trace_analysis_csvs(root, output)
    details = _details(output)
    assert len(details) == len(names)
    assert sorted(rows[0]["用例ID"] for rows in details.values()) == sorted(names)
    assert len({name.casefold() for name in details}) == len(names)
    assert sorted(int(rows[0]["总输入Token数"]) for rows in details.values()) == [1, 2]


def test_allocation_is_stable_across_exports(tmp_path: Path) -> None:
    root = tmp_path / "traces"
    output = tmp_path / "report"
    _traces(root, ["case one", "case_one", "ordinary-case"])
    write_trace_analysis_csvs(root, output)
    first = _details(output)
    write_trace_analysis_csvs(root, output)
    assert _details(output) == first
    assert len(first) == 3
    assert "ordinary-case.csv" in first


def test_generated_names_do_not_overwrite_an_ordinary_name(tmp_path: Path) -> None:
    digest = sha256("case one".encode()).hexdigest()[:12]
    ordinary = f"case_one-{digest}"
    root = tmp_path / "traces"
    output = tmp_path / "report"
    names = ["case one", "case_one", ordinary]
    _traces(root, names)
    write_trace_analysis_csvs(root, output)
    details = _details(output)
    assert len(details) == 3
    assert details[f"{ordinary}.csv"][0]["用例ID"] == ordinary
    assert sorted(rows[0]["用例ID"] for rows in details.values()) == sorted(names)
