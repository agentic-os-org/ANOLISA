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

"""Agent adapter registry and shared errors for SWE-bench."""

from __future__ import annotations

from swe_runner.agents.lifecycle import AgentAdapter, PreparedAgentRun
from swe_runner.agents.registry import (
    _ADAPTER_MODULES,
    _AGENT_MODULES,
    ADAPTER_REGISTRY,
    AGENT_DESCRIPTOR_REGISTRY,
    AgentDescriptor,
    agent_supports_run_option,
    get_agent,
    get_agent_descriptor,
    iter_environment_checks,
    iter_required_binaries,
    list_available_agent_names,
    register_agent,
    register_agent_descriptor,
)


class AgentError(Exception):
    """Base exception for agent-related errors."""


class AgentNotFoundError(AgentError):
    """Raised when agent binary is not found in PATH."""


class AgentTimeoutError(AgentError):
    """Raised when agent execution exceeds the timeout limit."""


class AgentStepLimitError(AgentError):
    """Raised when agent reaches the maximum number of steps."""


class AgentEnvironmentError(AgentError):
    """Raised when agent environment pre-flight check fails."""


__all__ = [
    "AgentAdapter",
    "AgentDescriptor",
    "PreparedAgentRun",
    "AgentError",
    "AgentNotFoundError",
    "AgentTimeoutError",
    "AgentStepLimitError",
    "AgentEnvironmentError",
    "ADAPTER_REGISTRY",
    "AGENT_DESCRIPTOR_REGISTRY",
    "_ADAPTER_MODULES",
    "_AGENT_MODULES",
    "agent_supports_run_option",
    "get_agent",
    "get_agent_descriptor",
    "iter_environment_checks",
    "iter_required_binaries",
    "list_available_agent_names",
    "register_agent",
    "register_agent_descriptor",
]
