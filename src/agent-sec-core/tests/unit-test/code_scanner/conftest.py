"""conftest.py — Adapts pure-string test data for unit tests.

Imports raw test cases from ``testdata.scan_test_data`` (plain strings) and
converts the ``language_str`` field to the real ``Language`` enum so that
unit-test code can pass typed values to scanner functions.
"""

import pathlib
import sys

import pytest
from agent_sec_cli.code_scanner.models import Language

# ---------------------------------------------------------------------------
# Ensure the ``testdata`` package is importable even when pytest is invoked
# from a non-standard working directory.
# ---------------------------------------------------------------------------
_TESTDATA_DIR = pathlib.Path(__file__).resolve().parent / "testdata"
if str(_TESTDATA_DIR.parent) not in sys.path:
    sys.path.insert(0, str(_TESTDATA_DIR.parent))

from testdata.scan_test_data import (  # noqa: E402
    BASH_SCAN_TEST_CASES as _RAW_BASH,
)
from testdata.scan_test_data import PYTHON_SCAN_TEST_CASES as _RAW_PYTHON
from testdata.scan_test_data import SCAN_TEST_CASES as _RAW_ALL

# ---------------------------------------------------------------------------
# Convert (code, lang_str, rule_id, count) → (code, Language, rule_id, count)
# ---------------------------------------------------------------------------
_LANG_MAP = {lang.value: lang for lang in Language}


def _wrap(cases: list) -> list:
    """Replace the plain language string with the Language enum."""
    return [(code, _LANG_MAP[lang], rule, cnt) for code, lang, rule, cnt in cases]


BASH_SCAN_TEST_CASES = _wrap(_RAW_BASH)
PYTHON_SCAN_TEST_CASES = _wrap(_RAW_PYTHON)
SCAN_TEST_CASES = _wrap(_RAW_ALL)

# Re-export per-rule case lists with Language enum for granular imports.
# (Unit tests that import individual SHELL_*/PY_* lists can still do so
#  via ``from testdata.scan_test_data import SHELL_RECURSIVE_DELETE_CASES``
#  — those remain plain strings, which is fine for regex-engine tests
#  that only need the code & rule_id fields.)


# ---------------------------------------------------------------------------
# Fixture
# ---------------------------------------------------------------------------


@pytest.fixture(
    params=SCAN_TEST_CASES,
    ids=lambda tc: f"{tc[2]}-{'TP' if tc[3] else 'TN'}-{tc[0][:30]}",
)
def scan_test_case(request: pytest.FixtureRequest) -> tuple:
    """Yield one (code, Language, rule_id, expected_finding_count) four-tuple."""
    return request.param
