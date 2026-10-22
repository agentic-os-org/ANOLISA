"""Compound/prefixed secret-field detection — V1/V2 parity (issue #8712).

The V2 Rust scanner (``v2/crates/asc-capability-pii-scan``) locks these cases
in ``detection_quality.rs`` (``compound_secret_field_names_are_detected`` and
``compound_field_matching_stays_anchored_to_secret_tails``). The V1 Python
detector must detect the same positive cases and reject the same negatives:
underscore-prefixed names such as ``db_password`` carry the same secret
semantics as the bare keyword names, while compound names whose secret word
is not at the tail (``MAX_TOKEN_LIFETIME``) and fused words (``mypassword``)
must stay rejected.
"""

import pytest

from agent_sec_cli.pii_checker.detectors.regex import RegexPiiDetector
from agent_sec_cli.pii_checker.scanner import PiiScanner

DETECTED_CASES = [
    # Evidence cases from issue #8712.
    'db_password = "Tr0ub4dor&3x"',
    'admin_password="Tr0ub4dor&3x"',
    'access_token = "Tr0ub4dor&3x"',
    # Oracle cases mirrored from the V2 detection_quality tests.
    "MYSQL_ROOT_PASSWORD=anotherLongSecret1",
    "AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMIzKEY12",
    'GITHUB_TOKEN = "Tr0ub4dor&3x"',
    "SESSION_TOKEN=compoundSessionValue9",
    "PRIVATE_KEY=unencryptedkeymaterial1",
    "AUTH_TOKEN=neutralAlphanumeric12",
    "redis_password=replica-auth-secret9",
]

REJECTED_CASES = [
    "MAX_TOKEN_LIFETIME=3600",
    "TOKEN_LIFETIME=3600",
    "PASSWORD_MIN_LENGTH=12",
    "API_KEY_ID=AKIAIOSFODNN7EXAMPLE",
    "FOREIGN_KEY=customers_order_id",
    'mypassword = "Tr0ub4dor&3x"',
]


def _secret_types(text: str) -> list[str]:
    return [
        candidate.pii_type
        for candidate in RegexPiiDetector().detect(text)
        if candidate.pii_type
        in ("generic_secret_field", "aliyun_access_key_secret")
    ]


@pytest.mark.parametrize("text", DETECTED_CASES)
def test_prefixed_secret_field_is_detected(text):
    assert _secret_types(text), f"missing secret-field finding: {text}"


@pytest.mark.parametrize("text", REJECTED_CASES)
def test_non_secret_compound_name_stays_rejected(text):
    assert _secret_types(text) == [], f"unexpected finding: {text}"


def test_bare_names_still_detected():
    assert _secret_types('password = "Tr0ub4dor&3x"')
    assert _secret_types('密码 = "Tr0ub4dor&3x"')


def test_scanner_verdict_denies_db_password():
    result = PiiScanner().scan('db_password = "Tr0ub4dor&3x"').to_dict()
    assert result["verdict"] == "deny"
    assert result["findings"]
