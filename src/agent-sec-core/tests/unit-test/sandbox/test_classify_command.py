"""Command classifier: sed in-place editing flags.

`sed -i` rewrites its input files. GNU sed spells the same switch
`--in-place`, optionally with a suffix (`--in-place=.bak`), and the read-only
shortcut must not treat those spellings as a read-only invocation.
"""

import pytest

from agent_sec_cli.sandbox.classify_command import CommandClassifier


@pytest.fixture(scope="module")
def classifier():
    return CommandClassifier()


@pytest.mark.parametrize(
    "command",
    [
        'sed -i "s/a/b/" notes.txt',
        'sed --in-place "s/a/b/" notes.txt',
    ],
)
def test_in_place_edits_are_dangerous(classifier, command):
    result = classifier.classify(command)

    assert result["decision"] == "dangerous"
    assert result["reason"] == "sed 原地修改文件"


@pytest.mark.parametrize(
    "command",
    [
        'sed -i.bak "s/a/b/" notes.txt',
        'sed --in-place=.bak "s/a/b/" notes.txt',
        'sed -n --in-place "s/a/b/" notes.txt',
    ],
)
def test_in_place_spellings_with_extras_are_not_read_only(classifier, command):
    assert classifier.classify(command)["decision"] != "safe"


@pytest.mark.parametrize(
    "command",
    [
        "sed -n p notes.txt",
        'sed "s/a/b/" notes.txt',
    ],
)
def test_plain_sed_stays_read_only(classifier, command):
    result = classifier.classify(command)

    assert result["decision"] == "safe"
    assert result["reason"] == "sed 只读模式"
