"""Pydantic schema for ``observability record`` payloads."""

from datetime import datetime
from types import MappingProxyType
from typing import Annotated, Any, Literal, TypeAlias, get_args

from agent_sec_cli.correlation_context import truncate_correlation_id
from pydantic import (
    BaseModel,
    BeforeValidator,
    ConfigDict,
    Field,
    TypeAdapter,
    ValidationInfo,
    field_validator,
    model_validator,
)

JSON_SCHEMA_DRAFT_2020_12 = "https://json-schema.org/draft/2020-12/schema"

UNKNOWN_HOOK_ERROR = "unknown observability hook"
EMPTY_METRICS_ERROR = "metrics must include at least one allowed metric"
NAIVE_TIMESTAMP_ERROR = "observedAt must be timezone-aware"


class ObservabilityMetadata(BaseModel):
    """Correlation metadata required on every observability record."""

    # Producer contexts can carry extra correlation hints, but the persisted
    # wire record keeps metadata limited to modeled fields. Signal fields must
    # go through hook-specific metrics, where only declared metric names are
    # serialized by ``to_record_metrics()``.
    model_config = ConfigDict(populate_by_name=True, extra="ignore")

    session_id: str = Field(alias="sessionId")
    run_id: str = Field(alias="runId")

    @field_validator("session_id", "run_id")
    @classmethod
    def _truncate_common_correlation_id(cls, value: str, info: ValidationInfo) -> str:
        return truncate_correlation_id(info.field_name, value)


class ModelCallMetadata(ObservabilityMetadata):
    """Correlation metadata for model API call records."""

    call_id: str | None = Field(default=None, alias="callId")

    @field_validator("call_id")
    @classmethod
    def _truncate_call_id(cls, value: str | None, info: ValidationInfo) -> str | None:
        if value is None:
            return None
        return truncate_correlation_id(info.field_name, value)


class ToolCallMetadata(ObservabilityMetadata):
    """Correlation metadata required on tool call records."""

    tool_call_id: str = Field(alias="toolCallId")
    call_id: str | None = Field(default=None, alias="callId")

    @field_validator("tool_call_id", "call_id")
    @classmethod
    def _truncate_tool_correlation_id(
        cls, value: str | None, info: ValidationInfo
    ) -> str | None:
        if value is None:
            return None
        return truncate_correlation_id(info.field_name, value)


class ObservabilityMetrics(BaseModel):
    """Base class for hook-specific metric payloads."""

    # Metric model fields are the public allowlist for each hook. Unknown metric
    # keys are accepted for forward-compatible ingestion but are not serialized.
    model_config = ConfigDict(
        extra="ignore",
        json_schema_extra={
            "minProperties": 1,
        },
    )

    @model_validator(mode="after")
    def _validate_at_least_one_known_metric(self) -> "ObservabilityMetrics":
        if not self.model_fields_set:
            raise ValueError(EMPTY_METRICS_ERROR)
        return self

    def to_record_metrics(self) -> dict[str, Any]:
        """Return only metrics that were supplied and accepted."""
        return self.model_dump(mode="json", exclude_unset=True)


class BeforeAgentRunMetrics(ObservabilityMetrics):
    prompt: Any = None
    system_prompt: Any = None
    user_input: Any = None
    pii_scan_input_sha256: Any = None
    history_messages_count: Any = None
    images_count: Any = None
    context_window_utilization: Any = None
    model_id: Any = None
    model_provider: Any = None


class BeforeLlmCallMetrics(ObservabilityMetrics):
    prompt: Any = None
    system_prompt: Any = None
    user_input: Any = None
    history_messages_count: Any = None
    images_count: Any = None
    context_window_utilization: Any = None
    model_id: Any = None
    model_provider: Any = None
    api: Any = None
    transport: Any = None


class AfterLlmCallMetrics(ObservabilityMetrics):
    latency_ms: Any = None
    outcome: Any = None
    error_category: Any = None
    failure_kind: Any = None
    response: Any = None
    output_kind: Any = None
    stop_reason: Any = None
    assistant_texts_count: Any = None
    tool_calls_count: Any = None
    tool_calls: Any = None
    request_payload_bytes: Any = None
    response_stream_bytes: Any = None
    time_to_first_byte_ms: Any = None
    upstream_request_id_hash: Any = None


class BeforeToolCallMetrics(ObservabilityMetrics):
    tool_name: Any = None
    parameters: Any = None
    pii_scan_input_sha256: Any = None


class AfterToolCallMetrics(ObservabilityMetrics):
    result: Any = None
    error: Any = None
    pii_scan_input_sha256: Any = None
    duration_ms: Any = None
    status: Any = None
    exit_code: Any = None
    result_size_bytes: Any = None


class AfterAgentRunMetrics(ObservabilityMetrics):
    response: Any = None
    output_kind: Any = None
    stop_reason: Any = None
    assistant_texts_count: Any = None
    tool_calls_count: Any = None
    tool_calls: Any = None
    success: Any = None
    error: Any = None
    duration_ms: Any = None
    total_api_calls: Any = None
    total_tool_calls: Any = None
    final_model_id: Any = None
    final_model_provider: Any = None


class ObservabilityRecord(BaseModel):
    """Common fields shared by every observability hook record."""

    model_config = ConfigDict(populate_by_name=True, extra="ignore")

    hook: str
    observed_at: datetime = Field(alias="observedAt")
    metadata: ObservabilityMetadata
    metrics: ObservabilityMetrics

    @field_validator("observed_at")
    @classmethod
    def _validate_observed_at(cls, value: datetime) -> datetime:
        if value.tzinfo is None or value.tzinfo.utcoffset(value) is None:
            raise ValueError(NAIVE_TIMESTAMP_ERROR)
        return value

    def to_record(self) -> dict[str, Any]:
        """Return a JSON-serializable record using the public wire aliases."""
        record = self.model_dump(by_alias=True, mode="json", exclude_none=True)
        record["metrics"] = self.metrics.to_record_metrics()
        return record


class BeforeAgentRunRecord(ObservabilityRecord):
    hook: Literal["before_agent_run"]
    metadata: ObservabilityMetadata
    metrics: BeforeAgentRunMetrics


class BeforeLlmCallRecord(ObservabilityRecord):
    hook: Literal["before_llm_call"]
    metadata: ModelCallMetadata
    metrics: BeforeLlmCallMetrics


class AfterLlmCallRecord(ObservabilityRecord):
    hook: Literal["after_llm_call"]
    metadata: ModelCallMetadata
    metrics: AfterLlmCallMetrics


class BeforeToolCallRecord(ObservabilityRecord):
    hook: Literal["before_tool_call"]
    metadata: ToolCallMetadata
    metrics: BeforeToolCallMetrics


class AfterToolCallRecord(ObservabilityRecord):
    hook: Literal["after_tool_call"]
    metadata: ToolCallMetadata
    metrics: AfterToolCallMetrics


class AfterAgentRunRecord(ObservabilityRecord):
    hook: Literal["after_agent_run"]
    metadata: ObservabilityMetadata
    metrics: AfterAgentRunMetrics


OBSERVABILITY_RECORD_TYPES: tuple[type[ObservabilityRecord], ...] = (
    BeforeAgentRunRecord,
    BeforeLlmCallRecord,
    AfterLlmCallRecord,
    BeforeToolCallRecord,
    AfterToolCallRecord,
    AfterAgentRunRecord,
)


def _record_hook(record_type: type[ObservabilityRecord]) -> str:
    hook_values = get_args(record_type.model_fields["hook"].annotation)
    if len(hook_values) != 1 or not isinstance(hook_values[0], str):
        raise TypeError(f"{record_type.__name__}.hook must be a single Literal string")
    return hook_values[0]


def _record_metric_names(record_type: type[ObservabilityRecord]) -> frozenset[str]:
    metrics_type = record_type.model_fields["metrics"].annotation
    if not isinstance(metrics_type, type) or not issubclass(
        metrics_type, ObservabilityMetrics
    ):
        raise TypeError(
            f"{record_type.__name__}.metrics must be an ObservabilityMetrics subclass"
        )
    return frozenset(metrics_type.model_fields)


SUPPORTED_OBSERVABILITY_HOOKS = frozenset(
    _record_hook(record_type) for record_type in OBSERVABILITY_RECORD_TYPES
)


def _validate_known_hook(value: Any) -> Any:
    if isinstance(value, dict):
        hook = value.get("hook")
        if hook is not None and hook not in SUPPORTED_OBSERVABILITY_HOOKS:
            raise ValueError(
                f"{UNKNOWN_HOOK_ERROR} {hook!r}; "
                f"expected one of {sorted(SUPPORTED_OBSERVABILITY_HOOKS)}"
            )
    return value


ObservabilityRecordPayload: TypeAlias = Annotated[
    BeforeAgentRunRecord
    | BeforeLlmCallRecord
    | AfterLlmCallRecord
    | BeforeToolCallRecord
    | AfterToolCallRecord
    | AfterAgentRunRecord,
    Field(discriminator="hook"),
    BeforeValidator(_validate_known_hook),
]

OBSERVABILITY_RECORD_ADAPTER = TypeAdapter(ObservabilityRecordPayload)


def validate_observability_record(value: Any) -> ObservabilityRecord:
    """Validate one observability record payload."""
    return OBSERVABILITY_RECORD_ADAPTER.validate_python(value)


def observability_record_json_schema() -> dict[str, Any]:
    """Return the public observability record JSON Schema."""
    schema = OBSERVABILITY_RECORD_ADAPTER.json_schema(by_alias=True)
    schema["$schema"] = JSON_SCHEMA_DRAFT_2020_12
    return schema


def observability_hook_metric_allowlist() -> MappingProxyType[str, frozenset[str]]:
    """Return hook-to-metric names derived from the typed record definitions."""
    return MappingProxyType(
        {
            _record_hook(record_type): _record_metric_names(record_type)
            for record_type in OBSERVABILITY_RECORD_TYPES
        }
    )
