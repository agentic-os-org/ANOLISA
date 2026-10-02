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

"""Agent descriptor registry.

This module is the single place where agent capabilities are discovered.
Adapter modules stay lazily imported so ``import swe_runner.agents`` remains
cheap.
"""

from __future__ import annotations

import importlib
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass, field
from typing import Any

from swe_runner.agents.lifecycle import AgentAdapter, PreparedAgentRun

MetadataCollectorFunc = Callable[[Mapping[str, object]], object]
InputManifestCollectorFunc = Callable[[PreparedAgentRun, Mapping[str, object]], object]
EnvironmentCheckFunc = Callable[..., None]


@dataclass(frozen=True)
class AgentDescriptor:
    """Declared capabilities for one external agent."""

    name: str
    adapter_cls: type[AgentAdapter] | None = None
    adapter_module: str | None = None
    required_binaries: tuple[str, ...] = field(default_factory=tuple)
    environment_checks: tuple[EnvironmentCheckFunc, ...] = field(default_factory=tuple)
    supported_run_options: tuple[str, ...] = field(default_factory=tuple)
    metadata_collectors: tuple[MetadataCollectorFunc, ...] = field(default_factory=tuple)
    input_manifest_collectors: tuple[InputManifestCollectorFunc, ...] = field(default_factory=tuple)

    def merged_with(self, other: AgentDescriptor) -> AgentDescriptor:
        """Merge another descriptor for the same agent name."""
        if self.name != other.name:
            raise ValueError(f"Cannot merge descriptors for different agents: {self.name} != {other.name}")
        return AgentDescriptor(
            name=self.name,
            adapter_cls=other.adapter_cls or self.adapter_cls,
            adapter_module=other.adapter_module or self.adapter_module,
            required_binaries=_dedupe((*self.required_binaries, *other.required_binaries)),
            environment_checks=_dedupe((*self.environment_checks, *other.environment_checks)),
            supported_run_options=_dedupe((*self.supported_run_options, *other.supported_run_options)),
            metadata_collectors=_dedupe((*self.metadata_collectors, *other.metadata_collectors)),
            input_manifest_collectors=_dedupe(
                (*self.input_manifest_collectors, *other.input_manifest_collectors)
            ),
        )


AGENT_DESCRIPTOR_REGISTRY: dict[str, AgentDescriptor] = {}
ADAPTER_REGISTRY: dict[str, type[AgentAdapter]] = {}

# Lazy-loading mapping: agent name -> descriptor module path.
_AGENT_MODULES: dict[str, str] = {
    "cosh": "swe_runner.agents.cosh.registration",
    "openclaw": "swe_runner.agents.openclaw.registration",
}

# Compatibility alias for code/tests that know the adapter module registry.
_ADAPTER_MODULES: dict[str, str] = {
    "cosh": "swe_runner.agents.cosh.adapter",
    "openclaw": "swe_runner.agents.openclaw.adapter",
}

_LOADED_AGENT_MODULES: set[str] = set()


def register_agent_descriptor(descriptor: AgentDescriptor) -> None:
    """Register all declared capabilities for one agent."""
    existing = AGENT_DESCRIPTOR_REGISTRY.get(descriptor.name)
    merged = existing.merged_with(descriptor) if existing is not None else descriptor
    AGENT_DESCRIPTOR_REGISTRY[descriptor.name] = merged

    if merged.adapter_module is not None:
        _ADAPTER_MODULES[merged.name] = merged.adapter_module
    if merged.adapter_cls is not None:
        ADAPTER_REGISTRY[merged.name] = merged.adapter_cls


def register_agent(name: str, adapter_cls: type[AgentAdapter]) -> None:
    """Register an external agent adapter class with the given CLI name."""
    register_agent_descriptor(
        AgentDescriptor(
            name=name,
            adapter_cls=adapter_cls,
            adapter_module=_ADAPTER_MODULES.get(name),
        )
    )


def get_agent(name: str, **kwargs: Any) -> AgentAdapter:
    """Get an external agent adapter instance by CLI name."""
    if name not in ADAPTER_REGISTRY:
        _load_agent_descriptor(name)
        _sync_adapter_registry_from_descriptor(name)

    if name not in ADAPTER_REGISTRY and name in _ADAPTER_MODULES:
        importlib.import_module(_ADAPTER_MODULES[name])
        _sync_adapter_registry_from_descriptor(name)

    if name not in ADAPTER_REGISTRY:
        available = ", ".join(sorted({*_ADAPTER_MODULES.keys(), *ADAPTER_REGISTRY.keys()})) or "(none)"
        raise KeyError(f"Unknown agent '{name}'. Available agents: {available}")
    return ADAPTER_REGISTRY[name](**kwargs)


def get_agent_descriptor(name: str) -> AgentDescriptor | None:
    """Return the descriptor for one agent, importing its registration module if needed."""
    _load_agent_descriptor(name)
    return AGENT_DESCRIPTOR_REGISTRY.get(name)


def list_available_agent_names() -> tuple[str, ...]:
    """Return known agent names without importing heavyweight adapter modules."""
    return tuple(sorted({*_AGENT_MODULES.keys(), *AGENT_DESCRIPTOR_REGISTRY.keys(), *ADAPTER_REGISTRY.keys()}))


def agent_supports_run_option(agent_name: str, option_name: str) -> bool:
    """Return whether an agent declares support for a named run option."""
    descriptor = get_agent_descriptor(agent_name)
    return descriptor is not None and option_name in descriptor.supported_run_options


def iter_required_binaries(agent_name: str) -> tuple[str, ...]:
    """Return CLI binaries required by one agent descriptor."""
    descriptor = get_agent_descriptor(agent_name)
    if descriptor is None:
        return ()
    return descriptor.required_binaries


def iter_environment_checks(agent_name: str) -> tuple[EnvironmentCheckFunc, ...]:
    """Return environment checks declared by one agent descriptor."""
    descriptor = get_agent_descriptor(agent_name)
    if descriptor is None:
        return ()
    return descriptor.environment_checks


def iter_metadata_collectors() -> tuple[MetadataCollectorFunc, ...]:
    """Return metadata collectors declared by registered agent descriptors."""
    _load_all_agent_descriptors()
    collectors: list[MetadataCollectorFunc] = []
    for descriptor in AGENT_DESCRIPTOR_REGISTRY.values():
        collectors.extend(descriptor.metadata_collectors)
    return tuple(collectors)


def iter_input_manifest_collectors() -> tuple[InputManifestCollectorFunc, ...]:
    """Return input manifest collectors declared by registered agent descriptors."""
    _load_all_agent_descriptors()
    collectors: list[InputManifestCollectorFunc] = []
    for descriptor in AGENT_DESCRIPTOR_REGISTRY.values():
        collectors.extend(descriptor.input_manifest_collectors)
    return tuple(collectors)


def _load_all_agent_descriptors() -> None:
    for name in tuple(_AGENT_MODULES):
        _load_agent_descriptor(name)


def _load_agent_descriptor(name: str) -> None:
    module_name = _AGENT_MODULES.get(name)
    if module_name is None or module_name in _LOADED_AGENT_MODULES:
        return
    importlib.import_module(module_name)
    _LOADED_AGENT_MODULES.add(module_name)


def _sync_adapter_registry_from_descriptor(name: str) -> None:
    descriptor = AGENT_DESCRIPTOR_REGISTRY.get(name)
    if descriptor is not None and descriptor.adapter_cls is not None:
        ADAPTER_REGISTRY[name] = descriptor.adapter_cls


def _dedupe[T](items: Iterable[T]) -> tuple[T, ...]:
    seen: set[int] = set()
    result: list[T] = []
    for item in items:
        marker = id(item)
        if marker in seen:
            continue
        seen.add(marker)
        result.append(item)
    return tuple(result)
