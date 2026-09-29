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

"""Contract tests for agent adapters and registry."""

import os
import subprocess
import sys
from collections.abc import Mapping
from pathlib import Path
from unittest.mock import patch

import pytest

from swe_runner.agents import (
    _ADAPTER_MODULES,
    ADAPTER_REGISTRY,
    AGENT_DESCRIPTOR_REGISTRY,
    AgentAdapter,
    AgentDescriptor,
    AgentError,
    AgentNotFoundError,
    AgentStepLimitError,
    AgentTimeoutError,
    PreparedAgentRun,
    agent_supports_run_option,
    get_agent,
    register_agent,
    register_agent_descriptor,
)
from swe_runner.common.models import AgentConfig, AgentResult, Settings, SWEInstance


class FakeAdapter(AgentAdapter):
    """Fake adapter for testing purposes."""

    def __init__(self, **kwargs):
        self._kwargs = kwargs

    @property
    def name(self) -> str:
        return "fake"

    def prepare(self, instance: SWEInstance, settings: Settings) -> PreparedAgentRun:
        return PreparedAgentRun(
            instance=instance,
            settings=settings,
            work_dir=Path("/tmp"),
            prompt=f"Fake prompt for {instance.instance_id}",
            timeout=settings.agent.timeout,
            max_turns=settings.agent.step_limit,
        )

    def run(self, prepared: PreparedAgentRun) -> AgentResult:
        return AgentResult(
            raw_output=f"Fake response to: {prepared.prompt[:50]}",
            patch=None,
            success=True,
            duration_seconds=0.1,
        )

    def post(self, prepared: PreparedAgentRun, agent_result: AgentResult, output_dir: Path):
        raise NotImplementedError


def test_fake_adapter_implements_base_class():
    """Verify FakeAdapter satisfies AgentAdapter."""
    adapter = FakeAdapter()
    assert adapter.name == "fake"
    assert isinstance(adapter, AgentAdapter)


def test_fake_adapter_returns_agent_result():
    """Verify FakeAdapter.run() returns AgentResult with correct fields."""
    agent = FakeAdapter()
    instance = SWEInstance(
        instance_id="case-1",
        repo="example/repo",
        version="1",
        base_commit="abc123",
        problem_statement="Fix it",
        patch="",
        test_patch="",
    )
    prepared = agent.prepare(instance, Settings(agent=AgentConfig(name="fake", timeout=100)))
    result = agent.run(prepared)

    assert isinstance(result, AgentResult)
    assert result.raw_output == "Fake response to: Fake prompt for case-1"
    assert result.patch is None
    assert result.success is True
    assert result.duration_seconds == 0.1


def test_registry_register_and_get():
    """Verify register_agent and get_agent work correctly."""
    # Clear registry for this test
    original_registry = ADAPTER_REGISTRY.copy()
    ADAPTER_REGISTRY.clear()

    try:
        register_agent("fake", FakeAdapter)
        agent = get_agent("fake")

        assert agent.name == "fake"
        assert isinstance(agent, FakeAdapter)
    finally:
        ADAPTER_REGISTRY.clear()
        ADAPTER_REGISTRY.update(original_registry)


def test_agent_descriptor_merges_capabilities():
    """Verify descriptor registration keeps agent capabilities together."""
    original_descriptors = AGENT_DESCRIPTOR_REGISTRY.copy()
    original_registry = ADAPTER_REGISTRY.copy()
    original_adapter_modules = _ADAPTER_MODULES.copy()
    AGENT_DESCRIPTOR_REGISTRY.clear()
    ADAPTER_REGISTRY.clear()
    _ADAPTER_MODULES.clear()

    def metadata_collector(metadata: Mapping[str, object]) -> object:
        return {"metadata": dict(metadata)}

    try:
        register_agent_descriptor(
            AgentDescriptor(
                name="fake",
                adapter_module="fake.adapter",
                metadata_collectors=(metadata_collector,),
                supported_run_options=("tokenless",),
            )
        )
        register_agent_descriptor(AgentDescriptor(name="fake", adapter_cls=FakeAdapter))

        descriptor = AGENT_DESCRIPTOR_REGISTRY["fake"]
        assert descriptor.adapter_module == "fake.adapter"
        assert descriptor.adapter_cls is FakeAdapter
        assert descriptor.metadata_collectors == (metadata_collector,)
        assert descriptor.supported_run_options == ("tokenless",)
        assert agent_supports_run_option("fake", "tokenless") is True
        assert agent_supports_run_option("fake", "unknown-option") is False
        assert ADAPTER_REGISTRY["fake"] is FakeAdapter
        assert _ADAPTER_MODULES["fake"] == "fake.adapter"
    finally:
        AGENT_DESCRIPTOR_REGISTRY.clear()
        AGENT_DESCRIPTOR_REGISTRY.update(original_descriptors)
        ADAPTER_REGISTRY.clear()
        ADAPTER_REGISTRY.update(original_registry)
        _ADAPTER_MODULES.clear()
        _ADAPTER_MODULES.update(original_adapter_modules)


def test_registry_get_unknown_raises():
    """Verify get_agent raises KeyError for unknown agent."""
    original_registry = ADAPTER_REGISTRY.copy()
    ADAPTER_REGISTRY.clear()

    try:
        with pytest.raises(KeyError) as exc_info:
            get_agent("nonexistent")

        assert "Unknown agent 'nonexistent'" in str(exc_info.value)
    finally:
        ADAPTER_REGISTRY.clear()
        ADAPTER_REGISTRY.update(original_registry)


def test_registry_empty_with_no_lazy_modules_lists_none():
    """Verify get_agent shows '(none)' when registry and lazy modules are both empty."""
    original_registry = ADAPTER_REGISTRY.copy()
    ADAPTER_REGISTRY.clear()

    try:
        with patch.dict(_ADAPTER_MODULES, {}, clear=True):
            with pytest.raises(KeyError) as exc_info:
                get_agent("x")

            assert "(none)" in str(exc_info.value)
    finally:
        ADAPTER_REGISTRY.clear()
        ADAPTER_REGISTRY.update(original_registry)


def test_registry_unknown_shows_lazy_agents_as_available():
    """Verify error message lists lazy-loadable agents even when registry is empty."""
    original_registry = ADAPTER_REGISTRY.copy()
    ADAPTER_REGISTRY.clear()

    try:
        with pytest.raises(KeyError) as exc_info:
            get_agent("nonexistent")

        msg = str(exc_info.value)
        # Lazy-loadable agents should appear in the available list
        assert "cosh" in msg
        assert "openclaw" in msg
    finally:
        ADAPTER_REGISTRY.clear()
        ADAPTER_REGISTRY.update(original_registry)


def test_agent_not_found_error():
    """Verify AgentNotFoundError inherits from AgentError."""
    assert issubclass(AgentNotFoundError, AgentError)
    # Can instantiate
    err = AgentNotFoundError("agent not found")
    assert isinstance(err, AgentError)
    assert str(err) == "agent not found"


def test_agent_timeout_error():
    """Verify AgentTimeoutError inherits from AgentError."""
    assert issubclass(AgentTimeoutError, AgentError)
    # Can instantiate
    err = AgentTimeoutError("timeout exceeded")
    assert isinstance(err, AgentError)
    assert str(err) == "timeout exceeded"


def test_agent_step_limit_error():
    """Verify AgentStepLimitError inherits from AgentError."""
    assert issubclass(AgentStepLimitError, AgentError)
    # Can instantiate
    err = AgentStepLimitError("step limit reached")
    assert isinstance(err, AgentError)
    assert str(err) == "step limit reached"


# -- Lazy loading tests --


def test_get_agent_lazily_loads_cosh():
    """get_agent('cosh') triggers lazy import of cosh.adapter module."""
    # If cosh.adapter is already imported (by other tests), that's fine —
    # get_agent should still return a valid instance.
    agent = get_agent("cosh")
    assert agent.name == "cosh"
    assert "swe_runner.agents.cosh.adapter" in sys.modules


def test_get_agent_lazily_loads_openclaw():
    """get_agent('openclaw') triggers lazy import of openclaw.adapter module."""
    agent = get_agent("openclaw")
    assert agent.name == "openclaw"
    assert "swe_runner.agents.openclaw.adapter" in sys.modules


def test_import_agents_does_not_import_adapter_modules():
    """Importing swe_runner.agents must NOT eagerly import adapter modules."""
    env = os.environ.copy()
    src_path = str(Path(__file__).resolve().parents[2] / "src")
    env["PYTHONPATH"] = src_path if not env.get("PYTHONPATH") else f"{src_path}:{env['PYTHONPATH']}"
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            (
                "import sys; "
                "import swe_runner.agents as agents_module; "
                "assert 'cosh' in agents_module._ADAPTER_MODULES; "
                "assert 'openclaw' in agents_module._ADAPTER_MODULES; "
                "assert 'swe_runner.agents.cosh.adapter' not in sys.modules; "
                "assert 'swe_runner.agents.openclaw.adapter' not in sys.modules"
            ),
        ],
        check=False,
        env=env,
        capture_output=True,
        text=True,
    )

    assert result.returncode == 0, result.stderr
