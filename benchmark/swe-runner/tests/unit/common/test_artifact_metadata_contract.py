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

"""Compatibility contract for generic and adapter-specific artifact metadata."""

from types import MappingProxyType

import pytest
from pydantic import ValidationError

from swe_runner.agents.openclaw.artifacts import OpenClawArtifacts
from swe_runner.run.io.artifacts import RunArtifacts, merge_metadata

ArtifactType = type[RunArtifacts] | type[OpenClawArtifacts]
MODEL_CASES = [(RunArtifacts, "session_id"), (OpenClawArtifacts, "openclaw_profile")]


@pytest.mark.parametrize(("model", "known"), MODEL_CASES)
def test_readonly_metadata_preserves_strings_without_mutating_input(model: ArtifactType, known: str) -> None:
    source = {known: "known-value", "unicode": "中文🙂", "empty_extra": "", "ignored_number": 3, "ignored_bool": False}
    original = dict(source)
    parsed = model.from_metadata(MappingProxyType(source))
    assert type(parsed) is model
    assert parsed.to_metadata() == {"unicode": "中文🙂", "empty_extra": "", known: "known-value"}
    assert list(parsed.to_metadata()) == ["unicode", "empty_extra", known]
    assert source == original


@pytest.mark.parametrize(("model", "known"), MODEL_CASES)
def test_absent_metadata_and_overflow_factories_are_independent(model: ArtifactType, known: str) -> None:
    first = model.from_metadata(None)
    second = model.from_metadata({})
    assert first.to_metadata() == second.to_metadata() == {}
    assert getattr(first, known) is None
    first.extra_metadata["custom"] = "one"
    assert second.extra_metadata == {}


@pytest.mark.parametrize(("model", "known"), MODEL_CASES)
def test_known_values_override_overflow_and_empty_known_values_stay_omitted(model: ArtifactType, known: str) -> None:
    parsed = model(**{known: "actual"}, extra_metadata={known: "older", "custom": "keep"})
    assert parsed.to_metadata() == {known: "actual", "custom": "keep"}
    assert model.from_metadata({known: "", "custom": ""}).to_metadata() == {"custom": ""}


@pytest.mark.parametrize(("model", "known"), MODEL_CASES)
def test_updates_preserve_concrete_type_and_ignore_none(model: ArtifactType, known: str) -> None:
    original = model.from_metadata({known: "original", "custom": "keep"})
    unchanged = original.with_updates(**{known: None})
    updated = original.with_updates(**{known: "updated"})
    assert type(unchanged) is type(updated) is model
    assert unchanged is not original and updated is not original
    assert original.to_metadata() == unchanged.to_metadata() == {"custom": "keep", known: "original"}
    assert updated.to_metadata() == {"custom": "keep", known: "updated"}


@pytest.mark.parametrize(("model", "known"), MODEL_CASES)
def test_pydantic_json_and_validation_contract_are_preserved(model: ArtifactType, known: str) -> None:
    artifact = model.from_metadata({known: "known-value", "extra_metadata": "literal-key", "custom": "keep"})
    restored = model.model_validate_json(artifact.model_dump_json())
    assert restored.to_metadata() == artifact.to_metadata()
    properties = model.model_json_schema()["properties"]
    assert properties["extra_metadata"]["additionalProperties"] == {"type": "string"}
    assert properties[known]["anyOf"] == [{"type": "string"}, {"type": "null"}]
    with pytest.raises(ValidationError):
        model(**{known: 123})


@pytest.mark.parametrize(("model", "known"), MODEL_CASES)
def test_cross_model_roundtrip_keeps_all_persisted_keys(model: ArtifactType, known: str) -> None:
    source = {
        "session_id": "session-1",
        "docker_image_name": "fixture/image",
        "openclaw_profile": "profile-1",
        "openclaw_returncode": "0",
        "openclaw_profile_dir": "/private/中文",
        "custom": "keep",
    }
    first = model.from_metadata(source)
    other = OpenClawArtifacts if model is RunArtifacts else RunArtifacts
    assert other.from_metadata(first.to_metadata()).to_metadata() == source


def test_merge_keeps_cross_model_data_and_existing_last_value_precedence() -> None:
    generic = RunArtifacts.from_metadata({"session_id": "prepared", "custom": "before"})
    adapter = OpenClawArtifacts.from_metadata({"openclaw_profile": "profile", "custom": "after"})
    assert merge_metadata(None, generic.to_metadata(), adapter.to_metadata(), {"ignored": False}) == {
        "session_id": "prepared",
        "openclaw_profile": "profile",
        "custom": "after",
    }
