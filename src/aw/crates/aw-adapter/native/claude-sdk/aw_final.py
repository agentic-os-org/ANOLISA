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
        self.enforcing = config.get("final_enforcing") is not False
        self.root = Path(config["ready_path"]).parent
        self.records = {}
        self.snapshot = None
        self.closed = False
        self.prefixes = ()

    def options(self, options, bundled):
        if (not isinstance(options.model, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]+", options.model)
                or options.model in {"auto", "default", "sonnet", "opus", "haiku"}
                or getattr(options, "fallback_model", None)):
            raise ValueError("AW final gate requires an explicit model ID without fallback")
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
        if (parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.username or parsed.password
                or not re.fullmatch(r"[A-Za-z0-9/_-]*", parsed.path) or parsed.query or parsed.fragment):
            raise ValueError("AW final gate requires an HTTP(S) Messages base URL without credentials or query")
        runtime = {**self.config, "origin": f"{parsed.scheme}://{parsed.netloc}",
                   "messages_path": parsed.path + "/v1/messages",
                   "cwd": str(Path(options.cwd or os.getcwd()).resolve())}
        (self.root / "claude-final-runtime.json").write_text(json.dumps(runtime))
        self._publish(self.records)
        preload = self.root / "claude-final-preload.mjs"
        if any(character.isspace() for character in str(preload)):
            raise ValueError("AW final preload path must not contain whitespace")
        if self.enforcing:
            options = self._enforcing_options(options)
        return replace(options, env={**options.env,
            "BUN_OPTIONS": f"--preload={preload}",
            "AW_CLAUDE_FINAL_CONFIG": str(self.root / "claude-final-runtime.json")})

    def _enforcing_options(self, options):
        def wrap(callback):
            async def checked(payload, call, context):
                result = await callback(payload, call, context)
                specific = result.get("hookSpecificOutput", {})
                if specific.get("additionalContext") or result.get("systemMessage"):
                    raise ValueError("AW final gate does not cover Hook context injection")
                return result
            return checked

        hooks = {event: [replace(matcher, hooks=[wrap(callback) for callback in matcher.hooks])
                         for matcher in matchers]
                 for event, matchers in (options.hooks or {}).items()}
        return replace(options, hooks=hooks, env={**options.env, "CLAUDE_CODE_DISABLE_BACKGROUND_TASKS": "1"})

    def supports_tool(self, payload):
        name = payload.get("tool_name", "")
        tool_input = payload.get("tool_input", {})
        return (isinstance(name, str) and (name == "Bash" or name.startswith(self.prefixes))
                and isinstance(tool_input, dict) and not tool_input.get("run_in_background"))

    def record(self, payload):
        call = payload.get("tool_use_id")
        if not self.enforcing and not self.supports_tool(payload):
            return
        try:
            if not isinstance(call, str) or not call:
                raise ValueError("AW final gate native identity limit")
            duplicate = call in self.records
            if len(self.records) >= 1024 and not (self.enforcing and duplicate):
                raise ValueError("AW final gate native identity limit")
            if self.enforcing and duplicate:
                value = {**self.records[call], "ambiguous": True}
            else:
                value = {key: payload.get(key) for key in
                         ("session_id", "cwd", "tool_use_id", "tool_name", "tool_input")}
                if duplicate:
                    value["ambiguous"] = True
                value = json.loads(json.dumps(value))
            self._publish({**self.records, call: value})
        except (OSError, ValueError, TypeError, RecursionError):
            if self.enforcing:
                raise

    def _publish(self, records):
        if self.closed:
            raise ValueError("AW final identity snapshot is closed")
        temporary = self.root / "claude-final-records.tmp"
        destination = self.root / "claude-final-records.json"
        if self.enforcing:
            self._revoke_snapshot()
            self.records = records
            snapshot = temporary.open("w", encoding="utf-8")
            try:
                json.dump({"pid": os.getpid(), "fd": snapshot.fileno(), "records": records}, snapshot)
                snapshot.flush()
                temporary.replace(destination)
            except BaseException:
                snapshot.close()
                raise
            self.snapshot = snapshot
            return
        try:
            temporary.write_text(json.dumps(records))
            temporary.replace(destination)
        except OSError:
            return
        else:
            self.records = records

    def close(self):
        self.closed = True
        self._revoke_snapshot()

    def _revoke_snapshot(self):
        if self.snapshot is not None:
            snapshot, self.snapshot = self.snapshot, None
            snapshot.close()

    def ready(self):
        value = json.loads((self.root / "claude-final-ready.json").read_text())
        if value.get("token") != self.config["ready_token"] or value.get("profile") != self.config["final_profile"]:
            raise ValueError("AW final gate runtime readiness mismatch")
