"""Apply strict result policy before Hermes persists each tool history entry."""

from contextvars import ContextVar
from functools import wraps
import json
import logging
import subprocess
from typing import Any, NamedTuple

LOG = logging.getLogger(__name__)
WITHHELD = "AW result withheld"


class _Batch(NamedTuple):
    agent: Any
    task_id: str
    outcomes: dict


_batch = ContextVar("aw_hermes_result_batch", default=None)


def remember_post(**values):
    """Keep the native outcome alongside its call until the executor commits the batch."""
    batch = _batch.get()
    if batch is not None:
        key = (values.get("session_id"), values.get("api_request_id"), values.get("tool_call_id"))
        batch.outcomes[key] = {k: values[k] for k in (
            "status", "error_type", "error_message", "duration_ms", "middleware_trace"
        ) if k in values}


def select_result(hooks, payload, invoke):
    result = payload["extra"]["result"]
    withheld = False
    for hook in hooks:
        if hook["event"] != "tool.after":
            continue
        strict = hook["on_error"] == "withhold_result"
        try:
            reply = json.loads(invoke(hook))
            if reply.get("aw_withheld") is True:
                withheld = True
            elif set(reply) == {"aw_replacement"} and isinstance(reply["aw_replacement"], str):
                result = reply["aw_replacement"]
            elif reply != {}:
                raise ValueError("invalid Hermes result directive")
        except Exception as error:
            LOG.warning("AW Hermes result check failed (%s), action=%s",
                        type(error).__name__, hook["on_error"])
            withheld |= strict
    return WITHHELD if withheld else result


def evaluate(launch, binding, payload):
    def invoke(hook):
        return subprocess.run(
            [launch["binary"], "hook", "--binding", binding, "--adapter", "hermes",
             "--event", "tool.after", "--step", hook["step"], "--on-error", hook["on_error"]],
            input=json.dumps(payload, allow_nan=False).encode(), capture_output=True,
            timeout=hook["timeout"], check=True).stdout

    return select_result(launch["hooks"], payload, invoke)


def install(evaluate_result):
    from agent import tool_executor

    def scoped_batch(original):
        @wraps(original)
        def run(agent, assistant_message, messages, effective_task_id, *args, **kwargs):
            # Hermes propagates this context to tool workers and native hook callbacks.
            token = _batch.set(_Batch(agent, effective_task_id, {}))
            try:
                return original(agent, assistant_message, messages, effective_task_id, *args, **kwargs)
            finally:
                _batch.reset(token)
        return run

    def checked(agent, task_id, call_id, name, args, result, status):
        try:
            ids = tool_executor.tool_hook_ids(agent, task_id, call_id)
            outcome = {"status": status}
            batch = _batch.get()
            if batch is not None:
                key = (ids["session_id"], ids["api_request_id"], ids["tool_call_id"])
                outcome.update(batch.outcomes.pop(key, {}))
            return evaluate_result(ids, name, args, result, outcome)
        except Exception as error:
            LOG.warning("AW Hermes result scope unavailable (%s)", type(error).__name__)
            return WITHHELD

    original_commit = tool_executor._commit_tool_result
    original_message = tool_executor.make_tool_result_message

    def commit(agent, messages, ref, function_result, **options):
        status = "blocked" if options["blocked"] else "error" if options["is_error"] else "ok"
        result = checked(agent, ref.task_id, ref.call_id, ref.name, ref.args, function_result, status)
        # The regular commit checks before spill. Its message constructor must not check twice.
        token = _batch.set(None)
        try:
            return original_commit(agent, messages, ref, result, **options)
        finally:
            _batch.reset(token)

    def message(name, content, tool_call_id, **options):
        batch = _batch.get()
        if batch is not None:
            # Only parse errors and skipped slots construct messages outside the regular
            # commit. Skipped slots carry disposition "none" and may emit post after append.
            status = "cancelled" if options.get("effect_disposition") == "none" else "error"
            content = checked(batch.agent, batch.task_id, tool_call_id, name, {}, content, status)
        return original_message(name, content, tool_call_id, **options)

    wrappers = {"_commit_tool_result": commit, "make_tool_result_message": message}
    for name in ("execute_tool_calls_sequential", "execute_tool_calls_concurrent"):
        wrappers[name] = scoped_batch(getattr(tool_executor, name))
    restores = [(tool_executor, name, getattr(tool_executor, name), wrapper)
                for name, wrapper in wrappers.items()]
    for module, name, _, wrapper in restores:
        setattr(module, name, wrapper)
    return restores


def payload(cwd, ids, name, args, result, outcome):
    return {"hook_event_name": "transform_tool_result", "cwd": cwd,
            "tool_name": name, "tool_input": args, "session_id": ids["session_id"],
            "extra": {**ids, **outcome, "result": result}}


def install_bound(resolve):
    def final(ids, name, args, result, outcome):
        binding = resolve(ids)
        if binding is None or not binding.final_result:
            return result
        binding.check()
        return binding.evaluate(payload(binding.cwd, ids, name, args, result, outcome))
    return install(final)
