"""
Unit and regression tests for sandbox command security classification.

Validates that destructive commands cannot evade sandbox DENY policies via
shell redirection, argument placement, sudo flags, pipes, or compound chaining.
"""

from typing import Any, Dict

import pytest

from agent_sec_cli.sandbox.classify_command import CommandClassifier


@pytest.fixture
def classifier() -> CommandClassifier:
    """Fixture providing a fresh CommandClassifier instance."""
    return CommandClassifier()


class TestDestructiveCommandsDirect:
    """Test direct invocation of destructive commands."""

    @pytest.mark.parametrize(
        "cmd",
        [
            "rm -rf /",
            "rm -rf /*",
            "rm -rf /etc",
            "rm -fr /boot",
            "rm -rf /usr",
            "rm -rf /var",
            "rm -f /bin",
            "rm -rf /sbin",
            "rm -rf /lib",
            "rm -rf /lib64",
            "rm -rf /root",
            "rm -rf /home",
            "mkfs.ext4 /dev/sda1",
            "mkfs.xfs /dev/nvme0n1p1",
            "fdisk /dev/sda",
            "parted /dev/sdb",
            "gdisk /dev/sdc",
            "cfdisk /dev/sdd",
            "shutdown -h now",
            "reboot",
            "poweroff",
            "halt",
            "init 0",
            "dd if=/dev/zero of=/dev/sda",
            "dd if=/dev/urandom of=/dev/nvme0n1",
            "kill -9 -1",
            "kill -KILL -1",
            "kill 1",
            ":(){ :|:& };:",
            ":(){ :|: ;",
        ],
    )
    def test_direct_destructive_commands(
        self, classifier: CommandClassifier, cmd: str
    ) -> None:
        result: Dict[str, Any] = classifier.classify(cmd)
        assert (
            result["decision"] == "destructive"
        ), f"Expected {cmd} to be destructive, got {result['decision']}"
        assert result["additional_permissions"] is None


class TestDestructiveEvasionShellRedirection:
    """Test evasion resistance against shell wrappers and I/O redirection."""

    @pytest.mark.parametrize(
        "cmd",
        [
            # Output redirection to /dev/null
            'bash -c "rm -rf / > /dev/null"',
            'bash -c "rm -rf / >> /dev/null"',
            'bash -c "rm -rf / 2>&1"',
            'bash -c "rm -rf / &> /dev/null"',
            'bash -c "rm -rf / >& /dev/null"',
            'bash -c "rm -rf / >| /dev/null"',
            'bash -c "rm -rf / 1>/dev/null 2>/dev/null"',
            'bash -c "rm -rf / 2>/dev/null 1>&2"',
            # Target attached to operator
            'bash -c "rm -rf / >/dev/null"',
            'bash -c "rm -rf / >>/dev/null"',
            'bash -c "rm -rf / &>/dev/null"',
            # Direct redirection on destructive commands
            "rm -rf / > /dev/null",
            "rm -rf / >> /dev/null",
            "rm -rf / 2>&1",
            "rm -rf / &> /dev/null",
            "mkfs.ext4 /dev/sda1 > /dev/null 2>&1",
            "dd if=/dev/zero of=/dev/sda > /tmp/out 2>&1",
            # Block device writing via shell wrapper
            'sh -c "dd if=/dev/zero of=/dev/sda > /tmp/out"',
            'zsh -c "dd if=/dev/zero of=/dev/nvme0n1 > /dev/null"',
            # Shell option combinations
            'bash -ec "rm -rf / > /dev/null"',
            'bash -xc "rm -rf / > /dev/null"',
            'bash -lc "rm -rf / > /dev/null"',
            'bash -elc "rm -rf / > /dev/null"',
            'bash -e -c "rm -rf / > /dev/null"',
            'bash -x -c "rm -rf / > /dev/null"',
            'bash --login -c "rm -rf / > /dev/null"',
            'sh -ec "mkfs.ext4 /dev/sda1 > /dev/null"',
            'zsh -lc "reboot > /dev/null"',
            '/bin/bash -c "poweroff > /dev/null"',
            '/bin/sh -c "shutdown now > /dev/null"',
        ],
    )
    def test_redirection_evasion_blocked(
        self, classifier: CommandClassifier, cmd: str
    ) -> None:
        result: Dict[str, Any] = classifier.classify(cmd)
        assert (
            result["decision"] == "destructive"
        ), f"Expected {cmd} to be destructive, got {result['decision']}"


class TestDestructiveEvasionSudo:
    """Test sudo flag normalization and evasion prevention."""

    @pytest.mark.parametrize(
        "cmd",
        [
            "sudo rm -rf /",
            "sudo -u root rm -rf /",
            "sudo -u root -g wheel rm -rf /",
            "sudo --user=root rm -rf /",
            "sudo -uroot rm -rf /",
            "sudo -E rm -rf /",
            "sudo -H rm -rf /",
            "sudo -n rm -rf /",
            "sudo -E -H -u root rm -rf /",
            "sudo -- rm -rf /",
            "sudo -u root rm -rf / > /dev/null",
            "sudo -E -H -u root rm -rf / > /dev/null 2>&1",
            'sudo -u root bash -c "rm -rf / > /dev/null"',
            'sudo bash -c "dd if=/dev/zero of=/dev/sda > /tmp/out"',
            "sudo mkfs.ext4 /dev/sda1",
            "sudo -u root mkfs.ext4 /dev/sda1",
            "sudo reboot",
            "sudo shutdown -h now",
        ],
    )
    def test_sudo_evasion_blocked(
        self, classifier: CommandClassifier, cmd: str
    ) -> None:
        result: Dict[str, Any] = classifier.classify(cmd)
        assert (
            result["decision"] == "destructive"
        ), f"Expected {cmd} to be destructive, got {result['decision']}"


class TestDestructiveEvasionChainingAndPipes:
    """Test compound command chaining, subshells, and pipelines."""

    @pytest.mark.parametrize(
        "cmd",
        [
            "echo safe ; rm -rf / > /dev/null",
            "echo safe && rm -rf / > /dev/null",
            "false || rm -rf / > /dev/null",
            "cat file | rm -rf /",
            "echo foo | dd if=/dev/zero of=/dev/sda",
            'bash -c "echo 1 && rm -rf / > /dev/null ; ls"',
            'bash -c "cat /dev/zero | dd of=/dev/sda"',
            'bash -c "(rm -rf / > /dev/null)"',
            'sh -c "echo start && reboot || echo end"',
        ],
    )
    def test_chaining_and_pipes_blocked(
        self, classifier: CommandClassifier, cmd: str
    ) -> None:
        result: Dict[str, Any] = classifier.classify(cmd)
        assert (
            result["decision"] == "destructive"
        ), f"Expected {cmd} to be destructive, got {result['decision']}"


class TestDestructiveBlockDeviceRedirection:
    """Test output redirection targeting raw block devices or critical system paths."""

    @pytest.mark.parametrize(
        "cmd",
        [
            "cat /dev/zero > /dev/sda",
            "echo evil > /dev/nvme0n1",
            "echo 0 > /dev/hda",
            "base64 /dev/urandom > /dev/vda",
            "> /dev/sda",
            "cat /tmp/payload > /etc/passwd",
            "echo root::0:0::: > /etc/shadow",
            "cat bad > /boot/vmlinuz",
            'bash -c "cat /dev/zero > /dev/sda"',
            'sh -c "echo evil > /etc/passwd"',
        ],
    )
    def test_block_device_and_system_file_redirection(
        self, classifier: CommandClassifier, cmd: str
    ) -> None:
        result: Dict[str, Any] = classifier.classify(cmd)
        assert (
            result["decision"] == "destructive"
        ), f"Expected {cmd} to be destructive, got {result['decision']}"


class TestDangerousCommands:
    """Test dangerous category commands running in sandbox without privilege expansion."""

    @pytest.mark.parametrize(
        "cmd",
        [
            "rm -rf /tmp/testdir",
            "rm -r ./build",
            "rm -f ./app.log",
            "chmod 755 run.sh",
            "chown root:root run.sh",
            "sed -i 's/foo/bar/g' config.txt",
            "find . -name '*.pyc' -delete",
            "find . -name '*.tmp' -exec rm -f {} +",
            "git clean -fd",
            "sudo chmod 777 /var/log/app.log",
            "sudo -u user1 ls",
            "sudo ls -la",
            "echo secret > .env",
            "echo API_KEY=xyz >> .env.production",
            "cat key > ~/.ssh/id_rsa",
            "cat creds > ~/.aws/credentials",
            'bash -c "echo secret > .env"',
        ],
    )
    def test_dangerous_commands(self, classifier: CommandClassifier, cmd: str) -> None:
        result: Dict[str, Any] = classifier.classify(cmd)
        assert (
            result["decision"] == "dangerous"
        ), f"Expected {cmd} to be dangerous, got {result['decision']}"
        assert result["additional_permissions"] is None


class TestSafeCommands:
    """Test safe read-only commands without side effects."""

    @pytest.mark.parametrize(
        "cmd",
        [
            "ls",
            "ls -la",
            "pwd",
            "whoami",
            "id",
            "cat README.md",
            "head -n 20 file.txt",
            "tail -f log.txt",
            "grep -rn 'search' .",
            "uname -a",
            "git status",
            "git log -n 5",
            "git diff HEAD~1",
            "git branch -a",
            "sed 's/foo/bar/g' file.txt",
            "cat < input.txt",
            "ls > /dev/null",
            "git status 2>&1",
            "pwd > /dev/null 2>&1",
            'bash -c "ls && pwd"',
            'sh -c "whoami ; id"',
        ],
    )
    def test_safe_commands(self, classifier: CommandClassifier, cmd: str) -> None:
        result: Dict[str, Any] = classifier.classify(cmd)
        assert (
            result["decision"] == "safe"
        ), f"Expected {cmd} to be safe, got {result['decision']}"
        assert result["additional_permissions"] is None


class TestDefaultAndPermissions:
    """Test default category with workspace-write and permission grants."""

    def test_file_redirection_falls_to_default(
        self, classifier: CommandClassifier
    ) -> None:
        result = classifier.classify("ls > out.txt")
        assert result["decision"] == "default"
        assert result["additional_permissions"] is None

    def test_echo_redirection_falls_to_default(
        self, classifier: CommandClassifier
    ) -> None:
        result = classifier.classify("echo hello > out.txt")
        assert result["decision"] == "default"
        assert result["additional_permissions"] is None

    def test_npm_install_global_grants_permissions(
        self, classifier: CommandClassifier
    ) -> None:
        result = classifier.classify("npm install -g express")
        assert result["decision"] == "default"
        assert result["additional_permissions"] is not None
        assert result["additional_permissions"]["network"]["enabled"] is True
        assert (
            "/usr/local/bin" in result["additional_permissions"]["file_system"]["write"]
        )

    def test_npm_install_wrapped_grants_permissions(
        self, classifier: CommandClassifier
    ) -> None:
        result = classifier.classify('bash -c "npm install -g express"')
        assert result["decision"] == "default"
        assert result["additional_permissions"] is not None
        assert result["additional_permissions"]["network"]["enabled"] is True

    def test_cargo_build_grants_network(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("cargo build")
        assert result["decision"] == "default"
        assert result["additional_permissions"] == {"network": {"enabled": True}}

    def test_curl_grants_network(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("curl https://example.com")
        assert result["decision"] == "default"
        assert result["additional_permissions"] == {"network": {"enabled": True}}


class TestEdgeCases:
    """Test edge cases such as empty input and whitespace."""

    def test_empty_command(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("")
        assert result["decision"] == "default"

    def test_whitespace_command(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("   \t  \n ")
        assert result["decision"] == "default"

    def test_unclosed_quote_resilience(self, classifier: CommandClassifier) -> None:
        result = classifier.classify('bash -c "rm -rf / > /dev/null')
        assert result["decision"] in ("destructive", "default")
