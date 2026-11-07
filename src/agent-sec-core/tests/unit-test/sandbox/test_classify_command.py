"""Unit tests for the v1 command classifier's shell-wrapper extraction."""

from agent_sec_cli.sandbox.classify_command import (
    CommandClassifier,
    _split_shell_statements,
)


def _decision(command: str) -> str:
    return CommandClassifier().classify(command)["decision"]


# ─── _split_shell_statements: separator handling ────────────────────────────


def test_split_on_all_statement_separators() -> None:
    # Newline, ;, |, single & and the two-char && / || all terminate a
    # statement in shell, so each must yield its own segment.
    assert _split_shell_statements("cat foo\nrm -rf bar") == ["cat foo", "rm -rf bar"]
    assert _split_shell_statements("cat foo;rm -rf bar") == ["cat foo", "rm -rf bar"]
    assert _split_shell_statements("cat foo | rm -rf bar") == ["cat foo", "rm -rf bar"]
    assert _split_shell_statements("cat foo & rm -rf bar") == ["cat foo", "rm -rf bar"]
    assert _split_shell_statements("cat foo && rm -rf bar") == [
        "cat foo",
        "rm -rf bar",
    ]
    assert _split_shell_statements("cat foo || rm -rf bar") == [
        "cat foo",
        "rm -rf bar",
    ]


def test_split_keeps_separators_inside_quotes() -> None:
    # A separator inside a quoted word is data, not syntax: splitting it
    # would leave unbalanced quotes, fail shlex, and misclassify benign
    # commands like `echo 'a;b'`.
    assert _split_shell_statements("echo 'a\nb'") == ["echo 'a\nb'"]
    assert _split_shell_statements('echo "a;b"') == ['echo "a;b"']
    assert _split_shell_statements("echo 'a & b'") == ["echo 'a & b'"]
    assert _split_shell_statements("echo 'a|b' c") == ["echo 'a|b' c"]


def test_split_respects_backslash_escapes() -> None:
    # Outside quotes a backslash escapes the next character...
    assert _split_shell_statements("echo a\\;b") == ["echo a\\;b"]
    # ...inside double quotes it escapes a closing quote, so the newline
    # after the closed string is still a separator...
    assert _split_shell_statements('echo "a\\"b"\nrm -rf x') == [
        'echo "a\\"b"',
        "rm -rf x",
    ]
    # ...and inside single quotes backslash is literal, so the quote after
    # it closes the string and the newline splits.
    assert _split_shell_statements("echo 'a\\'\nrm -rf x") == [
        "echo 'a\\'",
        "rm -rf x",
    ]


def test_split_drops_empty_segments() -> None:
    assert _split_shell_statements("\n  \ncat foo\n\n") == ["cat foo"]
    assert _split_shell_statements("cat foo ; ; rm -rf bar") == [
        "cat foo",
        "rm -rf bar",
    ]


# ─── classification: separators must not hide inner commands ────────────────


def test_newline_separated_destructive_command_is_destructive() -> None:
    # Red on main: the newline statement separator was not recognized, so
    # the mkfs/kill after the first newline was parsed as arguments of
    # `cat` and the whole script sailed through as "safe" — bypassing the
    # documented destructive deny.
    assert _decision("bash -c 'cat foo\nmkfs /dev/sda1'") == "destructive"
    assert _decision("bash -c 'cat foo\nkill -9 1'") == "destructive"


def test_newline_separated_dangerous_command_is_dangerous() -> None:
    assert _decision("bash -c 'cat foo\nrm -rf bar'") == "dangerous"
    assert _decision("bash -c 'cat foo\nchmod 777 /etc/hosts'") == "dangerous"
    assert _decision("bash -c 'cat foo\nsudo rm /etc/important'") == "dangerous"


def test_ampersand_separated_dangerous_command_is_dangerous() -> None:
    # `cat foo & rm -rf bar` runs rm in the background: & terminates the
    # statement just like ; does.
    assert _decision("bash -c 'cat foo & rm -rf bar'") == "dangerous"


def test_all_safe_statements_remain_safe() -> None:
    assert _decision("bash -c 'cat foo\nls -la'") == "safe"
    assert _decision("bash -c 'echo hi && cat foo'") == "safe"


def test_quoted_newline_argument_stays_safe() -> None:
    # The script itself quotes the newline (`echo 'a\nb'`), so it is one
    # statement and remains a read-only echo. Note the outer double
    # quotes: the command-level shlex already strips the outer quoting,
    # so only quotes surviving *inside* the script protect the newline.
    assert _decision("bash -c \"echo 'a\nb'\"") == "safe"


def test_unquoted_newline_is_two_statements() -> None:
    # `bash -c 'echo a\nb'` hands bash the script `echo a\nb` — an
    # unquoted newline, i.e. two statements where `b` is a command that
    # is not in SAFE_COMMANDS. Classifying that as default (sandboxed,
    # workspace-write) matches the real shell semantics; on main it was
    # "safe" only because the newline was never treated as a separator.
    assert _decision("bash -c 'echo a\nb'") == "default"


def test_semicolon_still_splits() -> None:
    # Guard for the pre-existing operator behavior, now routed through
    # the quote-aware splitter.
    assert _decision("bash -c 'cat foo; rm -rf bar'") == "dangerous"


def test_unquoted_ampersand_in_word_is_a_separator() -> None:
    # `echo a&b` genuinely runs `b` as a command in a shell, so the script
    # is not "all safe" anymore; it falls to the default sandbox policy.
    assert _decision("bash -c 'echo a&b'") == "default"
