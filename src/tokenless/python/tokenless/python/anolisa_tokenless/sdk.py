"""Framework-neutral bindings for the four Tokenless lifecycle operations."""

from __future__ import annotations

import asyncio
import copy
import json
import logging
import os
import re
import signal
import subprocess
import sys
from dataclasses import dataclass, field
from enum import StrEnum
from importlib.resources import files
from pathlib import Path
from typing import Any

from anolisa_tokenless._native import TokenlessRuntime, _load_policy_config
from anolisa_tokenless.stats import TokenlessStats

_LOGGER = logging.getLogger(__name__)

# RTK can block without limit, for example in `git rev-parse` on a hung mount.
_PRE_TOOL_TIMEOUT_SECONDS = 5.0
_PROCESS_CLEANUP_TIMEOUT_SECONDS = 2.0
_STDERR_EXCERPT_CHARS = 500
# `tokenless compress` exit status for a request it cannot read or decode.
_EXIT_INVALID_REQUEST = 2


class PreToolAction(StrEnum):
    """Host action selected by the PreTool service."""

    PASSTHROUGH = "passthrough"
    REPLACE_ARGUMENTS = "replace_arguments"
    BLOCK_AND_SUGGEST = "block_and_suggest"


class PostToolReplacementCapability(StrEnum):
    """Level of PostTool output replacement supported by the calling host."""

    FULL_RETRIEVABLE = "full_retrievable"
    LOSSLESS_ONLY = "lossless_only"
    NONE = "none"


class DomainRolloutMode(StrEnum):
    """Per-domain routing switch between PreTool RTK and PostTool native execution."""

    PRETOOL_RTK = "pretool_rtk"
    POSTTOOL_NATIVE = "posttool_native"


_RTK_ROLLOUT_DOMAINS = ("search", "vcs", "build_test", "lint", "cloud_net")


@dataclass(frozen=True)
class RtkRolloutConfig:
    """Domain-level switches between PreTool RTK and PostTool native compression."""

    search: DomainRolloutMode = DomainRolloutMode.POSTTOOL_NATIVE
    vcs: DomainRolloutMode = DomainRolloutMode.PRETOOL_RTK
    build_test: DomainRolloutMode = DomainRolloutMode.POSTTOOL_NATIVE
    lint: DomainRolloutMode = DomainRolloutMode.PRETOOL_RTK
    cloud_net: DomainRolloutMode = DomainRolloutMode.PRETOOL_RTK

    def __post_init__(self) -> None:
        for domain in _RTK_ROLLOUT_DOMAINS:
            value = getattr(self, domain)
            if not isinstance(value, DomainRolloutMode):
                object.__setattr__(self, domain, DomainRolloutMode(value))

    def as_dict(self) -> dict[str, str]:
        """Serializes the five rollout domains using canonical snake_case wire values."""
        return {domain: getattr(self, domain).value for domain in _RTK_ROLLOUT_DOMAINS}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> RtkRolloutConfig:
        """Constructs rollout switches from a partial or complete domain mapping."""
        unknown = sorted((repr(k) for k in data if k not in _RTK_ROLLOUT_DOMAINS))
        if unknown:
            raise ValueError(f"unknown rtk_rollout domain(s): {', '.join(unknown)}")
        defaults = cls()
        return cls(
            **{
                domain: DomainRolloutMode(data.get(domain, getattr(defaults, domain)))
                for domain in _RTK_ROLLOUT_DOMAINS
            }
        )


_UNSET: Any = object()


def _load_default_policy() -> tuple[RtkRolloutConfig, tuple[str, ...]]:
    decoded = json.loads(_load_policy_config())
    rollout = RtkRolloutConfig.from_dict(decoded.get("rtk_rollout") or {})
    excluded = tuple(str(item) for item in (decoded.get("exclude_commands") or ()))
    return rollout, excluded


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
    BROWSER_SNAPSHOT = "browser_snapshot"
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
    # Historical wire value; current table compression never emits it.
    TABULAR_ROW_REDUCTION = "tabular_row_reduction"
    SEARCH_PATH_SHARING = "search_path_sharing"
    DIFF_REDUCTION = "diff_reduction"
    HTML_EXTRACTION = "html_extraction"
    BROWSER_SNAPSHOT_REDUCTION = "browser_snapshot_reduction"
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


def _env_flag(name: str, default: bool) -> bool:
    value = os.environ.get(name)
    return default if not value else value.lower() in ("1", "true", "yes")


@dataclass(frozen=True)
class TokenlessConfig:
    """State and packaged-resource configuration for the lifecycle SDK."""

    data_dir: str | os.PathLike[str] | None = None
    retrieve_tool_name: str = "tokenless_retrieve"
    rtk_enabled: bool = True
    search_path_sharing_enabled: bool = field(
        default_factory=lambda: _env_flag("TOKENLESS_SEARCH_PATH_SHARING_ENABLED", True)
    )
    diff_compression_enabled: bool = field(
        default_factory=lambda: _env_flag("TOKENLESS_DIFF_COMPRESSION_ENABLED", True)
    )
    html_extraction_enabled: bool = field(
        default_factory=lambda: _env_flag("TOKENLESS_HTML_EXTRACTION_ENABLED", True)
    )
    compression_enabled: bool = field(
        default_factory=lambda: _env_flag("TOKENLESS_COMPRESSION_ENABLED", True)
    )
    # Append options so existing positional arguments retain their meaning.
    mixed_content_enabled: bool = field(
        default_factory=lambda: _env_flag("TOKENLESS_MIXED_CONTENT_ENABLED", False)
    )
    browser_snapshot_enabled: bool = field(
        default_factory=lambda: _env_flag("TOKENLESS_BROWSER_SNAPSHOT_ENABLED", True)
    )
    rtk_rollout: RtkRolloutConfig = _UNSET
    exclude_commands: tuple[str, ...] = _UNSET

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
        if self.rtk_rollout is _UNSET or self.exclude_commands is _UNSET:
            default_rollout, default_excludes = _load_default_policy()
            if self.rtk_rollout is _UNSET:
                object.__setattr__(self, "rtk_rollout", default_rollout)
            if self.exclude_commands is _UNSET:
                object.__setattr__(self, "exclude_commands", default_excludes)
        if isinstance(self.rtk_rollout, dict):
            object.__setattr__(self, "rtk_rollout", RtkRolloutConfig.from_dict(self.rtk_rollout))
        elif not isinstance(self.rtk_rollout, RtkRolloutConfig):
            raise TypeError("rtk_rollout must be an RtkRolloutConfig or dict")
        if isinstance(self.exclude_commands, (str, bytes)):
            raise TypeError("exclude_commands must be a sequence of strings, not str or bytes")
        normalized_excludes = tuple(self.exclude_commands)
        if not all(isinstance(item, str) for item in normalized_excludes):
            raise TypeError("exclude_commands entries must be strings")
        object.__setattr__(self, "exclude_commands", normalized_excludes)


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
    #: PostTool output replacement capability advertised by the host.
    #: Defaults to ``FULL_RETRIEVABLE`` and is omitted on the wire when default;
    #: sending non-default values requires a Core that supports this v2 field.
    post_tool_replacement: PostToolReplacementCapability = (
        PostToolReplacementCapability.FULL_RETRIEVABLE
    )


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
    #: Process exit status for a failed or completed command. Omitted on the wire
    #: when ``None``; passing it on ``ToolResultStatus.ERROR`` opts the result into
    #: Level E1/E2 error compression and requires a Core supporting Protocol v2
    #: optional error fields.
    exit_code: int | None = None
    #: Separate diagnostic stream captured alongside ``content``, when the host
    #: keeps ``stderr`` out of band. Used as fallback for ``ENV_*`` diagnosis;
    #: omitted on the wire when ``None``.
    stderr: str | None = None
    #: Unmodified user/agent shell command prior to PreTool rewriting, if tracked
    #: by the host adapter; omitted on the wire when ``None``.
    original_command: str | None = None
    #: Working directory in which the command executed, if known by the host;
    #: omitted on the wire when ``None``.
    cwd: str | None = None

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
            compression_enabled=self.config.compression_enabled,
            search_path_sharing_enabled=self.config.search_path_sharing_enabled,
            diff_compression_enabled=self.config.diff_compression_enabled,
            mixed_content_enabled=self.config.mixed_content_enabled,
            html_extraction_enabled=self.config.html_extraction_enabled,
            browser_snapshot_enabled=self.config.browser_snapshot_enabled,
            exclude_commands=list(self.config.exclude_commands),
        )
        self._rtk_launcher = self._resolve_launcher() if self.config.rtk_enabled else None
        self._rewrite_failure_reported = False
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
        """Runs the Core PreTool service in a short-lived `tokenless compress` process.

        RTK assumes one process per command: it caches trusted filters and project files
        for the process lifetime and can wait on `git` without a deadline, so it does not
        run inside the host. If the child fails or exceeds its deadline, the command passes
        through unchanged and a warning is logged. Only a request the CLI cannot read or
        decode raises `ValueError`.
        """
        if self._rtk_launcher is None:
            raise RuntimeError("Tokenless RTK is disabled")
        capabilities_payload: dict[str, Any] = {
            "replace_arguments": request.capabilities.replace_arguments,
            "block_and_suggest": request.capabilities.block_and_suggest,
        }
        if (
            request.capabilities.post_tool_replacement
            != PostToolReplacementCapability.FULL_RETRIEVABLE
        ):
            capabilities_payload["post_tool_replacement"] = (
                request.capabilities.post_tool_replacement
            )
        envelope = _json_dumps(
            {
                "protocol_version": 2,
                "operation": "pre_tool",
                "attribution": _attribution_kwargs(request.attribution),
                "input": {
                    "tool_name": request.tool_name,
                    "arguments": request.arguments,
                    "command_field": request.command_field,
                    "capabilities": capabilities_payload,
                },
            }
        ).encode("utf-8")
        try:
            output = await asyncio.to_thread(
                _run_compress,
                self._rtk_launcher,
                envelope,
                self.runtime.data_dir,
                rtk_rollout_json=_json_dumps(self.config.rtk_rollout.as_dict()),
                exclude_commands_json=_json_dumps(list(self.config.exclude_commands)),
            )
            return _pre_tool_response(output)
        except _RewriteUnavailable as failure:
            self._report_rewrite_failure(failure)
            return PreToolResponse(
                arguments=copy.deepcopy(request.arguments),
                action=PreToolAction.PASSTHROUGH,
                output_optimization=OutputOptimization.NONE,
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
        if request.exit_code is not None:
            payload["exit_code"] = request.exit_code
        if request.stderr is not None:
            payload["stderr"] = request.stderr
        if request.original_command is not None:
            payload["original_command"] = request.original_command
        if request.cwd is not None:
            payload["cwd"] = request.cwd
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

    def _resolve_launcher(self) -> Path:
        name = "tokenless.exe" if sys.platform == "win32" else "tokenless"
        resource = files("anolisa_tokenless").joinpath("_bin", name)
        if not isinstance(resource, Path):
            raise RuntimeError(
                "anolisa-tokenless must be installed as an unpacked wheel so the packaged "
                "tokenless executable has a stable path for RTK commands"
            )
        if not resource.is_file() or not os.access(resource, os.X_OK):
            raise RuntimeError(
                "anolisa-tokenless installation is missing its packaged tokenless executable"
            )
        return resource

    def _report_rewrite_failure(self, failure: _RewriteUnavailable) -> None:
        # One warning keeps lost savings visible without repeating on every tool call.
        level = logging.DEBUG if self._rewrite_failure_reported else logging.WARNING
        self._rewrite_failure_reported = True
        _LOGGER.log(level, "Tokenless PreTool left the command unchanged: %s", failure)


class _RewriteUnavailable(Exception):
    """PreTool produced no usable result, so the command runs unchanged."""


def _run_compress(
    launcher: Path,
    envelope: bytes,
    data_dir: str,
    *,
    rtk_rollout_json: str | None = None,
    exclude_commands_json: str | None = None,
) -> bytes:
    """Returns the stdout of one `tokenless compress`, bounded by the PreTool deadline."""
    isolation: dict[str, Any]
    if sys.platform == "win32":
        isolation = {"creationflags": subprocess.CREATE_NO_WINDOW}
    else:
        # A new session is a new process group, which a timeout kills as a whole.
        isolation = {"start_new_session": True}
    child_env = {**os.environ, "TOKENLESS_DATA_DIR": data_dir}
    if rtk_rollout_json is not None:
        child_env["TOKENLESS_RTK_ROLLOUT"] = rtk_rollout_json
    if exclude_commands_json is not None:
        child_env["TOKENLESS_EXCLUDE_COMMANDS"] = exclude_commands_json
    try:
        process = subprocess.Popen(
            [os.fspath(launcher), "compress"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=child_env,
            **isolation,
        )
    except OSError as error:
        raise _RewriteUnavailable(f"cannot start {launcher}: {error}") from error
    try:
        stdout, stderr = process.communicate(envelope, timeout=_PRE_TOOL_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired as error:
        _kill_process_tree(process)
        raise _RewriteUnavailable(
            f"{launcher} compress exceeded {_PRE_TOOL_TIMEOUT_SECONDS} seconds"
        ) from error
    if process.returncode == 0:
        return stdout
    detail = stderr.decode("utf-8", "replace").strip()[-_STDERR_EXCERPT_CHARS:]
    if process.returncode == _EXIT_INVALID_REQUEST:
        raise ValueError(f"invalid PreTool request: {detail}")
    raise _RewriteUnavailable(f"{launcher} compress exited with {process.returncode}: {detail}")


def _kill_process_tree(process: subprocess.Popen[bytes]) -> None:
    """Kills a timed-out child and what it started, such as a stalled `git`."""
    try:
        if sys.platform == "win32":
            system_root = os.environ.get("SystemRoot", r"C:\Windows")
            subprocess.run(
                [
                    os.path.join(system_root, "System32", "taskkill.exe"),
                    "/T",
                    "/F",
                    "/PID",
                    str(process.pid),
                ],
                check=True,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                timeout=_PROCESS_CLEANUP_TIMEOUT_SECONDS,
                creationflags=subprocess.CREATE_NO_WINDOW,
            )
        else:
            os.killpg(process.pid, signal.SIGKILL)
    except (OSError, subprocess.SubprocessError):
        process.kill()
    # The wait is bounded because a child stuck in the kernel outlives SIGKILL;
    # subprocess reaps an abandoned child when it next starts a process.
    try:
        process.wait(timeout=_PROCESS_CLEANUP_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        pass
    # On Windows, communicate() reader threads hold these pipes until every writer
    # exits, and closing a pipe would block on them; the threads release it instead.
    if sys.platform != "win32":
        for pipe in (process.stdin, process.stdout, process.stderr):
            if pipe is not None:
                pipe.close()


def _pre_tool_response(output: bytes) -> PreToolResponse:
    try:
        decoded = _json_object(output)
        # A version-skewed or mismatched envelope must not be trusted: its
        # result-shaped body would otherwise replace the original command.
        if decoded.get("protocol_version") != 2 or decoded.get("operation") != "pre_tool":
            raise ValueError("PreTool response does not match Protocol v2")
        value = decoded["result"]
        arguments = value["arguments"]
        if not isinstance(arguments, dict):
            raise TypeError("non-object PreTool arguments")
        return PreToolResponse(
            arguments=arguments,
            action=PreToolAction(value["action"]),
            output_optimization=OutputOptimization(value["output_optimization"]),
        )
    except (KeyError, TypeError, ValueError) as error:
        raise _RewriteUnavailable(f"malformed tokenless compress response: {error!r}") from error


def _attribution_kwargs(attribution: Attribution) -> dict[str, str | None]:
    return {
        "agent_id": attribution.agent_id,
        "session_id": attribution.session_id,
        "tool_use_id": attribution.tool_use_id,
    }


def _json_dumps(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


def _json_object(value: str | bytes) -> dict[str, Any]:
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
