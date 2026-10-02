"""Unit tests for pii_checker validators."""

import base64
import json

import pytest
from agent_sec_cli.pii_checker.validators import (
    luhn_check,
    validate_cn_id,
    validate_email,
    validate_jwt,
    validate_pem_private_key,
)


def _jwt_segment(value):
    encoded = json.dumps(value, separators=(",", ":")).encode("utf-8")
    return base64.urlsafe_b64encode(encoded).rstrip(b"=").decode("ascii")


def _jwt(header, payload, signature=b"signature"):
    encoded_signature = base64.urlsafe_b64encode(signature).rstrip(b"=").decode("ascii")
    return f"{_jwt_segment(header)}.{_jwt_segment(payload)}.{encoded_signature}"


def test_luhn_valid_card():
    assert luhn_check("4111 1111 1111 1111")


def test_luhn_invalid_card():
    assert not luhn_check("4111 1111 1111 1112")


def test_cn_id_valid_checksum_and_date():
    assert validate_cn_id("11010519491231002X")


def test_cn_id_accepts_lowercase_x_checksum():
    assert validate_cn_id("11010519491231002x")


def test_cn_id_invalid_date():
    assert not validate_cn_id("11010519490231002X")


def test_cn_id_invalid_checksum():
    assert not validate_cn_id("110105194912310021")


@pytest.mark.parametrize(
    "value",
    (
        "alice@company.cn",
        "first.last+tag@sub-domain.company.cn",
        "ALICE_01@SECURECORP.COM",
    ),
)
def test_email_valid_ascii_syntax(value):
    assert validate_email(value)


@pytest.mark.parametrize(
    "value",
    (
        ".alice@company.cn",
        "alice.@company.cn",
        "alice..bob@company.cn",
        "alice@-company.cn",
        "alice@company-.cn",
        "alice@bad_domain.cn",
        "alice@company..cn",
        "alice@localhost",
        "alice@company.123",
        f"{'a' * 65}@company.cn",
        f"alice@{'a' * 64}.cn",
    ),
)
def test_email_rejects_invalid_ascii_syntax(value):
    assert not validate_email(value)


def test_email_enforces_total_address_length():
    maximum_domain = ".".join(("a" * 63, "b" * 63, "c" * 60, "d" * 63))
    oversized_domain = ".".join(("a" * 63, "b" * 63, "c" * 61, "d" * 63))

    assert len(f"a@{maximum_domain}") == 254
    assert validate_email(f"a@{maximum_domain}")
    assert not validate_email(f"a@{oversized_domain}")


def test_jwt_valid_structure():
    token = (
        "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9."
        "eyJzdWIiOiIxMjM0NTY3ODkwIn0."
        "SflKxwRJSMeKKF2QT4fwpMeJf36POk6yJV_adQssw5c"
    )
    assert validate_jwt(token)


def test_jwt_type_header_is_optional():
    assert validate_jwt(_jwt({"alg": "HS256"}, {"sub": "123"}))


def test_jwt_only_accepts_noncanonical_signature_with_equivalent_bytes():
    token = _jwt({"alg": "HS256"}, {"sub": "123"}, signature=b"\0" * 32)
    header, payload, canonical_signature = token.split(".")
    noncanonical_payload = f"{payload[:-1]}R"
    noncanonical_signature = f"{canonical_signature[:-1]}B"

    assert payload.endswith("Q")
    assert base64.urlsafe_b64decode(f"{payload}==") == base64.urlsafe_b64decode(
        f"{noncanonical_payload}=="
    )
    assert canonical_signature.endswith("A")
    assert base64.urlsafe_b64decode(f"{canonical_signature}=") == (
        base64.urlsafe_b64decode(f"{noncanonical_signature}=")
    )
    assert validate_jwt(f"{header}.{payload}.{noncanonical_signature}")
    assert not validate_jwt(f"{header}.{noncanonical_payload}.{canonical_signature}")


def test_jwt_invalid_structure():
    assert not validate_jwt("not.a.jwt")


def test_jwt_requires_json_objects_and_header_algorithm():
    assert not validate_jwt(_jwt({"typ": "JWT"}, {"sub": "123"}))
    assert not validate_jwt(_jwt({"alg": "HS256"}, ["not", "an", "object"]))
    assert not validate_jwt(_jwt({"alg": "   "}, {"sub": "123"}))


def test_jwt_rejects_non_json_and_invalid_base64url_segments():
    payload = _jwt_segment({"sub": "123"})
    signature = base64.urlsafe_b64encode(b"signature").rstrip(b"=").decode("ascii")

    assert not validate_jwt(f"bm90LWpzb24.{payload}.{signature}")
    assert not validate_jwt(f"_w.{payload}.{signature}")
    assert not validate_jwt(f"{_jwt_segment({'alg': 'HS256'})}.{payload}.a")


def test_jwt_rejects_json_parser_resource_limits():
    deeply_nested_payload = "[" * 1_100 + "]" * 1_100
    oversized_integer_payload = "9" * 4_301
    header = _jwt_segment({"alg": "HS256"})
    signature = base64.urlsafe_b64encode(b"signature").rstrip(b"=").decode("ascii")

    for payload in (deeply_nested_payload, oversized_integer_payload):
        encoded_payload = (
            base64.urlsafe_b64encode(payload.encode()).rstrip(b"=").decode()
        )
        assert not validate_jwt(f"{header}.{encoded_payload}.{signature}")


def test_pem_private_key_matching_markers():
    pem = """-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA0testbody
-----END RSA PRIVATE KEY-----"""
    assert validate_pem_private_key(pem)


def test_pem_private_key_mismatched_markers():
    pem = """-----BEGIN RSA PRIVATE KEY-----
MIIEpAIBAAKCAQEA0testbody
-----END EC PRIVATE KEY-----"""
    assert not validate_pem_private_key(pem)
