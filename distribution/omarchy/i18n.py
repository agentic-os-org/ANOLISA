"""Share explicit user language preferences between the CLI and desktop view."""

import json
import os
from functools import cache
from pathlib import Path

DEFAULT_LANGUAGE = "en-US"
LANGUAGES = (DEFAULT_LANGUAGE, "zh-CN")


def preference_path() -> Path:
    config = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
    return config / "anolisa-omarchy/ui.json"


def language() -> str:
    # setup translates its own errors, so load its private-file helpers after startup.
    from setup import read_json

    try:
        settings = read_json(preference_path()) or {}
    except (OSError, ValueError):
        # An invalid display preference must not prevent setup or account recovery.
        return DEFAULT_LANGUAGE
    value = settings.get("language", DEFAULT_LANGUAGE)
    return value if value in LANGUAGES else DEFAULT_LANGUAGE


def set_language(value: str) -> None:
    """Write a separate private preference without rewriting account configuration."""
    from setup import atomic_write, json_bytes, private_directory

    if value not in LANGUAGES:
        raise ValueError("Supported languages: en-US, zh-CN")
    path = preference_path()
    private_directory(path.parent)
    atomic_write(path, json_bytes({"schema": 1, "language": value}))


@cache
def catalog() -> dict[str, dict[str, str]]:
    return json.loads(Path(__file__).with_name("translations.json").read_text(encoding="utf-8"))


def tr(message: str, *values: object) -> str:
    text = catalog()["cli"].get(message, message) if language() == "zh-CN" else message
    return text.format(*values) if values else text


def ui_state() -> dict:
    selected = language()
    return {"language": selected, "strings": catalog()["ui"] if selected == "zh-CN" else {}}
