"""Fixed delivery configuration, shared with the optional worker."""

from pydantic import BaseModel, ConfigDict, Field


class ScoreConfig(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model: str = "qwen3.8-max"
    judge_model: str = "qwen3.8-max"
    judge_max_tokens: int = Field(default=24000, gt=0)
    judge_concurrency: int = Field(default=4, gt=0)
    docker_image: str = "agent-sec-skill-score:1"
    claude_version: str = "2.1.143"
    timeout_seconds: int = Field(default=600, gt=0)
    hard_run_limit: int = Field(default=24, gt=0)
    deadline_minutes: int = Field(default=45, gt=0)
    agentsight_traces: dict[str, str] = Field(default_factory=dict)
