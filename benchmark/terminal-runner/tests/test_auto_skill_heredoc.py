"""_auto_skill_from_solve must recognize every legal heredoc form.

Only ``cmd <<'MARK' > file`` was recognized before. The append form
(``>>``) captured the target file as literally ``>``; the redirect-before
form and the bare/tee form were not recognized at all, so every heredoc
BODY line leaked into the Approach steps as a standalone command —
teaching the benchmark agent broken commands. The operator is only a
heredoc when it sits outside quotes, and only known writers (tee) infer a
file target: an interpreter reading stdin (``python3 -``) is a run step,
not a write.
"""

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
        "terminal_runner_openclaw_agent_heredoc", source
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    with patch.dict(sys.modules, modules):
        spec.loader.exec_module(module)
    return module.OpenClawExternalAgent


def _skill(solve: str, tmp_path: Path) -> str:
    solve_path = tmp_path / "solve.sh"
    solve_path.write_text(solve, encoding="utf-8")
    return _load_agent()._auto_skill_from_solve(str(solve_path))


class TestHeredocForms:
    def test_append_redirect_targets_the_file(self, tmp_path):
        skill = _skill(
            "#!/bin/bash\n"
            "cat <<'EOF' >> /var/log/app.log\n"
            "line1\n"
            "line2\n"
            "EOF\n"
            "systemctl restart app\n",
            tmp_path,
        )
        assert "Write /var/log/app.log" in skill
        assert "Write >" not in skill

    def test_append_body_not_leaked_as_commands(self, tmp_path):
        skill = _skill(
            "#!/bin/bash\n"
            "cat <<'EOF' >> /var/log/app.log\n"
            "line1\n"
            "line2\n"
            "EOF\n",
            tmp_path,
        )
        assert "`line1`" not in skill and "`line2`" not in skill

    def test_redirect_before_heredoc(self, tmp_path):
        skill = _skill(
            "#!/bin/bash\n"
            "sudo tee /etc/yum.repos.d/alinux.repo <<'EOF'\n"
            "[alinux]\n"
            "baseurl=https://example.com/repo\n"
            "EOF\n"
            "dnf makecache\n",
            tmp_path,
        )
        assert "etc/yum.repos.d/alinux.repo" in skill.replace("\\", "/")
        assert "`[alinux]`" not in skill
        assert "`baseurl=" not in skill
        assert "`EOF`" not in skill

    def test_plain_redirect_form_still_works(self, tmp_path):
        skill = _skill(
            "#!/bin/bash\n"
            "cat <<'EOF' > /tmp/out.txt\n"
            "payload\n"
            "EOF\n",
            tmp_path,
        )
        assert "Write /tmp/out.txt" in skill
        assert "`payload`" not in skill

    def test_bare_heredoc_body_not_leaked(self, tmp_path):
        skill = _skill(
            "#!/bin/bash\n"
            "python3 - <<'PYEOF'\n"
            "import sys\n"
            "sys.exit(0)\n"
            "PYEOF\n"
            "echo done\n",
            tmp_path,
        )
        assert "`import sys`" not in skill
        assert "`sys.exit(0)`" not in skill
        assert "echo done" in skill

    def test_quoted_operator_is_data_not_heredoc(self, tmp_path):
        """echo "<<'EOF'" quotes the operator: it is data, not a heredoc.

        Matching it would swallow the following commands as body and
        mislabel the step as a write.
        """
        skill = _skill(
            "#!/bin/bash\n"
            "echo \"<<'EOF'\"\n"
            "echo next\n"
            "echo last\n",
            tmp_path,
        )
        assert "with heredoc" not in skill
        assert "`echo next`" in skill
        assert "`echo last`" in skill

    def test_bare_heredoc_interpreter_not_a_write(self, tmp_path):
        """python3 - reads the heredoc on stdin: no file target, and the
        receiving command must survive in the step instead of a bogus
        "Write -" label.
        """
        skill = _skill(
            "#!/bin/bash\n"
            "python3 - <<'PYEOF'\n"
            "print('hi')\n"
            "PYEOF\n"
            "echo done\n",
            tmp_path,
        )
        assert "Write -" not in skill
        assert "python3 -" in skill
        assert "Provide heredoc input to `python3 -`:" in skill
        assert "print('hi')" in skill
        assert "`echo done`" in skill

    def test_bare_heredoc_tee_infers_target_only_for_writes(self, tmp_path):
        """The bare form infers a file target only for known writers —
        sudo tee writes the named file; an interpreter flag word is never
        a target.
        """
        skill = _skill(
            "#!/bin/bash\n"
            "sudo tee /etc/yum.repos.d/alinux2.repo <<'EOF'\n"
            "[alinux]\n"
            "EOF\n"
            "dnf makecache\n",
            tmp_path,
        )
        assert "etc/yum.repos.d/alinux2.repo" in skill.replace("\\", "/")
        assert "`dnf makecache`" in skill
