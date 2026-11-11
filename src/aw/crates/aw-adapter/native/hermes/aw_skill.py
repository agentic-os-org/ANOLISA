"""Pin a native local Skill selection across admission and instruction rendering."""

from contextlib import contextmanager
from contextvars import ContextVar
from copy import copy
from functools import wraps
from pathlib import Path
import os
import json
import stat


_selection = ContextVar("aw_hermes_skill", default=None)


def file_identity(value):
    return (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)


def identity(path):
    return file_identity(path.stat())


def prepare(arguments):
    from tools import skills_tool as native
    from tools.registry import registry
    failed = {"metadata": {"error_code": "skill_resolution_error"}}
    try:
        entry = registry.get_entry("skill_view")
        if entry is None or entry.handler is not native._skill_view_with_bump:
            return failed
        name = arguments.get("name")
        if (not isinstance(name, str) or not name or len(name) > 1024 or ":" in name
                or arguments.get("file_path") or native._skill_lookup_path_error(name)):
            return failed
        project, directories, _ = native._skill_search_dirs()
        error, directory, path = native._locate_skill(name, None, project, directories)
        if error is not None or directory is None:
            return failed
        directory, path = directory.resolve(strict=True), path.resolve(strict=True)
        if directory == Path("/") or path != directory / "SKILL.md":
            return failed
        before = identity(path)
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
        try:
            metadata = os.fstat(fd)
            if (not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 1024 * 1024
                    or file_identity(metadata) != before):
                return failed
            with os.fdopen(fd, "r", encoding="utf-8-sig", errors="replace", closefd=False) as handle:
                text = handle.read(1024 * 1024 + 1)
            if (len(text.encode("utf-8")) > 1024 * 1024
                    or file_identity(os.fstat(fd)) != before):
                return failed
        finally:
            os.close(fd)
        if identity(path) != before:
            return failed
        return {"metadata": {"name": name, "directory": str(directory), "source": "hermes-local"},
                "path": path, "identity": before, "text": text, "arguments": dict(arguments),
                "active": True, "checked": False, "entry": copy(entry)}
    except Exception:
        return failed


def verify(selection):
    if (not selection.get("active") or "path" not in selection
            or identity(selection["path"]) != selection["identity"]):
        raise OSError("AW Skill source changed or could not be resolved")


@contextmanager
def invocation(tool_name, arguments, dynamic=False):
    if tool_name != "skill_view" or _selection.get() is not None:
        yield
        return
    selected = prepare(arguments)
    selected["checking"] = not dynamic
    token = _selection.set(selected)
    try:
        yield
    finally:
        selected["active"] = False
        _selection.reset(token)


def callback(original, hook):
    @wraps(original)
    def checked(**kwargs):
        if kwargs.get("tool_name") != "skill_view":
            return original(**kwargs)
        selected = _selection.get()
        metadata = {"error_code": "skill_resolution_error"}
        if selected is not None and selected.get("arguments") == kwargs.get("args"):
            metadata = selected["metadata"]
        result = original(**{**kwargs, "aw_skill": metadata})
        checking, action = hook.get("skill_check"), hook["on_error"]
        if hook.get("dynamic"):
            result, action = result
            checking = action is not None
            if selected is not None:
                selected["checking"] = checking
        if isinstance(result, dict) and result.get("action") == "block":
            return result
        if not checking:
            return result
        try:
            if metadata.get("error_code"):
                raise OSError("AW Skill source could not be resolved")
            verify(selected or {})
            selected["checked"] = True
        except OSError:
            if action == "block":
                return {"action": "block", "message": "AW Skill source validation failed"}
        return result
    return checked


def install():
    """Keep native selection and rendering; substitute only the checked text slot."""
    from tools import skills_tool as native
    from tools.registry import registry
    locate, read, get_entry = native._locate_skill, native._read_skill_text, registry.get_entry

    @wraps(locate)
    def selected_source(*args, **kwargs):
        result = locate(*args, **kwargs)
        selected = _selection.get()
        if selected is not None and selected.get("checking", True) and "path" in selected:
            verify(selected)
            if (not selected["checked"] or result[0] is not None or result[1] is None
                    or result[2].resolve() != selected["path"]):
                raise OSError("AW Skill selection differs from its checked source")
            return result[0], selected["path"].parent, selected["path"]
        return result

    @wraps(read)
    def selected_text(path):
        selected = _selection.get()
        if selected is not None and selected.get("checking", True) and Path(path).resolve() == selected.get("path"):
            verify(selected)
            if not selected["checked"]:
                raise OSError("AW Skill source was not checked")
            return selected["text"]
        return read(path)

    @wraps(get_entry)
    def selected_entry(name, **options):
        entry = get_entry(name, **options)
        selected = _selection.get()
        if name == "skill_view" and selected is not None and selected.get("checking", True) and "entry" in selected:
            if entry is None or entry.handler is not selected["entry"].handler:
                raise OSError("AW Skill tool handler changed after selection")
            return copy(selected["entry"])
        return entry

    native._locate_skill, native._read_skill_text = selected_source, selected_text
    registry.get_entry = selected_entry

    def restore():
        native._locate_skill, native._read_skill_text = locate, read
        registry.get_entry = get_entry
    return restore


def dynamic_callback(spec, record_input=None):
    """Consume policy-derived source admission without exporting it to native hooks."""
    from agent.shell_hooks import _evaluate_result, _serialize_payload, _spawn

    def invoke(**kwargs):
        response = _spawn(spec, _serialize_payload(spec.event, kwargs))
        native = _evaluate_result(spec, response)
        if isinstance(native, dict) and native.get("action") == "block":
            return native, None
        try:
            value = json.loads(response["stdout"])
            mode = value["aw_skill_check"]
            if mode not in (None, "report", "block") or response["returncode"] != 0:
                raise ValueError("invalid Skill admission")
        except (ValueError, KeyError, TypeError):
            return {"action": "block", "message": "AW Skill admission is unavailable"}, None
        if record_input is not None:
            record_input(response, native)
        return native, mode

    checked = callback(invoke, {"dynamic": True, "on_error": "block"})

    def dispatch(**kwargs):
        if kwargs.get("tool_name") == "skill_view":
            return checked(**kwargs)
        return invoke(**kwargs)[0]
    return dispatch
