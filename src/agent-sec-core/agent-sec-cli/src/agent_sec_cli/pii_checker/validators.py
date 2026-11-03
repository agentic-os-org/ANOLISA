"""Validators used to reduce false positives in PII detection."""

import base64
import binascii
import json
import re
from datetime import datetime

_EMAIL_LOCAL_RE = re.compile(r"[A-Za-z0-9._%+-]+")
_EMAIL_DOMAIN_LABEL_RE = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?")
_EMAIL_TLD_RE = re.compile(r"[A-Za-z]{2,63}")

# Prefixes and lengths: braintree/credit-card-type at c50db7708ce3945a1cf00aca4220ee2356d2a580.
_CARD_NETWORKS = (
    ("visa", ((4, 4),), (16, 18, 19)),
    ("mastercard", ((51, 55), (2221, 2720)), (16,)),
    ("amex", ((34, 34), (37, 37)), (15,)),
    ("diners", ((300, 305), (36, 36), (38, 39)), (14, 16, 19)),
    ("discover", ((6011, 6011), (644, 649), (65, 65)), (16, 19)),
    ("jcb", ((2131, 2131), (1800, 1800), (3528, 3589)), (16, 17, 18, 19)),
    (
        "unionpay",
        (
            (620, 620),
            (62100, 62182),
            (62184, 62187),
            (62185, 62197),
            (62200, 62205),
            (622010, 622999),
            (622018, 622018),
            (62207, 62209),
            (623, 626),
            (6270, 6270),
            (6272, 6272),
            (6276, 6276),
            (627700, 627779),
            (627781, 627799),
            (6282, 6289),
            (6291, 6291),
            (6292, 6292),
            (810, 810),
            (8110, 8131),
            (8132, 8151),
            (8152, 8163),
            (8164, 8171),
        ),
        (14, 15, 16, 17, 18, 19),
    ),
)


def validate_credit_card(value: str) -> bool:
    """Require a complete supported card format, network structure and checksum."""
    # The longest supported grouping has 19 digits and four separators.
    if len(value) > 23:
        return False
    separator = next((ch for ch in value if ch in " -"), None)
    groups = value.split(separator) if separator else [value]
    if not all(group.isdecimal() for group in groups):
        return False
    digits = "".join(groups)
    for network, prefixes, lengths in _CARD_NETWORKS:
        if len(digits) not in lengths or not any(
            low <= int(digits[: len(str(low))]) <= high for low, high in prefixes
        ):
            continue
        if separator:
            if network == "amex":
                expected = (4, 6, 5)
            elif network == "diners" and len(digits) == 14:
                expected = (4, 6, 4)
            else:
                expected = tuple(
                    len(digits[i : i + 4]) for i in range(0, len(digits), 4)
                )
            if tuple(map(len, groups)) != expected:
                continue
        return luhn_check(digits)
    return False


def luhn_check(value: str) -> bool:
    """Validate a payment card number with the Luhn checksum."""
    digits = [int(ch) for ch in re.sub(r"\D", "", value)]
    if len(digits) < 13 or len(digits) > 19:
        return False

    total = 0
    parity = len(digits) % 2
    for idx, digit in enumerate(digits):
        current = digit
        if idx % 2 == parity:
            current *= 2
            if current > 9:
                current -= 9
        total += current
    return total % 10 == 0


def validate_cn_id(value: str) -> bool:
    """Validate an 18-digit Chinese Resident Identity Card number."""
    normalized = value.strip().upper()
    if not re.fullmatch(r"\d{17}[\dX]", normalized):
        return False

    birth_date = normalized[6:14]
    try:
        datetime.strptime(birth_date, "%Y%m%d")
    except ValueError:
        return False

    weights = [7, 9, 10, 5, 8, 4, 2, 1, 6, 3, 7, 9, 10, 5, 8, 4, 2]
    checks = "10X98765432"
    total = sum(int(normalized[i]) * weights[i] for i in range(17))
    return normalized[-1] == checks[total % 11]


def validate_email(value: str) -> bool:
    """Validate the supported ASCII email-address syntax."""
    if len(value) > 254 or value.count("@") != 1:
        return False

    local, domain = value.split("@")
    if not local or len(local) > 64 or not domain or len(domain) > 253:
        return False
    if local.startswith(".") or local.endswith(".") or ".." in local:
        return False
    if _EMAIL_LOCAL_RE.fullmatch(local) is None:
        return False

    labels = domain.split(".")
    if len(labels) < 2 or _EMAIL_TLD_RE.fullmatch(labels[-1]) is None:
        return False
    return all(_EMAIL_DOMAIN_LABEL_RE.fullmatch(label) is not None for label in labels)


def validate_jwt(value: str) -> bool:
    """Validate the structural shape of a JWT."""
    parts = value.split(".")
    if len(parts) != 3 or not all(parts):
        return False
    if not all(re.fullmatch(r"[A-Za-z0-9_-]+", part) for part in parts):
        return False

    decoded_parts: list[bytes] = []
    for index, part in enumerate(parts):
        if len(part) % 4 == 1:
            return False
        padded = part + "=" * (-len(part) % 4)
        try:
            decoded = base64.b64decode(
                padded.encode("ascii"), altchars=b"-_", validate=True
            )
        except (binascii.Error, ValueError):
            return False
        if not decoded:
            return False
        # Preserve canonical JSON segments while accepting signature aliases
        # that permissive JWT libraries decode to the same bytes.
        if index < 2:
            canonical = base64.urlsafe_b64encode(decoded).rstrip(b"=").decode("ascii")
            if canonical != part:
                return False
        decoded_parts.append(decoded)

    json_parts: list[object] = []
    for decoded in decoded_parts[:2]:
        try:
            json_parts.append(json.loads(decoded.decode("utf-8")))
        except (UnicodeDecodeError, ValueError, RecursionError):
            return False
    if not all(isinstance(part, dict) for part in json_parts):
        return False

    algorithm = json_parts[0].get("alg")
    if not isinstance(algorithm, str) or not algorithm.strip():
        return False
    return True


def validate_pem_private_key(value: str) -> bool:
    """Validate that a PEM private key has matching BEGIN/END markers."""
    match = re.search(
        r"-----BEGIN ([A-Z0-9 ]*PRIVATE KEY)-----[\s\S]+?-----END \1-----",
        value.strip(),
    )
    return match is not None
