"""Flag values must not be read as subcommands during classification.

Extraction skipped only ``-``-prefixed tokens, so the value of a value-taking
flag (``git -C /repo push``) was taken as the subcommand: ``push``/``fetch``/
``pull`` lost their network grant, ``clean`` escaped the dangerous rule, and
sandbox policies were derived from the wrong classification.
"""

import pytest

from agent_sec_cli.sandbox.classify_command import CommandClassifier


@pytest.fixture()
def classifier() -> CommandClassifier:
    return CommandClassifier()


class TestGitFlagValues:
    def test_git_push_with_path_flag_keeps_network_grant(
        self, classifier: CommandClassifier
    ) -> None:
        result = classifier.classify("git -C /repo push")
        assert result["decision"] != "safe"
        assert result["additional_permissions"] == {"network": {"enabled": True}}

    def test_git_clean_with_path_flag_is_dangerous(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("git -C /repo clean -fd")
        assert result["decision"] == "dangerous"

    def test_git_pull_with_work_tree_keeps_network_grant(
        self, classifier: CommandClassifier
    ) -> None:
        result = classifier.classify("git --work-tree /repo pull")
        assert result["decision"] != "safe"
        assert result["additional_permissions"] == {"network": {"enabled": True}}

    def test_git_fetch_with_config_flag_keeps_network_grant(
        self, classifier: CommandClassifier
    ) -> None:
        result = classifier.classify("git -c http.proxy=x fetch")
        assert result["decision"] != "safe"
        assert result["additional_permissions"] == {"network": {"enabled": True}}

    def test_git_attached_config_value_is_not_subcommand(
        self, classifier: CommandClassifier
    ) -> None:
        result = classifier.classify("git -chttp.proxy=x fetch")
        assert result["decision"] != "safe"
        assert result["additional_permissions"] == {"network": {"enabled": True}}


class TestPackageManagerFlagValues:
    def test_npm_registry_value_keeps_install_grant(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("npm --registry http://mirror install pkg")
        assert result["additional_permissions"] == {"network": {"enabled": True}}

    def test_pip_index_url_value_keeps_install_grant(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("pip --index-url http://mirror install pkg")
        assert result["additional_permissions"] == {"network": {"enabled": True}}


class TestPlainSubcommandsUnchanged:
    def test_git_push_still_gets_network_grant(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("git push")
        assert result["decision"] != "safe"
        assert result["additional_permissions"] == {"network": {"enabled": True}}

    def test_git_clean_still_dangerous(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("git clean -fd")
        assert result["decision"] == "dangerous"

    def test_git_status_still_safe(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("git status")
        assert result["decision"] == "safe"

    def test_git_log_with_path_flag_still_safe(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("git -C /repo log")
        assert result["decision"] == "safe"

    def test_npm_install_still_gets_network_grant(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("npm install pkg")
        assert result["additional_permissions"] == {"network": {"enabled": True}}


class TestPerCommandValueFlags:
    """Every table entry must keep its subcommand recognizable.

    A wrong or missing entry silently drops the network grant for that
    command's flag-then-subcommand form, so each command with a value-flag
    table needs at least one exercised case.
    """

    @pytest.mark.parametrize(
        ("command", "arguments"),
        [
            ("docker", ["--context", "remote", "build", "."]),
            ("cargo", ["--config", "net.git-fetch-with-cli=true", "build"]),
            ("go", ["-C", "subdir", "install", "tool"]),
            ("gem", ["--config-file", "gemrc", "install", "rails"]),
            ("yarn", ["--cwd", "web", "install"]),
            ("pnpm", ["-C", "web", "install"]),
        ],
    )
    def test_flag_value_keeps_network_grant(
        self,
        classifier: CommandClassifier,
        command: str,
        arguments: list[str],
    ) -> None:
        result = classifier.classify(" ".join([command, *arguments]))
        grant = result["additional_permissions"] or {}
        assert grant.get("network", {}).get("enabled") is True, result
