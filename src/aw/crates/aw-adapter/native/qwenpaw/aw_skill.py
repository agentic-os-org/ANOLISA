"""Resolve and pin the native SkillViewer selection for one acting invocation."""

from contextvars import ContextVar
from copy import deepcopy
from functools import wraps
from pathlib import Path
import os


_selection = ContextVar("aw_skill_selection", default=None)


def identity(path):
    value = path.stat()
    return (value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns)


def verify():
    """Reject a source edit across the backend check or before native delivery."""
    selection = _selection.get()
    if selection and selection.get("path") is not None:
        if identity(selection["path"]) != selection["identity"]:
            raise OSError("AW Skill content changed during validation")


def metadata():
    """Expose only the local source selected by the native Toolkit."""
    selection = _selection.get()
    return {"aw_skill": selection["metadata"]} if selection else {}


async def prepare(agent, call):
    from agentscope._utils._common import _json_loads_with_repair
    from agentscope.tool._builtin import SkillViewer
    import frontmatter

    error = {"metadata": {"error_code": "skill_resolution_error"}}
    try:
        groups = list(agent.state.tool_context.activated_groups)
        viewer = await agent.toolkit.check_tool_available(call.name, groups)
        if not isinstance(viewer, SkillViewer):
            return error
        if not getattr(viewer, "_aw_skill_bound", False):
            original = viewer._get_skills_method

            @wraps(original)
            async def pinned(activated_groups):
                selection = _selection.get()
                if selection and selection.get("viewer") is viewer:
                    if list(activated_groups) != selection["groups"]:
                        raise RuntimeError("AW Skill groups changed after validation")
                    verify()
                    # No loader is called again between check and delivery.
                    return deepcopy(selection["skills"])
                return await original(activated_groups)

            viewer._get_skills_method = pinned
            viewer._aw_skill_bound = True
        skills = await viewer._get_skills_method(groups)
        arguments = _json_loads_with_repair(call.input, viewer.input_schema)
        name = arguments.get("skill")
        if not isinstance(name, str) or not name or len(name) > 1024:
            return error
        selected = skills.get(name)
        if selected is None:
            return error
        directory = Path(selected.dir).resolve(strict=True)
        if not directory.is_dir() or directory == Path("/"):
            return error
        path = directory / "SKILL.md"
        before = identity(path)
        fd = os.open(path, os.O_RDONLY | os.O_NONBLOCK)
        try:
            import stat
            file_stat = os.fstat(fd)
            if not stat.S_ISREG(file_stat.st_mode) or file_stat.st_size > 1024 * 1024:
                return error
            with os.fdopen(fd, "r", encoding="utf-8-sig", closefd=False) as handle:
                text = handle.read(1024 * 1024 + 1)
            if len(text.encode("utf-8")) > 1024 * 1024:
                return error
        finally:
            os.close(fd)
        if identity(path) != before or frontmatter.loads(text).content != selected.markdown:
            return error
        return {
            "metadata": {"name": name, "directory": str(directory), "source": "toolkit"},
            "viewer": viewer, "groups": groups, "skills": {name: deepcopy(selected)},
            "path": path, "identity": before,
        }
    except Exception:
        return error


def skill_context(handler):
    """Share one selection across nested AW middleware and Coordinator tasks."""
    @wraps(handler)
    async def wrapped(self, agent, input_kwargs, next_handler):
        call = input_kwargs["tool_call"]
        token = None
        hooks = [self.hook] if hasattr(self, "hook") else self.config.get("hooks", [])
        enabled = any(hook.get("skill_check") is True for hook in hooks)
        if enabled and call.name == "Skill" and _selection.get() is None:
            token = _selection.set(await prepare(agent, call))
        stream = handler(self, agent, input_kwargs, next_handler)
        try:
            async for item in stream:
                yield item
        finally:
            try:
                await stream.aclose()
            finally:
                if token is not None:
                    _selection.reset(token)
    return wrapped
