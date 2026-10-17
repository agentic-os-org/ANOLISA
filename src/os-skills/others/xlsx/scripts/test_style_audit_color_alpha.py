#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
"""Tests for style_audit.py font color role matching.

Regression tests for ARGB alpha handling: Excel and LibreOffice write
explicit font colors with an FF alpha channel (FF0000FF), while the
skill's own templates use the 00-alpha form (000000FF). The role
matchers compared the full 8-digit literal, so every FF-alpha file —
anything resaved by Excel/LibreOffice, or created by openpyxl with an
explicit ARGB color — had its blue/green-font violations silently
missed.
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import style_audit  # noqa: E402


def _font(rgb):
    return {"rgb": rgb, "theme": None}


class TestColorRoleMatching(unittest.TestCase):
    def test_blue_font_matches_ff_alpha(self):
        """Excel/LibreOffice form FF0000FF must be recognized as blue."""
        self.assertTrue(style_audit._is_blue_font(_font("FF0000FF")))

    def test_blue_font_matches_zero_alpha(self):
        """Template form 000000FF keeps matching (control)."""
        self.assertTrue(style_audit._is_blue_font(_font("000000FF")))

    def test_green_font_matches_ff_alpha(self):
        self.assertTrue(style_audit._is_green_font(_font("FF008000")))

    def test_green_font_matches_zero_alpha(self):
        self.assertTrue(style_audit._is_green_font(_font("00008000")))

    def test_black_font_matches_ff_alpha(self):
        self.assertTrue(style_audit._is_black_font(_font("FF000000")))

    def test_non_blue_color_not_matched(self):
        self.assertFalse(style_audit._is_blue_font(_font("FFFF0000")))


if __name__ == "__main__":
    unittest.main()
