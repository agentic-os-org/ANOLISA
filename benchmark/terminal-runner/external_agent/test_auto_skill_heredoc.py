# Copyright 2026 Alibaba Cloud
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#     http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

"""_auto_skill_from_solve must recognize every legal heredoc form.

Only ``cmd <<'MARK' > file`` was recognized. The append form (``>>``)
captured the target file as literally ``>``; the redirect-before form and
the bare/tee form were not recognized at all, so every heredoc BODY line
leaked into the Approach steps as a standalone command — teaching the
benchmark agent broken commands.
"""

from pathlib import Path

from openclaw_external_agent import OpenClawExternalAgent


def _skill(solve: str, tmp_path: Path) -> str:
    solve_path = tmp_path / "solve.sh"
    solve_path.write_text(solve, encoding="utf-8")
    return OpenClawExternalAgent._auto_skill_from_solve(str(solve_path))


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
