"""Framework-neutral bindings for the four Tokenless lifecycle operations."""

from __future__ import annotations

import asyncio
import json
import os
import re
from dataclasses import dataclass
from enum import StrEnum
from importlib.resources import files
from pathlib import Path
from typing import Any

from anolisa_tokenless._native import TokenlessRuntime
from anolisa_tokenless.stats import TokenlessStats


class PreToolAction(StrEnum):
    """Host action selected by the PreTool service."""

    PASSTHROUGH = "passthrough"
    REPLACE_ARGUMENTS = "replace_arguments"
    BLOCK_AND_SUGGEST = "block_and_suggest"


@dataclass(frozen=True)
class RecoveryMethod:
    """Declared model recovery entry point, not a compression policy switch."""

    kind: str = "none"
    name: str | None = None

    def __post_init__(self) -> None:
        if self.kind not in {"none", "shell", "tool"}:
            raise ValueError(f"unknown recovery kind: {self.kind!r}")
        if self.kind == "tool":
            if not isinstance(self.name, str) or not re.fullmatch(
                r"[A-Za-z0-9_-]{1,64}", self.name
            ):
                raise ValueError(
                    "recovery tool name must contain 1–64 ASCII letters, digits, '_' or '-'"
                )
        elif self.name is not None:
            raise ValueError("only tool recovery accepts a name")

    @classmethod
    def shell(cls) -> RecoveryMethod:
        """Uses the host's existing shell tool and trusted local CLI."""
        return cls("shell")

    @classmethod
    def tool(cls, name: str) -> RecoveryMethod:
        """Uses an already registered, reference-authorized tool."""
        return cls("tool", name)

    def as_dict(self) -> dict[str, str]:
        """Serializes the strict operation capability."""
        result = {"kind": self.kind}
        if self.name is not None:
            result["name"] = self.name
        return result


class OutputOptimization(StrEnum):
    """Optimization already applied before PostTool."""

    NONE = "none"
    RTK = "rtk"


class ResultKind(StrEnum):
    """Whether PostTool is processing an ordinary or Retrieve result."""

    TOOL = "tool"
    RETRIEVE = "retrieve"


class ToolResultStatus(StrEnum):
    """Framework-neutral final state of one tool execution."""

    SUCCESS = "success"
    ERROR = "error"
    INTERRUPTED = "interrupted"
    DENIED = "denied"


class ContentOrigin(StrEnum):
    """Authoritative semantic source of PostTool content."""

    COMMAND_OUTPUT = "command_output"
    FILE_CONTENT = "file_content"
    FILE_READ = "file_read"
    API_RESPONSE = "api_response"


class Disposition(StrEnum):
    """Final PostTool routing or compression decision."""

    APPLIED = "applied"
    DRY_RUN = "dry_run"
    PASSTHROUGH = "passthrough"
    NO_SAVINGS = "no_savings"
    RECOVERABILITY_UNAVAILABLE = "recoverability_unavailable"
    TIMEOUT = "timeout"
    TOOL_ERROR = "tool_error"


class ContentType(StrEnum):
    """Detected PostTool content domain."""

    JSON = "json"
    SEARCH_RESULTS = "search_results"
    BUILD_LOG = "build_log"
    STACK_TRACE = "stack_trace"
    DIFF = "diff"
    HTML = "html"
    TABULAR = "tabular"
    SOURCE_CODE = "source_code"
    PLAIN_TEXT = "plain_text"
    UNKNOWN = "unknown"


class AppliedOperation(StrEnum):
    """Concrete transformations applied by Core."""

    SCHEMA_COMPRESSION = "schema_compression"
    TERMINAL_CLEANUP = "terminal_cleanup"
    BUILD_LOG_REDUCTION = "build_log_reduction"
    TABULAR_COMPACTION = "tabular_compaction"
    TABULAR_ROW_REDUCTION = "tabular_row_reduction"
    SEARCH_PATH_SHARING = "search_path_sharing"
    DIFF_REDUCTION = "diff_reduction"
    HTML_EXTRACTION = "html_extraction"
    JSON_CLEANUP = "json_cleanup"
    JSON_RECORD_REDUCTION = "json_record_reduction"
    JSON_TRUNCATION = "json_truncation"
    TOON = "toon"


class Recoverability(StrEnum):
    """Recovery state of one emitted result."""

    LOSSLESS = "lossless"
    RETRIEVABLE = "retrievable"
    UNRECOVERABLE = "unrecoverable"


@dataclass(frozen=True)
class Attribution:
    """Stable identifiers used to attribute lifecycle operations."""

    agent_id: str
    session_id: str
    tool_use_id: str | None = None

    def __post_init__(self) -> None:
        if not self.agent_id:
            raise ValueError("agent_id must not be empty")
        if not self.session_id:
            raise ValueError("session_id must not be empty")


@dataclass(frozen=True)
class TokenlessConfig:
    """State and packaged-resource configuration for the lifecycle SDK."""

    data_dir: str | os.PathLike[str] | None = None
    retrieve_tool_name: str = "tokenless_retrieve"
    rtk_enabled: bool = True
    search_path_sharing_enabled: bool = True
    diff_compression_enabled: bool = False
    html_extraction_enabled: bool = True

    def __post_init__(self) -> None:
        try:
            RecoveryMethod.tool(self.retrieve_tool_name)
        except ValueError as error:
            raise ValueError(f"retrieve_tool_name: {error}") from error
        if self.data_dir is not None:
            data_dir = Path(self.data_dir).expanduser()
            if not data_dir.is_absolute():
                raise ValueError("data_dir must be an absolute path")
            object.__setattr__(self, "data_dir", os.fspath(data_dir))


@dataclass(frozen=True)
class BeforeModelCapabilities:
    """Host capabilities relevant to BeforeModel.

    Only static-tool recovery permits recoverable schema truncation.
    """

    replace_tools: bool
    recovery: RecoveryMethod


@dataclass(frozen=True)
class BeforeModelRequest:
    """Model-visible tools and context at the BeforeModel boundary."""

    tools: tuple[Any, ...]
    visible_context: Any
    capabilities: BeforeModelCapabilities
    attribution: Attribution


@dataclass(frozen=True)
class BeforeModelResponse:
    """Transformed tools and the marker authorization set."""

    tools: tuple[Any, ...]
    visible_markers: frozenset[str]


@dataclass(frozen=True)
class PreToolCapabilities:
    """Host capabilities relevant to PreTool."""

    replace_arguments: bool
    block_and_suggest: bool


@dataclass(frozen=True)
class PreToolRequest:
    """One explicitly identified command field before tool execution."""

    tool_name: str
    arguments: dict[str, Any]
    command_field: str
    capabilities: PreToolCapabilities
    attribution: Attribution

    def __post_init__(self) -> None:
        if not self.tool_name:
            raise ValueError("tool_name must not be empty")
        if not self.command_field:
            raise ValueError("command_field must not be empty")
        if self.attribution.tool_use_id is None:
            raise ValueError("tool_use_id is required for PreTool")


@dataclass(frozen=True)
class PreToolResponse:
    """Arguments, host action, and output-optimization state from Core."""

    arguments: dict[str, Any]
    action: PreToolAction
    output_optimization: OutputOptimization


@dataclass(frozen=True)
class PostToolCapabilities:
    """Host capabilities relevant to PostTool.

    Recovery uses either the existing shell tool or a static authorized tool.
    """

    replace_output: bool
    recovery: RecoveryMethod
    replace_with_text: bool


@dataclass(frozen=True)
class PostToolRequest:
    """One final model-visible tool result before Core routing.

    The exact tool name ``Grep`` opts into search-only routing; its output
    never enters JSON, table, or log compressors.
    """

    result_kind: ResultKind
    tool_name: str
    content: str
    status: ToolResultStatus
    content_origin: ContentOrigin
    output_optimization: OutputOptimization
    capabilities: PostToolCapabilities
    attribution: Attribution
    #: Shell command line behind a ``COMMAND_OUTPUT`` result, when the host has
    #: it. Core reports a plain print of local files such as ``cat page.html``
    #: as ``FILE_READ``; ignored for every other origin.
    command: str | None = None

    def __post_init__(self) -> None:
        if not self.tool_name:
            raise ValueError("tool_name must not be empty")
        if self.attribution.tool_use_id is None and self.result_kind == ResultKind.TOOL:
            raise ValueError("tool_use_id is required for PostTool tool results")


@dataclass(frozen=True)
class PostToolResponse:
    """Core-owned PostTool output and its complete operation metadata."""

    output: str
    disposition: Disposition
    content_type: ContentType | None
    applied_operations: tuple[AppliedOperation, ...]
    recoverability: Recoverability
    before_tokens: int
    after_tokens: int
    stash_keys: tuple[str, ...]
    tokenizer_id: str
    additional_context: str | None


@dataclass(frozen=True)
class RetrieveRequest:
    """Marker-authorized stash lookup with model visibility context."""

    hash_or_marker: str
    visible_markers: frozenset[str]
    attribution: Attribution


@dataclass(frozen=True)
class RetrieveResponse:
    """Normalized stash identity and byte-exact recovered payload."""

    hash: str
    payload: str


class TokenlessSdk:
    """Calls the four Rust lifecycle services without duplicating policy."""

    def __init__(self, config: TokenlessConfig | None = None) -> None:
        self.config = config or TokenlessConfig()
        self.runtime = TokenlessRuntime(
            self.config.data_dir,
            search_path_sharing_enabled=self.config.search_path_sharing_enabled,
            diff_compression_enabled=self.config.diff_compression_enabled,
            html_extraction_enabled=self.config.html_extraction_enabled,
        )
        self._rtk_path = self._resolve_rtk() if self.config.rtk_enabled else None
        self._stats: TokenlessStats | None = None

    @property
    def stats(self) -> TokenlessStats:
        """Returns a lazy query client bound to the Runtime state directory."""
        if self._stats is None:
            self._stats = TokenlessStats(self.runtime.data_dir)
        return self._stats

    async def before_model(self, request: BeforeModelRequest) -> BeforeModelResponse:
        """Runs the Core BeforeModel service."""
        response = await asyncio.to_thread(
            self.runtime._before_model_json,
            _json_dumps(
                {
                    "tools": request.tools,
                    "visible_context": request.visible_context,
                    "capabilities": {
                        "replace_tools": request.capabilities.replace_tools,
                        "recovery": request.capabilities.recovery.as_dict(),
                    },
                }
            ),
            **_attribution_kwargs(request.attribution),
        )
        return _before_model_response(response)

    async def pre_tool(self, request: PreToolRequest) -> PreToolResponse:
        """Runs the Core PreTool service with the packaged RTK executable."""
        if self._rtk_path is None:
            raise RuntimeError("Tokenless RTK is disabled")
        response = await asyncio.to_thread(
            self.runtime._pre_tool_json,
            _json_dumps(
                {
                    "tool_name": request.tool_name,
                    "arguments": request.arguments,
                    "command_field": request.command_field,
                    "capabilities": {
                        "replace_arguments": request.capabilities.replace_arguments,
                        "block_and_suggest": request.capabilities.block_and_suggest,
                    },
                }
            ),
            self._rtk_path,
            **_attribution_kwargs(request.attribution),
        )
        value = _json_object(response)
        arguments = value["arguments"]
        if not isinstance(arguments, dict):
            raise TypeError("Core returned non-object PreTool arguments")
        return PreToolResponse(
            arguments=arguments,
            action=PreToolAction(value["action"]),
            output_optimization=OutputOptimization(value["output_optimization"]),
        )

    async def post_tool(self, request: PostToolRequest) -> PostToolResponse:
        """Runs Core PostTool routing and the content-domain Pipeline."""
        payload: dict[str, Any] = {
            "result_kind": request.result_kind,
            "tool_name": request.tool_name,
            "content": request.content,
            "status": request.status,
            "content_origin": request.content_origin,
            "output_optimization": request.output_optimization,
            "capabilities": {
                "replace_output": request.capabilities.replace_output,
                "recovery": request.capabilities.recovery.as_dict(),
                "replace_with_text": request.capabilities.replace_with_text,
            },
        }
        if request.command is not None:
            payload["command"] = request.command
        response = await asyncio.to_thread(
            self.runtime._post_tool_json,
            _json_dumps(payload),
            **_attribution_kwargs(request.attribution),
        )
        value = _json_object(response)
        content_type = value.get("content_type")
        return PostToolResponse(
            output=value["output"],
            disposition=Disposition(value["disposition"]),
            content_type=(ContentType(content_type) if content_type is not None else None),
            applied_operations=tuple(
                AppliedOperation(item) for item in value["applied_operations"]
            ),
            recoverability=Recoverability(value["recoverability"]),
            before_tokens=value["before_tokens"],
            after_tokens=value["after_tokens"],
            stash_keys=tuple(value["stash_keys"]),
            tokenizer_id=value["tokenizer_id"],
            additional_context=value.get("additional_context"),
        )

    async def retrieve(self, request: RetrieveRequest) -> RetrieveResponse:
        """Runs the Core marker-authorized Retrieve service."""
        response = await asyncio.to_thread(
            self.runtime._retrieve_authorized_json,
            _json_dumps(
                {
                    "hash_or_marker": request.hash_or_marker,
                    "visible_markers": sorted(request.visible_markers),
                }
            ),
            **_attribution_kwargs(request.attribution),
        )
        value = _json_object(response)
        return RetrieveResponse(hash=value["hash"], payload=value["payload"])

    def _resolve_rtk(self) -> Path:
        resource = files("anolisa_tokenless").joinpath("_bin", "rtk")
        if not isinstance(resource, Path):
            raise RuntimeError(
                "anolisa-tokenless must be installed as an unpacked wheel so packaged "
                "RTK has a stable executable path"
            )
        if not resource.is_file() or not os.access(resource, os.X_OK):
            raise RuntimeError(
                "anolisa-tokenless installation is missing its executable packaged RTK"
            )
        return resource


def _attribution_kwargs(attribution: Attribution) -> dict[str, str | None]:
    return {
        "agent_id": attribution.agent_id,
        "session_id": attribution.session_id,
        "tool_use_id": attribution.tool_use_id,
    }


def _json_dumps(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def _json_object(value: str) -> dict[str, Any]:
    decoded = json.loads(value)
    if not isinstance(decoded, dict):
        raise TypeError("Core returned a non-object lifecycle response")
    return decoded


def _before_model_response(value: str) -> BeforeModelResponse:
    decoded = _json_object(value)
    return BeforeModelResponse(
        tools=tuple(decoded["tools"]),
        visible_markers=frozenset(decoded["visible_markers"]),
    )
