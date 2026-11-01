"""Regression tests for sensitive-directory rules (issue #5902).

Directory-style entries in ``rules/bash/_shared.yaml`` and
``rules/python/_shared.yaml`` used to end with a literal ``/``, so the most
idiomatic spelling of the path (``cd /etc/ssh``) never matched.  The entries
now use a word boundary (``/etc/ssh\\b``) and must keep matching both the bare
directory and anything inside it, without matching look-alike names such as
``/etc/sshfs``.
"""

import pytest

from agent_sec_cli.code_scanner.models import Language, Verdict
from agent_sec_cli.code_scanner.scanner import scan

SENSITIVE_DIR_RULES = {
    "shell-cd-sensitive-dir",
    "shell-read-sensitive-file",
    "shell-tamper-sensitive-file",
}


def _rule_ids(code: str, language: Language) -> set[str]:
    result = scan(code, language)
    assert result.ok, result.summary
    return {finding.rule_id for finding in result.findings}


@pytest.mark.parametrize(
    "directory",
    [
        "/etc/ssh",
        "/etc/pam.d",
        "/etc/security",
        "/etc/sysctl.d",
        "/boot",
        "/usr/lib/systemd",
        "/etc/systemd/system",
        "/etc/cron.d",
        "/var/spool/cron",
        "/etc/kubernetes",
    ],
)
def test_cd_into_sensitive_directory_without_trailing_slash(directory: str) -> None:
    """The bare directory spelling must be flagged, just like the trailing-slash one."""
    assert "shell-cd-sensitive-dir" in _rule_ids(f"cd {directory}", Language.BASH)


@pytest.mark.parametrize(
    "command",
    [
        "cd /etc/ssh/",
        "cp -r /etc/ssh /tmp/backup",
        "tar -czf /tmp/b.tgz /etc/kubernetes",
        "rsync -a /boot /tmp/boot-copy",
    ],
)
def test_slash_suffixed_and_nested_references_still_flagged(command: str) -> None:
    assert _rule_ids(command, Language.BASH) & SENSITIVE_DIR_RULES


@pytest.mark.parametrize(
    "code",
    [
        "open('/etc/ssh/id_rsa')\n",
        "open('/etc/pam.d/common-auth')\n",
    ],
)
def test_python_sensitive_directory_is_detected(code: str) -> None:
    assert "py-sensitive-file-access" in _rule_ids(code, Language.PYTHON)


@pytest.mark.parametrize(
    "directory",
    [
        "/etc/sshfs",
        "/etc/securityx",
        "/etc/kubernetesx",
        "/bootleg",
        "/etc/cron.deploy",
    ],
)
def test_lookalike_directory_is_not_a_sensitive_path(directory: str) -> None:
    """The word boundary must not turn prefix look-alikes into matches."""
    assert "shell-cd-sensitive-dir" not in _rule_ids(f"cd {directory}", Language.BASH)
    assert scan(f"cd {directory}", Language.BASH).verdict is Verdict.PASS
