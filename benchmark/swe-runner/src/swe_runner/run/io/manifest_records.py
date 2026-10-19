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

"""Reusable input manifest record builders."""

from __future__ import annotations

import hashlib
import json
import logging
import re
from pathlib import Path
from typing import Any, cast

logger = logging.getLogger(__name__)

_REDACTED = "<redacted>"
_SENSITIVE_EXACT_KEYS = frozenset(
    {
        "api_key",
        "apikey",
        "auth",
        "authorization",
        "bearer",
        "cookie",
        "credential",
        "credentials",
        "password",
        "secret",
        "token",
    }
)
_SENSITIVE_KEY_SUFFIXES = (
    "_api_key",
    "apikey",
    "password",
    "secret",
    "token",
)
# A credential mapping or list is routinely named in the plural ("tokens",
# "secrets"); only whole words are generalised, so a numeric limit such as
# "max_tokens" keeps its value.
_SENSITIVE_KEY_FORMS = _SENSITIVE_EXACT_KEYS | frozenset(
    key + "s" for key in _SENSITIVE_EXACT_KEYS
)
# Split camelCase and acronym runs, so "APIKey" and "authToken" keep their
# word boundaries instead of turning into one letter per underscore.
_CAMEL_BOUNDARY_RE = re.compile(r"(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])")


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_text(text: str) -> str:
    return sha256_bytes(text.encode("utf-8"))


def canonical_json_bytes(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def text_record(text: str) -> dict[str, int | str]:
    encoded = text.encode("utf-8")
    return {
        "sha256": sha256_bytes(encoded),
        "bytes": len(encoded),
        "chars": len(text),
        "lines": len(text.splitlines()),
    }


def file_record(path: Path | str | None) -> dict[str, Any]:
    if path is None:
        return {"path": None, "exists": False}

    file_path = Path(path)
    record: dict[str, Any] = {
        "path": str(file_path),
        "exists": file_path.is_file(),
    }
    if not file_path.is_file():
        return record

    data = file_path.read_bytes()
    record.update(
        {
            "sha256": sha256_bytes(data),
            "bytes": len(data),
        }
    )
    return record


def directory_tree_record(path: Path | str | None) -> dict[str, Any]:
    if path is None:
        return {"path": None, "exists": False}

    root = Path(path)
    record: dict[str, Any] = {
        "path": str(root),
        "exists": root.is_dir(),
    }
    if not root.is_dir():
        return record

    entries: list[dict[str, Any]] = []
    for child in sorted(root.rglob("*")):
        if not child.is_file():
            continue
        relative_path = child.relative_to(root).as_posix()
        file_data = child.read_bytes()
        entries.append(
            {
                "path": relative_path,
                "sha256": sha256_bytes(file_data),
                "bytes": len(file_data),
            }
        )

    record.update(
        {
            "file_count": len(entries),
            "sha256": sha256_bytes(canonical_json_bytes(entries)),
            "files": entries,
        }
    )
    return record


def _is_sensitive_key(key: str) -> bool:
    """Return whether *key* names a credential that must never be recorded.

    Separators are folded alike, so ``apiKey``, ``APIKey``, ``X-API-Key`` and
    ``OPENAI_API_KEY`` all normalise to ``api_key``. The previous splitter
    inserted an underscore before *every* capital, so all-caps and hyphenated
    keys reached the matcher as one letter per underscore
    (``o_p_e_n_a_i__a_p_i__k_e_y``) and were recorded verbatim.
    """
    snake = _CAMEL_BOUNDARY_RE.sub("_", key)
    normalized = re.sub(r"[^a-z0-9]+", "_", snake.lower()).strip("_")
    return normalized in _SENSITIVE_KEY_FORMS or normalized.endswith(
        _SENSITIVE_KEY_SUFFIXES
    )


def _redact_sensitive(value: Any, *, key: str = "") -> Any:
    if key and _is_sensitive_key(key):
        return _REDACTED
    if isinstance(value, dict):
        return {
            str(child_key): _redact_sensitive(child_value, key=str(child_key))
            for child_key, child_value in value.items()
        }
    if isinstance(value, list):
        return [_redact_sensitive(item) for item in value]
    return value


def redacted_json_file(path: Path | str | None) -> dict[str, Any] | None:
    if path is None:
        return None
    file_path = Path(path)
    if not file_path.is_file():
        return None
    try:
        parsed: Any = json.loads(file_path.read_text(encoding="utf-8"))
        redacted = _redact_sensitive(parsed)
    except (OSError, json.JSONDecodeError):
        logger.warning("INPUT_MANIFEST_JSON_REDACTION_FAILED file=%s", file_path)
        return None
    if not isinstance(redacted, dict):
        return {"value": redacted}
    return cast(dict[str, Any], redacted)
