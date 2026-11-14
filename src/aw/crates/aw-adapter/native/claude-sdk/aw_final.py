"""Bind the private runtime preload to one foreground SDK client."""

from dataclasses import replace
import hashlib
import json
import os
from pathlib import Path
import re
from urllib.parse import urlparse


class FinalGate:
    def __init__(self, config):
        self.config = config
        self.root = Path(config["ready_path"]).parent
        self.records = {}
        self.prefixes = ()

    def options(self, options, bundled):
        if not isinstance(options.model, str) or not re.fullmatch(r"claude-[A-Za-z0-9._-]+", options.model) or getattr(options, "fallback_model", None):
            raise ValueError("AW final gate requires an explicit Claude model without fallback")
        if (os.environ.get("AW_CLAUDE_RESIDENT") == "1" or options.setting_sources != []
                or options.settings is not None or options.resume or options.continue_conversation
                or options.agents or options.plugins or not isinstance(options.tools, list)
                or any(tool != "Bash" for tool in options.tools)):
            raise ValueError("AW final gate requires a fresh foreground client with setting_sources=[]")
        if not isinstance(options.mcp_servers, dict) or any(
            not isinstance(server, dict) or server.get("type") != "sdk"
            for server in options.mcp_servers.values()
        ):
            raise ValueError("AW final gate currently covers only in-process SDK MCP servers")
        self.prefixes = tuple(f"mcp__{name}__" for name in options.mcp_servers)
        env = {**os.environ, **options.env}
        if any(env.get(key) for key in ("BUN_OPTIONS", "NODE_OPTIONS")) or any(
            value and key.startswith("CLAUDE_CODE_USE_") for key, value in env.items()
        ):
            raise ValueError("AW final gate does not support runtime or provider overrides")
        for name, expected in self.config["final_profile"]["sha256"].items():
            if hashlib.sha256((bundled.parent.parent / name).read_bytes()).hexdigest() != expected:
                raise ValueError("AW final gate SDK fingerprint changed after admission")
        base = env.get("ANTHROPIC_BASE_URL", "https://api.anthropic.com").rstrip("/")
        parsed = urlparse(base)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.path or parsed.query or parsed.fragment:
            raise ValueError("AW final gate requires an HTTP(S) Anthropic origin")
        runtime = {**self.config, "origin": base, "cwd": str(Path(options.cwd or os.getcwd()).resolve())}
        (self.root / "claude-final-runtime.json").write_text(json.dumps(runtime))
        self._save()
        preload = self.root / "claude-final-preload.mjs"
        if any(character.isspace() for character in str(preload)):
            raise ValueError("AW final preload path must not contain whitespace")
        hooks = {}
        for event, matchers in (options.hooks or {}).items():
            hooks[event] = []
            for matcher in matchers:
                callbacks = []
                for callback in matcher.hooks:
                    async def checked(payload, call, context, callback=callback):
                        result = await callback(payload, call, context)
                        specific = result.get("hookSpecificOutput", {})
                        if specific.get("additionalContext") or result.get("systemMessage"):
                            raise ValueError("AW final gate does not cover Hook context injection")
                        return result
                    callbacks.append(checked)
                hooks[event].append(replace(matcher, hooks=callbacks))
        return replace(options, hooks=hooks, env={**options.env,
            "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS": "1",
            "BUN_OPTIONS": f"--preload={preload}",
            "AW_CLAUDE_FINAL_CONFIG": str(self.root / "claude-final-runtime.json")})

    def supports_tool(self, payload):
        name = payload.get("tool_name", "")
        return (name == "Bash" or name.startswith(self.prefixes)) and not payload.get(
            "tool_input", {}).get("run_in_background")

    def record(self, payload):
        call = payload.get("tool_use_id")
        if not isinstance(call, str) or not call or len(self.records) >= 1024:
            raise ValueError("AW final gate native identity limit")
        value = {key: payload.get(key) for key in
                 ("session_id", "cwd", "tool_use_id", "tool_name", "tool_input")}
        if call in self.records:
            value["ambiguous"] = True
        self.records[call] = json.loads(json.dumps(value))
        self._save()

    def _save(self):
        temporary = self.root / "claude-final-records.tmp"
        temporary.write_text(json.dumps(self.records))
        temporary.replace(self.root / "claude-final-records.json")

    def ready(self):
        value = json.loads((self.root / "claude-final-ready.json").read_text())
        if value.get("token") != self.config["ready_token"] or value.get("profile") != self.config["final_profile"]:
            raise ValueError("AW final gate runtime readiness mismatch")
