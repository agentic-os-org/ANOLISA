"""Authenticate Skill Ledger and SkillFS over private Unix socket channels."""

import base64
import binascii
import hashlib
import hmac
import json
import os
import re
import secrets
import stat
from pathlib import Path
from typing import Any, Literal

AGENT_SEC_SKILLFS_CONTROL_SOCKET = "AGENT_SEC_SKILLFS_CONTROL_SOCKET"
AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE = "AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE"
AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE = "AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE"

AUTH_VERSION = "1"
AUTH_FRAME_LIMIT = 4096
AUTH_HANDSHAKE_TIMEOUT_SECONDS = 5.0
NONCE_LENGTH = 32
MIN_SECRET_LENGTH = 32
MAX_SECRET_LENGTH = 4096

CONTROL_CLIENT_DOMAIN = "anolisa.skillfs.control.client.v1"
CONTROL_SERVER_DOMAIN = "anolisa.skillfs.control.server.v1"
NOTIFY_CLIENT_DOMAIN = "anolisa.skillfs.notify.client.v1"
NOTIFY_SERVER_DOMAIN = "anolisa.skillfs.notify.server.v1"

FrameSender = Literal["client", "server"]
ProofFrameKind = Literal["auth.proof", "auth.ok"]

_AUTH_FIELD_PATTERN = re.compile(
    rb'"(?:authVersion|nonce|proof)"|"type"\s*:\s*"auth\.|"auth\.'
)


class _JsonObjectPairs(list[tuple[str, Any]]):
    """Preserve decoded field occurrences while classifying a first frame."""


class SkillFsPeerAuthError(Exception):
    """SkillFS peer authentication configuration or protocol is invalid."""


class SharedSecret:
    """Immutable shared secret whose representation never exposes key bytes."""

    __slots__ = ("_value",)

    def __init__(self, value: bytes) -> None:
        if not MIN_SECRET_LENGTH <= len(value) <= MAX_SECRET_LENGTH:
            raise SkillFsPeerAuthError(
                "authentication key must contain between "
                f"{MIN_SECRET_LENGTH} and {MAX_SECRET_LENGTH} bytes"
            )
        self._value = bytes(value)

    def __repr__(self) -> str:
        return "SharedSecret([REDACTED])"

    @classmethod
    def load(cls, path: str | Path) -> "SharedSecret":
        """Load a bounded regular key file without following a final symlink."""
        key_path = Path(path)
        if not key_path.is_absolute():
            raise SkillFsPeerAuthError("authentication key file path must be absolute")

        flags = os.O_RDONLY
        flags |= getattr(os, "O_NOFOLLOW", 0)
        flags |= getattr(os, "O_CLOEXEC", 0)
        flags |= getattr(os, "O_NONBLOCK", 0)
        try:
            file_descriptor = os.open(key_path, flags)
        except OSError as exc:
            raise SkillFsPeerAuthError(
                "failed to open authentication key file"
            ) from exc

        try:
            metadata = os.fstat(file_descriptor)
            if not stat.S_ISREG(metadata.st_mode):
                raise SkillFsPeerAuthError("authentication key must be a regular file")
            if metadata.st_uid != os.geteuid():
                raise SkillFsPeerAuthError(
                    "authentication key file must be owned by the effective user"
                )
            if stat.S_IMODE(metadata.st_mode) & 0o077:
                raise SkillFsPeerAuthError(
                    "authentication key file must not grant group or other permissions"
                )
            value = _read_bounded_secret(file_descriptor)
        except OSError as exc:
            raise SkillFsPeerAuthError(
                "failed to read authentication key file"
            ) from exc
        finally:
            os.close(file_descriptor)

        return cls(value)

    def proof(self, domain: str, nonce: bytes) -> bytes:
        """Return a domain-separated handshake proof."""
        _validate_nonce(nonce)
        return hmac.new(
            self._value,
            domain.encode("utf-8") + b"\0" + nonce,
            hashlib.sha256,
        ).digest()

    def frame_proof(self, domain: str, nonce: bytes, payload: bytes) -> bytes:
        """Return a direction- and session-bound business-frame proof."""
        _validate_nonce(nonce)
        message = (
            domain.encode("utf-8")
            + b"\0frame\0"
            + nonce
            + len(payload).to_bytes(8, byteorder="big")
            + payload
        )
        return hmac.new(self._value, message, hashlib.sha256).digest()


class AuthenticatedSession:
    """Session-bound proof helper that does not own the transport socket."""

    __slots__ = ("_client_domain", "_nonce", "_secret", "_server_domain")

    def __init__(
        self,
        secret: SharedSecret,
        nonce: bytes,
        client_domain: str,
        server_domain: str,
    ) -> None:
        _validate_nonce(nonce)
        self._secret = secret
        self._nonce = bytes(nonce)
        self._client_domain = client_domain
        self._server_domain = server_domain

    def protect_payload(self, payload: bytes, sender: FrameSender) -> bytes:
        """Append the NDJSON delimiter and a session-bound authentication tag."""
        if b"\n" in payload:
            raise SkillFsPeerAuthError(
                "authenticated business payload must be one NDJSON frame"
            )
        proof = self._secret.frame_proof(self._domain(sender), self._nonce, payload)
        return payload + b"\n" + _encode_auth_frame("auth.frame", proof=proof)

    def verify_payload(
        self,
        payload: bytes,
        auth_frame: bytes,
        sender: FrameSender,
    ) -> None:
        """Verify a raw business payload before callers parse or dispatch it."""
        frame = _parse_auth_frame(auth_frame)
        _validate_auth_frame(
            frame,
            expected_kind="auth.frame",
            require_nonce=False,
            require_proof=True,
        )
        actual = _decode_32(frame["proof"])
        expected = self._secret.frame_proof(self._domain(sender), self._nonce, payload)
        if not hmac.compare_digest(actual, expected):
            raise SkillFsPeerAuthError("authentication proof verification failed")

    def _domain(self, sender: FrameSender) -> str:
        if sender == "client":
            return self._client_domain
        if sender == "server":
            return self._server_domain
        raise SkillFsPeerAuthError("invalid authenticated frame sender")


def configured_control_socket_path() -> Path | None:
    """Return the configured SkillFS control socket, if present."""
    return _optional_environment_path(AGENT_SEC_SKILLFS_CONTROL_SOCKET)


def configured_control_key_path() -> Path | None:
    """Return the configured control authentication key, if present."""
    return _optional_environment_path(AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE)


def configured_notify_key_path() -> Path | None:
    """Return the configured notify authentication key, if present."""
    return _optional_environment_path(AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE)


def build_auth_init_frame() -> bytes:
    """Build the first frame of the four-step handshake."""
    return _encode_auth_frame("auth.init")


def validate_auth_init_frame(frame: bytes) -> None:
    """Require an exact, versioned authentication init frame."""
    parsed = _parse_auth_frame(frame)
    _validate_auth_frame(
        parsed,
        expected_kind="auth.init",
        require_nonce=False,
        require_proof=False,
    )


def classify_initial_frame(frame: bytes) -> bool:
    """Return whether a daemon first frame is auth.init, rejecting auth lookalikes."""
    try:
        content = _frame_content(frame, None)
        pairs = _parse_json_object_pairs(content)
    except SkillFsPeerAuthError:
        if _contains_auth_field(frame):
            raise
        return False

    auth_candidate = any(
        key in {"authVersion", "nonce", "proof"}
        or (key == "type" and isinstance(value, str) and value.startswith("auth."))
        for key, value in pairs
    )
    if not auth_candidate:
        return False
    if len(content) > AUTH_FRAME_LIMIT:
        raise SkillFsPeerAuthError(
            f"authenticated frame exceeds {AUTH_FRAME_LIMIT} byte limit"
        )
    payload = _unique_json_object(pairs)
    _validate_auth_frame(
        payload,
        expected_kind="auth.init",
        require_nonce=False,
        require_proof=False,
    )
    return True


def build_auth_challenge_frame(nonce: bytes | None = None) -> tuple[bytes, bytes]:
    """Build a challenge around a fresh nonce and return both values."""
    challenge_nonce = secrets.token_bytes(NONCE_LENGTH) if nonce is None else nonce
    _validate_nonce(challenge_nonce)
    return bytes(challenge_nonce), _encode_auth_frame(
        "auth.challenge", nonce=challenge_nonce
    )


def parse_auth_challenge_frame(frame: bytes) -> bytes:
    """Parse a canonical 32-byte nonce from an auth.challenge frame."""
    parsed = _parse_auth_frame(frame)
    _validate_auth_frame(
        parsed,
        expected_kind="auth.challenge",
        require_nonce=True,
        require_proof=False,
    )
    return _decode_32(parsed["nonce"])


def build_auth_proof_frame(
    kind: ProofFrameKind,
    secret: SharedSecret,
    domain: str,
    nonce: bytes,
) -> bytes:
    """Build an auth.proof or auth.ok frame for the supplied direction."""
    return _encode_auth_frame(kind, proof=secret.proof(domain, nonce))


def verify_auth_proof_frame(
    frame: bytes,
    *,
    expected_kind: ProofFrameKind,
    secret: SharedSecret,
    domain: str,
    nonce: bytes,
) -> None:
    """Verify an auth.proof or auth.ok frame in constant time."""
    parsed = _parse_auth_frame(frame)
    _validate_auth_frame(
        parsed,
        expected_kind=expected_kind,
        require_nonce=False,
        require_proof=True,
    )
    actual = _decode_32(parsed["proof"])
    expected = secret.proof(domain, nonce)
    if not hmac.compare_digest(actual, expected):
        raise SkillFsPeerAuthError("authentication proof verification failed")


def _optional_environment_path(name: str) -> Path | None:
    value = os.environ.get(name)
    if value is None:
        return None
    if not value.strip():
        raise SkillFsPeerAuthError(f"{name} must not be empty")
    return Path(value)


def _read_bounded_secret(file_descriptor: int) -> bytes:
    remaining = MAX_SECRET_LENGTH + 1
    chunks: list[bytes] = []
    while remaining:
        chunk = os.read(file_descriptor, remaining)
        if not chunk:
            break
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def _encode_auth_frame(
    kind: str,
    *,
    nonce: bytes | None = None,
    proof: bytes | None = None,
) -> bytes:
    payload: dict[str, str] = {"authVersion": AUTH_VERSION, "type": kind}
    if nonce is not None:
        payload["nonce"] = _encode(nonce)
    if proof is not None:
        payload["proof"] = _encode(proof)
    raw = json.dumps(payload, separators=(",", ":")).encode("utf-8")
    if len(raw) > AUTH_FRAME_LIMIT:
        raise SkillFsPeerAuthError(
            f"authenticated frame exceeds {AUTH_FRAME_LIMIT} byte limit"
        )
    return raw + b"\n"


def _parse_auth_frame(frame: bytes) -> dict[str, Any]:
    return _parse_json_object(_frame_content(frame, AUTH_FRAME_LIMIT))


def _frame_content(frame: bytes, limit: int | None) -> bytes:
    if not frame.endswith(b"\n") or b"\n" in frame[:-1]:
        raise SkillFsPeerAuthError("invalid authentication frame")
    content = frame[:-1]
    if limit is not None and len(content) > limit:
        raise SkillFsPeerAuthError(f"authenticated frame exceeds {limit} byte limit")
    return content


def _contains_auth_field(frame: bytes) -> bool:
    return _AUTH_FIELD_PATTERN.search(frame) is not None


def _parse_json_object(raw: bytes) -> dict[str, Any]:
    def reject_duplicate_fields(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise SkillFsPeerAuthError("invalid authentication frame")
            result[key] = value
        return result

    try:
        payload = json.loads(
            raw.decode("utf-8"), object_pairs_hook=reject_duplicate_fields
        )
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as exc:
        raise SkillFsPeerAuthError("invalid authentication frame") from exc
    if not isinstance(payload, dict):
        raise SkillFsPeerAuthError("invalid authentication frame")
    return payload


def _parse_json_object_pairs(raw: bytes) -> _JsonObjectPairs:
    try:
        payload = json.loads(raw.decode("utf-8"), object_pairs_hook=_JsonObjectPairs)
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as exc:
        raise SkillFsPeerAuthError("invalid authentication frame") from exc
    if not isinstance(payload, _JsonObjectPairs):
        raise SkillFsPeerAuthError("invalid authentication frame")
    return payload


def _unique_json_object(pairs: _JsonObjectPairs) -> dict[str, Any]:
    payload: dict[str, Any] = {}
    for key, value in pairs:
        if key in payload:
            raise SkillFsPeerAuthError("invalid authentication frame")
        payload[key] = value
    return payload


def _validate_auth_frame(
    frame: dict[str, Any],
    *,
    expected_kind: str,
    require_nonce: bool,
    require_proof: bool,
) -> None:
    expected_fields = {"authVersion", "type"}
    if require_nonce:
        expected_fields.add("nonce")
    if require_proof:
        expected_fields.add("proof")
    if set(frame) != expected_fields:
        raise SkillFsPeerAuthError("invalid authentication frame")
    if frame.get("authVersion") != AUTH_VERSION or frame.get("type") != expected_kind:
        raise SkillFsPeerAuthError("invalid authentication frame")
    if require_nonce and not isinstance(frame.get("nonce"), str):
        raise SkillFsPeerAuthError("invalid authentication frame")
    if require_proof and not isinstance(frame.get("proof"), str):
        raise SkillFsPeerAuthError("invalid authentication frame")


def _encode(value: bytes) -> str:
    return base64.b64encode(value).decode("ascii")


def _decode_32(value: str) -> bytes:
    try:
        decoded = base64.b64decode(value, validate=True)
    except (binascii.Error, ValueError) as exc:
        raise SkillFsPeerAuthError("invalid authentication frame") from exc
    if len(decoded) != NONCE_LENGTH or _encode(decoded) != value:
        raise SkillFsPeerAuthError("invalid authentication frame")
    return decoded


def _validate_nonce(nonce: bytes) -> None:
    if len(nonce) != NONCE_LENGTH:
        raise SkillFsPeerAuthError("invalid authentication nonce")
