#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Shared, one-based Excel column conversions for the workbook editors."""


def col_letter(n: int) -> str:
    """Convert a one-based column number to letters; nonpositive values yield empty text."""
    result = ""
    while n > 0:
        n, remainder = divmod(n - 1, 26)
        result = chr(65 + remainder) + result
    return result


def col_number(s: str) -> int:
    """Convert case-insensitive column letters to a one-based number."""
    number = 0
    for character in s.upper():
        number = number * 26 + (ord(character) - 64)
    return number
