"""Built-in regex and validator based PII detector."""

import re

from agent_sec_cli.pii_checker.detectors.base import PiiCandidate
from agent_sec_cli.pii_checker.models import PiiCategory, PiiSeverity
from agent_sec_cli.pii_checker.validators import (
    luhn_check,
    validate_cn_id,
    validate_email,
    validate_jwt,
)

_CONTEXT_WINDOW_RADIUS = 64
_CONTEXT_POSITIVE_DELTA = 0.12
_CONTEXT_NEGATIVE_DELTA = -0.35
_REMOTE_COMMAND_CONTEXT_LIMIT = 4_096
_REMOTE_PATH_CONTEXT_LIMIT = 1_024
_MAX_PRIVATE_KEY_CHARS = 16_384
_PRIVATE_KEY_EVIDENCE_PLACEHOLDER = "[PRIVATE_KEY_OMITTED]"

BUILTIN_PII_TYPES = frozenset(
    {
        "aliyun_access_key_id",
        "aliyun_access_key_secret",
        "api_key",
        "bearer_token",
        "cn_id",
        "credit_card",
        "email",
        "generic_secret_field",
        "jwt",
        "phone_cn",
        "private_key",
    }
)

# Confidence model (v1 fixed heuristic; values are not calibrated probabilities):
#
# | Signal class                        | Base |
# | ----------------------------------- | ---- |
# | private_key                         | 1.00 |
# | jwt                                 | 0.94 |
# | cn_id                               | 0.93 |
# | bearer_token, aliyun_access_key_id  | 0.92 |
# | credit_card with Luhn validation    | 0.92 |
# | api_key prefix patterns             | 0.86 |
# | generic_secret_field, email         | 0.82 |
# | phone_cn                            | 0.78 |
# | reserved/remote-identity email      | 0.35 |
#
# Context adjustment uses a 64-character window around each match. Security
# keywords raise matches by +0.12; fixture/example markers lower non-email test
# data by -0.35. Scanner-level thresholding hides low-confidence findings unless
# include_low_confidence is enabled.
_BASE_CONFIDENCE: dict[str, float] = {
    "private_key": 1.0,
    "jwt": 0.94,
    "cn_id": 0.93,
    "bearer_token": 0.92,
    "aliyun_access_key_id": 0.92,
    "credit_card": 0.92,
    "api_key": 0.86,
    "generic_secret_field": 0.82,
    "email": 0.82,
    "phone_cn": 0.78,
    "email_low_confidence_context": 0.35,
}

_POSITIVE_CONTEXT = (
    "password",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "bearer",
    "accesskeysecret",
    "access_key_secret",
    "密码",
    "口令",
    "密钥",
    "令牌",
    "授权",
    "访问密钥",
)
_NEGATIVE_CONTEXT = ("example", "dummy", "test", "sample", ".invalid")
_RESERVED_EMAIL_DOMAINS = frozenset(
    {
        "example",
        "example.com",
        "example.net",
        "example.org",
        "invalid",
        "localhost",
        "test",
    }
)
_REMOTE_EMAIL_URI_RE = re.compile(
    r"(?<![\w+.-])(?:git\+ssh|ssh|sftp|scp|rsync)://$", re.IGNORECASE
)
_REMOTE_COMMAND_OPTIONS_WITH_VALUE = {
    "ssh": frozenset(
        {
            "-B",
            "-b",
            "-c",
            "-D",
            "-E",
            "-e",
            "-F",
            "-I",
            "-i",
            "-J",
            "-L",
            "-l",
            "-m",
            "-O",
            "-o",
            "-P",
            "-p",
            "-R",
            "-S",
            "-W",
            "-w",
        }
    ),
    "sftp": frozenset(
        {
            "-B",
            "-b",
            "-c",
            "-F",
            "-i",
            "-J",
            "-l",
            "-o",
            "-P",
            "-R",
            "-S",
            "-s",
            "-X",
        }
    ),
}
_REMOTE_COMMAND_FLAG_OPTIONS = {
    "ssh": frozenset(
        {
            "-4",
            "-6",
            "-A",
            "-a",
            "-C",
            "-f",
            "-G",
            "-g",
            "-K",
            "-k",
            "-M",
            "-N",
            "-n",
            "-q",
            "-s",
            "-T",
            "-t",
            "-v",
            "-X",
            "-x",
            "-Y",
            "-y",
        }
    ),
    "sftp": frozenset(
        {"-4", "-6", "-A", "-a", "-C", "-f", "-N", "-p", "-q", "-r", "-v"}
    ),
}
_SHELL_COMMAND_SEPARATOR_RE = re.compile(r"[\n;|&]")

_EMAIL_RE = re.compile(
    r"(?<![\w.+-])[A-Za-z0-9._%+-]{1,64}@[A-Za-z0-9.-]+\.[A-Za-z]{2,63}(?![\w.-])"
)
_PHONE_CN_RE = re.compile(
    r"(?<!\d)(?:\+?86[-\s]?)?1[3-9]\d[-\s]?\d{4}[-\s]?\d{4}(?!\d)"
)
_CREDIT_CARD_RE = re.compile(r"(?<!\d)(?:\d[ -]?){13,19}(?!\d)")
_CN_ID_RE = re.compile(r"(?<!\d)\d{17}[\dXx](?!\w)")
_JWT_RE = re.compile(
    r"(?<![A-Za-z0-9_-])[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}(?![A-Za-z0-9_-])"
)
_API_KEY_RE = re.compile(
    r"\b(?:sk|pk|rk|gh[pousr]|xox[baprs])[-_][A-Za-z0-9_=-]{16,}\b"
)
_BEARER_RE = re.compile(r"\bBearer\s+([A-Za-z0-9._~+/=-]{16,})", re.IGNORECASE)
_PRIVATE_KEY_RE = re.compile(
    r"-----BEGIN ([A-Z0-9 ]*PRIVATE KEY)-----[\s\S]+?-----END \1-----"
)
_ALIYUN_ACCESS_KEY_ID_RE = re.compile(r"\bLTAI[A-Za-z0-9]{12,30}\b")
_SECRET_FIELD_RE = re.compile(
    r"(?i)(?<![\w\u4e00-\u9fff])(?P<name>password|passwd|secret|token|"
    r"api[_-]?key|apikey|access[_-]?key[_-]?secret|accessKeySecret|"
    r"client[_-]?secret|authorization|密码|口令|密钥|令牌|授权|访问密钥)"
    r"\s*[:=：]\s*(?P<quoted_value>\"(?P<double_value>[^\s\"',;，；：]{8,})\"|"
    r"'(?P<single_value>[^\s\"',;，；：]{8,})'|"
    r"(?P<bare_value>[^\s\"',;，；：]{8,}))"
)


def _context_window(
    text: str, start: int, end: int, radius: int = _CONTEXT_WINDOW_RADIUS
) -> str:
    """Return lowercase context around a match."""
    return text[max(0, start - radius) : min(len(text), end + radius)].lower()


def _score_with_context(text: str, start: int, end: int, base: float) -> float:
    """Adjust confidence up/down based on surrounding context."""
    context = _context_window(text, start, end)
    score = base
    compact_context = context.replace("-", "_")
    if any(marker in compact_context for marker in _POSITIVE_CONTEXT):
        score += _CONTEXT_POSITIVE_DELTA
    if any(marker in compact_context for marker in _NEGATIVE_CONTEXT):
        score += _CONTEXT_NEGATIVE_DELTA
    return max(0.0, min(1.0, score))


def _score_email_with_context(text: str, start: int, end: int, base: float) -> float:
    """Raise email confidence for security context without fixture-word penalties."""
    context = _context_window(text, start, end)
    compact_context = context.replace("-", "_")
    score = base
    if any(marker in compact_context for marker in _POSITIVE_CONTEXT):
        score += _CONTEXT_POSITIVE_DELTA
    return max(0.0, min(1.0, score))


def _is_reserved_email_domain(value: str) -> bool:
    """Return whether an address uses a reserved example or testing domain."""
    domain = value.rsplit("@", maxsplit=1)[-1].lower()
    return any(
        domain == reserved or domain.endswith(f".{reserved}")
        for reserved in _RESERVED_EMAIL_DOMAINS
    )


def _is_remote_command_target(command_prefix: str) -> bool:
    """Return whether the candidate immediately follows SSH/SFTP options."""
    command_prefix = command_prefix.strip()
    if not command_prefix:
        return False

    tokens = command_prefix.split()
    command = tokens[0].rsplit("/", maxsplit=1)[-1].lower()
    value_options = _REMOTE_COMMAND_OPTIONS_WITH_VALUE.get(command)
    flag_options = _REMOTE_COMMAND_FLAG_OPTIONS.get(command)
    if value_options is None or flag_options is None:
        return False

    index = 1
    while index < len(tokens):
        option = tokens[index]
        if option == "--":
            return index == len(tokens) - 1
        if option in flag_options:
            index += 1
            continue
        if option in value_options:
            index += 2
            if index > len(tokens):
                return False
            continue
        if len(option) > 2 and option[:2] in value_options:
            index += 1
            continue
        if (
            len(option) > 2
            and option.startswith("-")
            and all(f"-{flag}" in flag_options for flag in option[1:])
        ):
            index += 1
            continue
        return False
    return True


def _is_remote_identity_context(
    text: str, start: int, end: int, command_start: int
) -> bool:
    """Return whether an email-shaped value is clearly a remote login identity."""
    prefix = text[max(0, start - _CONTEXT_WINDOW_RADIUS) : start]
    if _REMOTE_EMAIL_URI_RE.search(prefix) is not None:
        return True

    if end + 1 < len(text) and text[end] == ":" and not text[end + 1].isspace():
        path_start = end + 1
        path_limit = min(len(text), path_start + _REMOTE_PATH_CONTEXT_LIMIT)
        remote_path = text[path_start:path_limit].split(maxsplit=1)[0]
        is_uri = re.match(r"[A-Za-z][A-Za-z0-9+.-]*://", remote_path) is not None
        if not is_uri and (
            remote_path.startswith(("/", "~", "./", "../"))
            or "/" in remote_path
            or remote_path.endswith(".git")
        ):
            return True

    target_start = start
    opening_quote = (
        text[start - 1] if start > 0 and text[start - 1] in {'"', "'"} else None
    )
    closing_quote = text[end] if end < len(text) and text[end] in {'"', "'"} else None
    if opening_quote is not None or closing_quote is not None:
        if opening_quote is None or closing_quote != opening_quote:
            return False
        after_quote = end + 1
        if (
            after_quote < len(text)
            and not text[after_quote].isspace()
            and text[after_quote] not in ";|&"
        ):
            return False
        target_start -= 1

    prefix_start = command_start + 1
    if (
        target_start <= prefix_start
        or not text[target_start - 1].isspace()
        or target_start - prefix_start > _REMOTE_COMMAND_CONTEXT_LIMIT
    ):
        return False
    return _is_remote_command_target(text[prefix_start:target_start])


def _severity_for(pii_type: str) -> tuple[str, str]:
    """Return category and severity for a finding type."""
    if pii_type in {"email", "phone_cn", "credit_card", "cn_id"}:
        return PiiCategory.PERSONAL_DATA.value, PiiSeverity.WARN.value
    return PiiCategory.CREDENTIAL.value, PiiSeverity.DENY.value


class RegexPiiDetector:
    """Built-in detector using regexes, validators, and context scoring."""

    name = "regex"
    engine = "regex_v1"

    def detect(self, text: str) -> list[PiiCandidate]:
        """Run all regex-backed detectors and return raw candidates."""
        candidates: list[PiiCandidate] = []
        self._detect_private_keys(text, candidates)
        self._detect_bearer_tokens(text, candidates)
        self._detect_secret_fields(text, candidates)
        self._detect_api_keys(text, candidates)
        self._detect_aliyun_access_key_ids(text, candidates)
        self._detect_jwts(text, candidates)
        self._detect_credit_cards(text, candidates)
        self._detect_cn_ids(text, candidates)
        self._detect_phone_numbers(text, candidates)
        self._detect_emails(text, candidates)
        return candidates

    def _add_candidate(
        self,
        candidates: list[PiiCandidate],
        *,
        pii_type: str,
        value: str,
        span: tuple[int, int],
        confidence: float,
        metadata: dict[str, object] | None = None,
    ) -> None:
        """Append a candidate with type-derived category and severity."""
        category, severity = _severity_for(pii_type)
        candidates.append(
            PiiCandidate(
                pii_type=pii_type,
                category=category,
                severity=severity,
                confidence=confidence,
                value=value,
                span=span,
                metadata=metadata or {},
                detector=self.name,
                engine=self.engine,
            )
        )

    def _detect_private_keys(self, text: str, candidates: list[PiiCandidate]) -> None:
        for match in _PRIVATE_KEY_RE.finditer(text):
            span = match.span()
            if span[1] - span[0] > _MAX_PRIVATE_KEY_CHARS:
                self._add_candidate(
                    candidates,
                    pii_type="private_key",
                    value=_PRIVATE_KEY_EVIDENCE_PLACEHOLDER,
                    span=span,
                    confidence=_BASE_CONFIDENCE["private_key"],
                    metadata={
                        "validator": "pem_private_key",
                        "evidence_omitted": True,
                    },
                )
                continue

            value = match.group(0)
            self._add_candidate(
                candidates,
                pii_type="private_key",
                value=value,
                span=span,
                confidence=_BASE_CONFIDENCE["private_key"],
                metadata={"validator": "pem_private_key"},
            )

    def _detect_bearer_tokens(self, text: str, candidates: list[PiiCandidate]) -> None:
        for match in _BEARER_RE.finditer(text):
            value = match.group(1)
            span = match.span(1)
            self._add_candidate(
                candidates,
                pii_type="bearer_token",
                value=value,
                span=span,
                confidence=_score_with_context(
                    text, *span, _BASE_CONFIDENCE["bearer_token"]
                ),
                metadata={"context": "bearer"},
            )

    def _detect_secret_fields(self, text: str, candidates: list[PiiCandidate]) -> None:
        for match in _SECRET_FIELD_RE.finditer(text):
            field_name = match.group("name")
            value = (
                match.group("double_value")
                or match.group("single_value")
                or match.group("bare_value")
            )
            evidence_value = match.group("quoted_value")
            if value is None:
                continue
            if len(value) < 12 and not field_name.lower().startswith("accesskey"):
                continue
            normalized_name = field_name.lower().replace("-", "_")
            compact_name = normalized_name.replace("_", "")
            if compact_name == "accesskeysecret":
                pii_type = "aliyun_access_key_secret"
            elif compact_name in {"apikey", "api_key"}:
                pii_type = "api_key"
            else:
                pii_type = "generic_secret_field"
            span = match.span("quoted_value")
            self._add_candidate(
                candidates,
                pii_type=pii_type,
                value=evidence_value,
                span=span,
                confidence=_score_with_context(
                    text, *span, _BASE_CONFIDENCE["generic_secret_field"]
                ),
                metadata={"field": field_name},
            )

    def _detect_api_keys(self, text: str, candidates: list[PiiCandidate]) -> None:
        for match in _API_KEY_RE.finditer(text):
            self._add_candidate(
                candidates,
                pii_type="api_key",
                value=match.group(0),
                span=match.span(),
                confidence=_score_with_context(
                    text, *match.span(), _BASE_CONFIDENCE["api_key"]
                ),
                metadata={"pattern": "token_prefix"},
            )

    def _detect_aliyun_access_key_ids(
        self, text: str, candidates: list[PiiCandidate]
    ) -> None:
        for match in _ALIYUN_ACCESS_KEY_ID_RE.finditer(text):
            self._add_candidate(
                candidates,
                pii_type="aliyun_access_key_id",
                value=match.group(0),
                span=match.span(),
                confidence=_score_with_context(
                    text, *match.span(), _BASE_CONFIDENCE["aliyun_access_key_id"]
                ),
            )

    def _detect_jwts(self, text: str, candidates: list[PiiCandidate]) -> None:
        if text.count(".") < 2:
            return
        for match in _JWT_RE.finditer(text):
            value = match.group(0)
            if validate_jwt(value):
                self._add_candidate(
                    candidates,
                    pii_type="jwt",
                    value=value,
                    span=match.span(),
                    confidence=_score_with_context(
                        text, *match.span(), _BASE_CONFIDENCE["jwt"]
                    ),
                    metadata={"validator": "jwt_structure"},
                )

    def _detect_credit_cards(self, text: str, candidates: list[PiiCandidate]) -> None:
        for match in _CREDIT_CARD_RE.finditer(text):
            value = match.group(0)
            if luhn_check(value):
                self._add_candidate(
                    candidates,
                    pii_type="credit_card",
                    value=value,
                    span=match.span(),
                    confidence=_score_with_context(
                        text, *match.span(), _BASE_CONFIDENCE["credit_card"]
                    ),
                    metadata={"validator": "luhn"},
                )

    def _detect_cn_ids(self, text: str, candidates: list[PiiCandidate]) -> None:
        for match in _CN_ID_RE.finditer(text):
            value = match.group(0)
            if validate_cn_id(value):
                self._add_candidate(
                    candidates,
                    pii_type="cn_id",
                    value=value,
                    span=match.span(),
                    confidence=_score_with_context(
                        text, *match.span(), _BASE_CONFIDENCE["cn_id"]
                    ),
                    metadata={"validator": "cn_id_checksum"},
                )

    def _detect_phone_numbers(self, text: str, candidates: list[PiiCandidate]) -> None:
        for match in _PHONE_CN_RE.finditer(text):
            value = match.group(0)
            self._add_candidate(
                candidates,
                pii_type="phone_cn",
                value=value,
                span=match.span(),
                confidence=_score_with_context(
                    text, *match.span(), _BASE_CONFIDENCE["phone_cn"]
                ),
            )

    def _detect_emails(self, text: str, candidates: list[PiiCandidate]) -> None:
        separator_cursor = 0
        command_start = -1
        for match in _EMAIL_RE.finditer(text):
            for separator in _SHELL_COMMAND_SEPARATOR_RE.finditer(
                text, separator_cursor, match.start()
            ):
                command_start = separator.start()
            separator_cursor = match.end()

            value = match.group(0)
            if not validate_email(value):
                continue

            base = _BASE_CONFIDENCE["email"]
            metadata = {"validator": "email_syntax"}
            if _is_remote_identity_context(text, *match.span(), command_start):
                base = _BASE_CONFIDENCE["email_low_confidence_context"]
                metadata["context"] = "remote_identity"
            elif _is_reserved_email_domain(value):
                base = _BASE_CONFIDENCE["email_low_confidence_context"]
                metadata["context"] = "reserved_domain"
            self._add_candidate(
                candidates,
                pii_type="email",
                value=value,
                span=match.span(),
                confidence=_score_email_with_context(text, *match.span(), base),
                metadata=metadata,
            )
