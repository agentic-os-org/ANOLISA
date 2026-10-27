"""Publish redacted JSON evidence from the worker."""

import json
from pathlib import Path
from typing import Any

from agent_sec_cli.skill_score.evidence.redact import redact_value


def save(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(
        json.dumps(redact_value(value), ensure_ascii=False, indent=2) + "\n"
    )
    temporary.replace(path)
