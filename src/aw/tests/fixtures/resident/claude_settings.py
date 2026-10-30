"""Exercise native model settings precedence through real resident tool turns."""

import asyncio
from dataclasses import replace
import json
from pathlib import Path


def configure(options, directory, origin, endpoint):
    settings = {"env": {
        "ANTHROPIC_BASE_URL": endpoint,
        "ANTHROPIC_API_KEY": "native-model-fixture",
        "AW_NATIVE_CONFIG": "retained",
    }}
    # A direct environment fallback cannot reach the model or supply its credential.
    environment = {**options.env, "ANTHROPIC_BASE_URL": "http://127.0.0.1:1",
                   "ANTHROPIC_API_KEY": "wrong-fixture-key"}
    options = replace(options, env=environment, setting_sources=["user", "project", "local"])
    files = {}
    if origin == "inline":
        options = replace(options, settings=json.dumps(settings))
    else:
        path = {
            "file": directory / "model-settings.json",
            "user": Path(environment["CLAUDE_CONFIG_DIR"]) / "settings.json",
            "project": directory / ".claude/settings.json",
            "local": directory / ".claude/settings.local.json",
            "default": directory / ".claude/settings.local.json",
        }[origin]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(settings))
        files[path] = path.read_bytes()
        if origin == "file":
            options = replace(options, settings="model-settings.json")
        elif origin == "default":
            options = replace(options, setting_sources=None)
    return options, files


async def run(root, task, results):
    origins = ["inline", "file", "user", "project", "local", "default"]
    for index, origin in enumerate(origins):
        await task(index, "allow", settings_origin=origin)
    # Separate native sources and upstreams must remain isolated in one host.
    await asyncio.gather(
        task(6, "allow", settings_origin="inline"),
        task(7, "allow", settings_origin="project"),
    )
    assert len({case["instance"] for case in results}) == 8
    (root / "result.json").write_text(json.dumps({
        "tasks": 8, "passed": True, "settings_sources": origins,
        "concurrent_settings_clients": 2, "cases": results,
    }))
