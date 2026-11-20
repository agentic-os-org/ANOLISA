"""Subcommand extraction must skip the values of value-taking flags.

`git -C /repo push` used to read `/repo` as the subcommand, which made a
networked `git push` look safe and let `git -C /repo clean -fd` bypass the
dangerous layer.
"""

import pytest

from agent_sec_cli.sandbox.classify_command import CommandClassifier


@pytest.fixture(scope="module")
def classifier():
    return CommandClassifier()


@pytest.mark.parametrize(
    "command",
    [
        "git push",
        "git -C /repo push",
        "git --work-tree /repo pull",
        "git -c user.name=bot push",
    ],
)
def test_git_network_subcommands_keep_their_network_grant(classifier, command):
    result = classifier.classify(command)

    assert result["decision"] == "default"
    assert result["additional_permissions"] == {"network": {"enabled": True}}


def test_git_clean_behind_a_path_flag_stays_dangerous(classifier):
    result = classifier.classify("git -C /repo clean -fd")

    assert result["decision"] == "dangerous"
    assert result["reason"] == "git clean 删除未跟踪文件"


@pytest.mark.parametrize(
    "command",
    [
        "npm install pkg",
        "npm --registry https://registry.example install pkg",
    ],
)
def test_npm_install_behind_a_registry_flag_keeps_network(classifier, command):
    result = classifier.classify(command)

    assert result["decision"] == "default"
    assert result["additional_permissions"] == {"network": {"enabled": True}}


@pytest.mark.parametrize(
    "command",
    [
        "pip install pkg",
        "pip --index-url https://pypi.example install pkg",
        "pip3 --index-url https://pypi.example install pkg",
    ],
)
def test_pip_install_behind_an_index_flag_keeps_network(classifier, command):
    result = classifier.classify(command)

    assert result["decision"] == "default"
    assert result["additional_permissions"] == {"network": {"enabled": True}}


def test_git_read_only_subcommand_stays_safe(classifier):
    result = classifier.classify("git status")

    assert result["decision"] == "safe"
