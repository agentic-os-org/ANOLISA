"""Regressions for skill hints generated from shell solutions."""

import importlib.util
import sys
import types
from pathlib import Path
from unittest.mock import patch


def _load_agent():
    source = Path(__file__).parents[1] / "external_agent" / "openclaw_external_agent.py"
    modules = {}
    for name in (
        "harbor",
        "harbor.agents",
        "harbor.agents.base",
        "harbor.environments",
        "harbor.environments.base",
        "harbor.models",
        "harbor.models.agent",
        "harbor.models.agent.context",
    ):
        modules[name] = types.ModuleType(name)
    modules["harbor.agents.base"].BaseAgent = object
    modules["harbor.environments.base"].BaseEnvironment = object
    modules["harbor.models.agent.context"].AgentContext = object
    spec = importlib.util.spec_from_file_location(
        "terminal_runner_openclaw_agent", source
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    with patch.dict(sys.modules, modules):
        spec.loader.exec_module(module)
    return module.OpenClawExternalAgent


def test_auto_skill_preserves_quoted_hashes_and_removes_real_comments(tmp_path):
    solve_path = tmp_path / "sample-task" / "solution" / "solve.sh"
    solve_path.parent.mkdir(parents=True)
    commands = [
        'echo "revenue #1 in Q3" > report.txt',
        "sed 's/old #value/new/' config.ini",
        'echo "escaped \\" # still quoted"',
        r"echo escaped\ #value",
        r"echo escaped\#value",
        'echo "quoted #value" # remove this comment',
        "echo keep # remove this comment",
        "echo tab\t# remove this too",
        "echo after # remove this comment",
    ]
    solve_path.write_text("\n".join(commands), encoding="utf-8")

    hint = _load_agent()._auto_skill_from_solve(str(solve_path))

    for command in commands[:5]:
        assert command in hint
    assert 'echo "quoted #value"`' in hint
    assert 'echo "quoted #value" # remove' not in hint
    assert "echo keep # remove" not in hint
    assert "echo tab\t# remove" not in hint
    assert "echo after # remove" not in hint
    assert "echo keep`" in hint
    assert "echo tab`" in hint
    assert "echo after`" in hint


def test_auto_skill_keeps_heredoc_body_unchanged(tmp_path):
    solve_path = tmp_path / "sample-task" / "solution" / "solve.sh"
    solve_path.parent.mkdir(parents=True)
    solve_path.write_text(
        "cat <<'EOF' > output.txt\nquoted # content\nEOF\n", encoding="utf-8"
    )

    hint = _load_agent()._auto_skill_from_solve(str(solve_path))

    assert "quoted # content" in hint


def test_auto_skill_preserves_hash_after_non_breaking_space(tmp_path):
    solve_path = tmp_path / "sample-task" / "solution" / "solve.sh"
    solve_path.parent.mkdir(parents=True)
    command = "echo left\u00a0#right"
    solve_path.write_text(command + "\n", encoding="utf-8")

    agent = _load_agent()
    with patch("builtins.open", side_effect=lambda path: Path(path).open(encoding="utf-8")):
        hint = agent._auto_skill_from_solve(str(solve_path))

    assert f"`{command}`" in hint
