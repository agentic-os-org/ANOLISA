"""GNU sed's long in-place flag must not read as a read-only invocation.

The sed special case only matched ``-i`` (``a == "-i" or a.startswith("-i")``),
and the dangerous rule's flag list only contained ``"-i"`` — so
``sed --in-place "s/a/b/" file`` was classified safe (read-only sandbox)
while the identical ``sed -i`` was dangerous.
"""

import pytest

from agent_sec_cli.sandbox.classify_command import CommandClassifier


@pytest.fixture()
def classifier() -> CommandClassifier:
    return CommandClassifier()


class TestSedInPlaceLongFlag:
    def test_in_place_long_flag_is_dangerous(self, classifier: CommandClassifier) -> None:
        result = classifier.classify('sed --in-place "s/a/b/" file.txt')
        assert result["decision"] == "dangerous"

    def test_in_place_long_flag_with_suffix_is_not_safe(
        self, classifier: CommandClassifier
    ) -> None:
        result = classifier.classify('sed --in-place=.bak "s/a/b/" file.txt')
        assert result["decision"] != "safe"

    def test_short_in_place_with_suffix_is_not_safe(self, classifier: CommandClassifier) -> None:
        result = classifier.classify('sed -i.bak "s/a/b/" file.txt')
        assert result["decision"] != "safe"

    def test_short_in_place_still_dangerous(self, classifier: CommandClassifier) -> None:
        result = classifier.classify('sed -i "s/a/b/" file.txt')
        assert result["decision"] == "dangerous"

    def test_plain_substitution_still_safe(self, classifier: CommandClassifier) -> None:
        result = classifier.classify('sed "s/a/b/" file.txt')
        assert result["decision"] == "safe"

    def test_stream_edits_still_safe(self, classifier: CommandClassifier) -> None:
        result = classifier.classify("sed -n 5p file.txt")
        assert result["decision"] == "safe"
