"""Build native settings and plugin assets for the real bundled CLI fixture."""

from dataclasses import replace
import json
import os
from pathlib import Path
import shlex
import sys


def configure(options, root, mode):
    if mode.startswith("bash-native-rewrite-"):
        script = root / "native-rewrite.py"
        script.write_text("import json\nprint(json.dumps({'hookSpecificOutput': {"
                          "'hookEventName': 'PreToolUse', 'updatedInput': {"
                          "'command': 'printf rewritten > rewritten-effect'}}}))\n")
        hooks = {"PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": shlex.join([sys.executable, str(script)])}]}]}
        source = mode.removesuffix("-deny").rsplit("-", 1)[1]
        if source == "inline":
            return replace(options, settings=json.dumps({"hooks": hooks}))
        if source == "project":
            settings = root / ".claude/settings.json"
            settings.parent.mkdir()
            settings.write_text(json.dumps({"hooks": hooks}))
            return replace(options, setting_sources=["project"])
        assert source == "plugin", mode
        options = configure(options, root, "plugins")
        path = Path(options.plugins[0]["path"]) / "hooks/hooks.json"
        path.parent.mkdir()
        path.write_text(json.dumps({"hooks": hooks}))
        return options
    if mode.startswith("settings"):
        script = root / "native-settings-hook.py"
        script.write_text(
            "import json, os\nfrom pathlib import Path\n"
            "assert os.environ['AW_NATIVE_USER'] == 'user'\n"
            "assert os.environ['AW_NATIVE_PROJECT'] == 'project'\n"
            "assert os.environ['AW_NATIVE_LOCAL'] == 'local'\n"
            "assert os.environ['AW_NATIVE_INLINE'] == 'inline'\n"
            "Path('native-settings-hook').touch()\n"
            + ("print(json.dumps({'hookSpecificOutput': {'hookEventName': 'PreToolUse', "
               "'permissionDecision': 'deny', 'permissionDecisionReason': 'native settings veto'}}))\n"
               if mode == "settings-native-deny" else "print('{}')\n")
        )
        project = root / ".claude"
        project.mkdir()
        user = Path(os.environ["CLAUDE_CONFIG_DIR"])
        user.mkdir(parents=True)
        for path, scope in ((user / "settings.json", "USER"),
                            (project / "settings.json", "PROJECT"),
                            (project / "settings.local.json", "LOCAL")):
            value = {"env": {f"AW_NATIVE_{scope}": scope.lower()}}
            if scope == "PROJECT":
                value["hooks"] = {"PreToolUse": [{"matcher": "mcp__local__touch", "hooks": [
                    {"type": "command", "command": shlex.join([sys.executable, str(script)])}]}]}
            path.write_text(json.dumps(value))
        skill = project / "skills/aw-native-fixture/SKILL.md"
        skill.parent.mkdir(parents=True)
        skill.write_text("---\nname: aw-native-fixture\ndescription: Native discovery fixture\n---\nReturn the fixture text.\n")
        settings = {"env": {"AW_NATIVE_INLINE": "inline"}}
        if mode == "settings-disabled-hooks":
            settings["disableAllHooks"] = True
        return replace(options, setting_sources=["user", "project", "local"],
                       settings=json.dumps(settings))
    if mode == "plugins":
        plugin = root / "fixture-plugin"
        metadata = plugin / ".claude-plugin/plugin.json"
        metadata.parent.mkdir(parents=True)
        metadata.write_text(json.dumps({"name": "aw-native-fixture", "version": "1.0.0"}))
        command = plugin / "commands/fixture.md"
        command.parent.mkdir()
        command.write_text("---\ndescription: Native plugin discovery fixture\n---\nReturn the fixture text.\n")
        return replace(options, plugins=[{"type": "local", "path": str(plugin)}])
    return options
