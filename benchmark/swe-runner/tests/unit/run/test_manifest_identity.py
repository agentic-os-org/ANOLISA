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

"""Protect per-instance provenance when display components alias."""

from __future__ import annotations

import json
from types import SimpleNamespace

import pytest

from swe_runner.run.io.input_manifest import write_input_manifest


@pytest.fixture(autouse=True)
def isolated_payload(monkeypatch):
    monkeypatch.setattr(
        "swe_runner.run.io.input_manifest.build_input_manifest",
        lambda **kwargs: {
            "instance_id": kwargs["prepared"].instance.instance_id,
            "manifest_path": str(kwargs["manifest_path"]),
        },
    )


def write(output, identity):
    prepared = SimpleNamespace(instance=SimpleNamespace(instance_id=identity))
    return write_input_manifest(output, agent_name="fixture", prepared=prepared)


@pytest.mark.parametrize(
    "first,second", [("fixture", "fixture_"), ("fixture", "_fixture"), ("a b", "a-b"), ("", "instance")]
)
def test_distinct_ids_retain_both_actual_manifest_payloads(tmp_path, first, second):
    before = write(tmp_path, first)
    after = write(tmp_path, second)
    assert before != after
    assert json.loads(before.read_text(encoding="utf-8"))["instance_id"] == first
    assert json.loads(after.read_text(encoding="utf-8"))["instance_id"] == second
    assert before.is_relative_to(tmp_path) and after.is_relative_to(tmp_path)


def test_case_distinctions_do_not_alias_on_case_insensitive_paths(tmp_path):
    first = write(tmp_path, "Fixture")
    second = write(tmp_path, "fixture")
    assert first.parent.name.casefold() != second.parent.name.casefold()


def test_long_unicode_id_uses_a_bounded_component_without_losing_identity(tmp_path):
    identity = "示例" * 150
    path = write(tmp_path, identity)
    assert len(path.parent.name.encode("utf-8")) <= 200
    assert json.loads(path.read_text(encoding="utf-8"))["instance_id"] == identity


def test_repeat_same_id_reuses_its_own_path(tmp_path):
    first = write(tmp_path, "fixture")
    second = write(tmp_path, "fixture")
    assert first == second
    assert len(list(tmp_path.rglob("input_manifest.json"))) == 1
