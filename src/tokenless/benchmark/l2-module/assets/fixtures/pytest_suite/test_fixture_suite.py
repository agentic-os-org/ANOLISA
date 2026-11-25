"""L2 fixture: thirty pytest cases; L2_FIXTURE_FAIL=1 fails cube[11] and cube[23]."""

import os

import pytest

FAIL = os.environ.get("L2_FIXTURE_FAIL") is not None


@pytest.mark.parametrize("n", range(1, 31))
def test_cube(n):
    expected = n ** 3
    if FAIL and n in (11, 23):
        expected += 1
    assert n * n * n == expected, f"cube({n}) mismatch against the reference table"
