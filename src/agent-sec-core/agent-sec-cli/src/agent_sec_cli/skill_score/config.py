"""Fixed delivery configuration, shared with the optional worker."""

from pydantic import BaseModel, ConfigDict, Field


class ScoreConfig(BaseModel):
    model_config = ConfigDict(extra="forbid")
    model: str = "deepseek-v4.1-flash-1017"
    docker_image: str = "agent-sec-skill-score:1"
    claude_version: str = "2.1.143"
    timeout_seconds: int = Field(default=600, gt=0)
    agentsight_traces: dict[str, str] = Field(default_factory=dict)
