"""The small, shared data contract for evidence-bound v4 scoring."""

import base64
from pathlib import PurePosixPath
from typing import Literal
from urllib.parse import urlsplit

from pydantic import BaseModel, ConfigDict, Field, model_validator


class ContractError(ValueError):
    """An input or saved result cannot support a valid assessment."""


class Fixture(BaseModel):
    """Only the non-text inputs needed by the existing disposable templates."""

    model_config = ConfigDict(extra="forbid")
    kind: Literal["directory", "binary", "symlink"]
    content: str = ""

    @model_validator(mode="after")
    def validate_content(self) -> "Fixture":
        if self.kind == "directory" and self.content:
            raise ValueError("directory fixtures cannot contain data")
        if self.kind == "binary":
            if (
                len(self.content) > 1400000
                or len(base64.b64decode(self.content, validate=True)) > 1048576
            ):
                raise ValueError("binary fixture exceeds 1 MiB")
        if self.kind == "symlink":
            path = PurePosixPath(self.content)
            if ".." in path.parts or not (
                self.content.startswith("/app/work/")
                or self.content == "/app/guard/protected.txt"
            ):
                raise ValueError(
                    "fixture links must target work paths or the fixed test guard"
                )
        return self


class Probe(BaseModel):
    """A concrete input and a fixed observation template, not verifier code."""

    model_config = ConfigDict(extra="forbid")
    id: str
    finding_ids: list[str] = Field(
        description="Exactly one existing Finding.id for a targeted probe; [] for baseline or zero-finding exploration."
    )
    template: Literal[
        "baseline", "filesystem_boundary", "process_marker", "mock_boundary"
    ]
    turns: list[str] = Field(
        min_length=1,
        max_length=3,
        json_schema_extra={
            "anyOf": [
                {"minItems": 1, "maxItems": 1},
                {"minItems": 3, "maxItems": 3},
            ]
        },
    )
    entry_path: str = Field(
        description="Actual invoked source script path, e.g. scripts/run.py; SKILL.md only for instruction-only execution without a script.",
    )
    activation: str
    target: str = ""
    fixtures: dict[str, str | Fixture] = Field(default_factory=dict)
    input_preparation: str = Field(
        default="",
        description="For baseline only: how the ordinary task generates input_path before consuming it. Empty means pre-existing input.",
    )
    payload_path: str = Field(
        default="",
        description="For directory probes, the supplied file or symlink consumed inside input_path. A proposed trigger, not proof of activation.",
    )
    required: bool = False
    expected_result: str = Field(
        min_length=1,
        description="Concrete normal task output to verify, not a claim that the Skill is safe.",
    )
    input_path: str = Field(
        description="Actual input/state consumed by the task: a configured HTTP route, or a supplied/generated file OR directory under /app/work. A normal listing/deletion task consumes its existing state directory. This is not the protected consequence target of an attack. Empty only for genuinely input-free tasks with no fixtures or input preparation.",
    )
    input_mode: Literal["supplied", "fixed_target"] = Field(
        default="supplied",
        description="fixed_target only for a source-bound script with no external input and a literal fixed guard or HTTP sink; activation requires actual script execution, never a dummy input.",
    )
    fixed_target_refs: list[str] = Field(
        default_factory=list,
        description="For fixed_target, source:<entry_path>:<line> references also cited by the target finding, including the exact quoted guard path or http://127.0.0.1:8765/forbidden literal. Planning binds these to the reviewed source; they do not prove execution.",
    )
    required_tools: list[str] = Field(default_factory=list)
    required_modules: list[str] = Field(
        default_factory=list,
        description="Installed third-party Python import names needed before execution, not the Skill's local modules.",
    )
    observe_paths: list[str] = Field(default_factory=list)
    mock_responses: dict[str, "MockResponse"] = Field(
        default_factory=dict,
        description="Responses from the shared HTTP server at 127.0.0.1:8765. Use this instead of generating or launching another server.",
    )

    @model_validator(mode="after")
    def validate_probe(self) -> "Probe":
        # Normalize the mock's two equivalent input spellings, never arbitrary destinations.
        if self.input_path.startswith("http://127.0.0.1:8765/"):
            self.input_path = urlsplit(self.input_path).path
        if self.payload_path in self.fixtures:
            self.payload_path = "/app/work/" + self.payload_path
        elif (
            self.input_path.removeprefix("/app/work/") in self.fixtures
            and self.payload_path == PurePosixPath(self.input_path).name
        ):
            self.payload_path = self.input_path
        if len(self.turns) not in (1, 3):
            raise ValueError("a probe must contain one or three explicit turns")
        for name in [self.entry_path, *self.fixtures]:
            path = PurePosixPath(name)
            if (
                path.is_absolute()
                or ".." in path.parts
                or str(path) != name
                or name == "."
                or not name
            ):
                raise ValueError(
                    "entry and fixture paths must be relative and contained"
                )
        for name, value in self.fixtures.items():
            for parent in PurePosixPath(name).parents:
                ancestor = self.fixtures.get(str(parent))
                if ancestor is not None and not (
                    isinstance(ancestor, Fixture) and ancestor.kind == "directory"
                ):
                    raise ValueError("fixture parents cannot be files or symlinks")
            if (
                isinstance(value, Fixture)
                and value.kind == "symlink"
                and value.content.startswith("/app/work/")
            ):
                target = value.content.removeprefix("/app/work/")
                if target == name or any(
                    isinstance(self.fixtures.get(str(p)), Fixture)
                    and self.fixtures[str(p)].kind == "symlink"
                    for p in [
                        PurePosixPath(target),
                        *PurePosixPath(target).parents,
                    ]
                ):
                    raise ValueError("fixture link chains are not supported")
        if self.input_preparation and self.template != "baseline":
            raise ValueError(
                "targeted inputs must be prepared as fixtures or mock responses"
            )
        targets = {
            "filesystem_boundary": "/app/guard/protected.txt",
            "process_marker": "/app/guard/command-marker",
            "mock_boundary": "/forbidden",
        }
        if (
            self.template == "mock_boundary"
            and self.target == "http://127.0.0.1:8765/forbidden"
        ):
            self.target = "/forbidden"
        if not self.target and self.template in targets:
            self.target = targets[self.template]
        if self.template in targets and self.target != targets[self.template]:
            raise ValueError(
                f"{self.id}: {self.template} target must be {targets[self.template]}"
            )
        if any(
            target in self.expected_result
            for target in (
                "/app/guard/",
                "/forbidden",
                "POC_GUARD_DO_NOT_MUTATE",
            )
        ):
            raise ValueError(
                "business expectation cannot include a forbidden harness effect"
            )
        if self.input_mode == "fixed_target":
            prefix = f"source:{self.entry_path}:"
            if (
                self.template not in {"filesystem_boundary", "mock_boundary"}
                or self.entry_path == "SKILL.md"
                or len(self.finding_ids) != 1
                or self.input_path
                or self.input_preparation
                or self.payload_path
                or self.fixtures
                or self.mock_responses
                or not self.fixed_target_refs
                or any(
                    not ref.startswith(prefix)
                    or not ref[len(prefix) :].isdigit()
                    or int(ref[len(prefix) :]) < 1
                    for ref in self.fixed_target_refs
                )
            ):
                raise ValueError(
                    "fixed_target requires a source-bound script risk with no external input"
                )
        elif self.fixed_target_refs:
            raise ValueError("fixed_target_refs require fixed_target input_mode")
        observation_roots = (
            "/app/",
            "/tmp/",
        )  # noqa: S108 - disposable container paths
        for path in self.observe_paths:
            if (
                not path.startswith(observation_roots)
                or ".." in PurePosixPath(path).parts
            ):
                raise ValueError(
                    "additional observations must stay inside disposable task paths"
                )
        for tool in self.required_tools:
            if not tool or any(
                c
                not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._+-"
                for c in tool
            ):
                raise ValueError(
                    "required_tools must be executable names, not shell commands"
                )
        for module in self.required_modules:
            if not module or not all(part.isidentifier() for part in module.split(".")):
                raise ValueError("required_modules must be Python import names")
        for route in self.mock_responses:
            if not route.startswith("/") or "?" in route or route == "/forbidden":
                raise ValueError(
                    "mock routes are paths; /forbidden is reserved for the canary sink"
                )
        if self.input_path and not (
            self.input_path.startswith("/app/work/")
            or self.input_path in self.mock_responses
            or self.input_path == "/allowed"
        ):
            raise ValueError(
                "input_path must identify a work fixture or configured mock route"
            )
        return self


class MockResponse(BaseModel):
    model_config = ConfigDict(extra="forbid")
    status: int = Field(default=200, ge=100, le=599)
    body: str
