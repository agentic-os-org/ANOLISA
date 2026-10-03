"""Unit tests for the SkillFS shared-key peer authentication contract."""

import base64
import json
import os
from pathlib import Path

import pytest
from agent_sec_cli.skill_ledger import skillfs_peer_auth as peer_auth
from agent_sec_cli.skill_ledger.skillfs_peer_auth import (
    AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE,
    AGENT_SEC_SKILLFS_CONTROL_SOCKET,
    AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE,
    AUTH_FRAME_LIMIT,
    CONTROL_CLIENT_DOMAIN,
    CONTROL_SERVER_DOMAIN,
    MAX_SECRET_LENGTH,
    MIN_SECRET_LENGTH,
    NOTIFY_CLIENT_DOMAIN,
    NOTIFY_SERVER_DOMAIN,
    AuthenticatedSession,
    SharedSecret,
    SkillFsPeerAuthError,
    build_auth_challenge_frame,
    build_auth_init_frame,
    build_auth_proof_frame,
    classify_initial_frame,
    configured_control_key_path,
    configured_control_socket_path,
    configured_notify_key_path,
    parse_auth_challenge_frame,
    validate_auth_init_frame,
    verify_auth_proof_frame,
)

FIXED_SECRET = bytes(range(32))
FIXED_NONCE = bytes(range(32, 64))
FIXED_PAYLOAD = b'{"schemaVersion":"1","method":"ping"}'

HANDSHAKE_VECTORS = (
    (CONTROL_CLIENT_DOMAIN, "pqaSiunq07XWqMvQ8xSiSLi6dsLEy5iaCEF3md04AVI="),
    (CONTROL_SERVER_DOMAIN, "naSgjgOT+Zs71EytW6byhJMCkfek2sGmK+CDqHmDsas="),
    (NOTIFY_CLIENT_DOMAIN, "aFcVadTie7FrVTYOjk1OOjBpoQZ6LUvnLGC6stiqt6M="),
    (NOTIFY_SERVER_DOMAIN, "F22J+ua0Pmha2dPyTMmTQNtKjcmed59Mo8FKgdcgBOc="),
)

BUSINESS_FRAME_VECTORS = (
    (CONTROL_CLIENT_DOMAIN, "zT6arzIjdC4fJqiSM59qNhU2BADHJgFRq8YqifcdHCM="),
    (CONTROL_SERVER_DOMAIN, "W9lF84f43ROoMXPHkgovtowDiZ5zv0zubs2vmq98T9k="),
    (NOTIFY_CLIENT_DOMAIN, "Zzf/dpWsuj89DFbpJtwqBk5dHsV+GXwGOytZMh5xDKw="),
    (NOTIFY_SERVER_DOMAIN, "ut2Pv/8XHmiImbWSfm53ixIcoDimZOPHzrS6g3TtO+M="),
)


def _auth_frame(kind: str, **fields: object) -> bytes:
    payload = {"authVersion": "1", "type": kind, **fields}
    return json.dumps(payload, separators=(",", ":")).encode("utf-8") + b"\n"


def _write_key(path: Path, value: bytes = b"k" * 32, mode: int = 0o600) -> Path:
    path.write_bytes(value)
    path.chmod(mode)
    return path


@pytest.mark.parametrize(("domain", "expected"), HANDSHAKE_VECTORS)
def test_handshake_hmac_fixed_vectors(domain: str, expected: str) -> None:
    secret = SharedSecret(FIXED_SECRET)

    actual = base64.b64encode(secret.proof(domain, FIXED_NONCE)).decode("ascii")

    assert actual == expected


@pytest.mark.parametrize(("domain", "expected"), BUSINESS_FRAME_VECTORS)
def test_business_frame_hmac_fixed_vectors(domain: str, expected: str) -> None:
    secret = SharedSecret(FIXED_SECRET)

    actual = base64.b64encode(
        secret.frame_proof(domain, FIXED_NONCE, FIXED_PAYLOAD)
    ).decode("ascii")

    assert len(FIXED_PAYLOAD) == 37
    assert actual == expected


def test_auth_frame_builders_and_parsers_use_exact_wire_shapes() -> None:
    secret = SharedSecret(FIXED_SECRET)

    assert build_auth_init_frame() == b'{"authVersion":"1","type":"auth.init"}\n'
    validate_auth_init_frame(build_auth_init_frame())

    nonce, challenge = build_auth_challenge_frame(FIXED_NONCE)
    assert nonce == FIXED_NONCE
    assert challenge == _auth_frame(
        "auth.challenge",
        nonce="ICEiIyQlJicoKSorLC0uLzAxMjM0NTY3ODk6Ozw9Pj8=",
    )
    assert parse_auth_challenge_frame(challenge) == FIXED_NONCE

    for kind, domain in (
        ("auth.proof", CONTROL_CLIENT_DOMAIN),
        ("auth.ok", CONTROL_SERVER_DOMAIN),
    ):
        proof_frame = build_auth_proof_frame(kind, secret, domain, FIXED_NONCE)
        verify_auth_proof_frame(
            proof_frame,
            expected_kind=kind,
            secret=secret,
            domain=domain,
            nonce=FIXED_NONCE,
        )


@pytest.mark.parametrize(
    "frame",
    (
        b'{"type":"auth.init"}\n',
        b'{"authVersion":1,"type":"auth.init"}\n',
        b'{"authVersion":"2","type":"auth.init"}\n',
        b'{"authVersion":"1","type":"auth.challenge"}\n',
        b'{"authVersion":"1","type":"auth.init","nonce":null}\n',
        b'{"authVersion":"1","type":"auth.init","proof":"value"}\n',
        b'{"authVersion":"1","type":"auth.init","unknown":true}\n',
        b'{"authVersion":"1","authVersion":"1","type":"auth.init"}\n',
        b'[{"authVersion":"1","type":"auth.init"}]\n',
    ),
)
def test_auth_init_rejects_non_exact_or_duplicate_fields(frame: bytes) -> None:
    with pytest.raises(SkillFsPeerAuthError, match="invalid authentication frame"):
        validate_auth_init_frame(frame)


def test_auth_proof_rejects_wrong_secret() -> None:
    expected_secret = SharedSecret(b"e" * 32)
    wrong_secret = SharedSecret(b"w" * 32)
    frame = build_auth_proof_frame(
        "auth.proof",
        wrong_secret,
        CONTROL_CLIENT_DOMAIN,
        FIXED_NONCE,
    )

    with pytest.raises(SkillFsPeerAuthError, match="proof verification failed"):
        verify_auth_proof_frame(
            frame,
            expected_kind="auth.proof",
            secret=expected_secret,
            domain=CONTROL_CLIENT_DOMAIN,
            nonce=FIXED_NONCE,
        )


def test_authenticated_payload_rejects_tampering_and_wrong_direction() -> None:
    session = AuthenticatedSession(
        SharedSecret(FIXED_SECRET),
        FIXED_NONCE,
        CONTROL_CLIENT_DOMAIN,
        CONTROL_SERVER_DOMAIN,
    )
    protected = session.protect_payload(FIXED_PAYLOAD, "client")
    payload, auth_frame = protected.split(b"\n", maxsplit=1)

    assert payload == FIXED_PAYLOAD
    session.verify_payload(payload, auth_frame, "client")
    with pytest.raises(SkillFsPeerAuthError, match="proof verification failed"):
        session.verify_payload(
            payload.replace(b"ping", b"status"), auth_frame, "client"
        )
    with pytest.raises(SkillFsPeerAuthError, match="proof verification failed"):
        session.verify_payload(payload, auth_frame, "server")


def test_authenticated_payload_rejects_newlines_and_invalid_sender() -> None:
    session = AuthenticatedSession(
        SharedSecret(FIXED_SECRET),
        FIXED_NONCE,
        CONTROL_CLIENT_DOMAIN,
        CONTROL_SERVER_DOMAIN,
    )

    with pytest.raises(SkillFsPeerAuthError, match="one NDJSON frame"):
        session.protect_payload(b"first\nsecond", "client")
    with pytest.raises(
        SkillFsPeerAuthError, match="invalid authenticated frame sender"
    ):
        session.protect_payload(FIXED_PAYLOAD, "invalid")


def test_nonce_and_proof_require_canonical_padded_base64() -> None:
    secret = SharedSecret(FIXED_SECRET)
    canonical_nonce = base64.b64encode(FIXED_NONCE).decode("ascii")
    canonical_proof = base64.b64encode(
        secret.proof(CONTROL_CLIENT_DOMAIN, FIXED_NONCE)
    ).decode("ascii")

    with pytest.raises(SkillFsPeerAuthError, match="invalid authentication frame"):
        parse_auth_challenge_frame(
            _auth_frame("auth.challenge", nonce=canonical_nonce.rstrip("="))
        )
    with pytest.raises(SkillFsPeerAuthError, match="invalid authentication frame"):
        verify_auth_proof_frame(
            _auth_frame("auth.proof", proof=canonical_proof.rstrip("=")),
            expected_kind="auth.proof",
            secret=secret,
            domain=CONTROL_CLIENT_DOMAIN,
            nonce=FIXED_NONCE,
        )


def test_authenticated_tag_rejects_unknown_and_duplicate_fields() -> None:
    session = AuthenticatedSession(
        SharedSecret(FIXED_SECRET),
        FIXED_NONCE,
        CONTROL_CLIENT_DOMAIN,
        CONTROL_SERVER_DOMAIN,
    )
    proof = base64.b64encode(
        SharedSecret(FIXED_SECRET).frame_proof(
            CONTROL_CLIENT_DOMAIN, FIXED_NONCE, FIXED_PAYLOAD
        )
    ).decode("ascii")
    unknown = _auth_frame("auth.frame", proof=proof, unknown=True)
    duplicate = (
        b'{"authVersion":"1","type":"auth.frame","proof":"'
        + proof.encode("ascii")
        + b'","proof":"'
        + proof.encode("ascii")
        + b'"}\n'
    )

    for frame in (unknown, duplicate):
        with pytest.raises(SkillFsPeerAuthError, match="invalid authentication frame"):
            session.verify_payload(FIXED_PAYLOAD, frame, "client")


def test_auth_frame_limit_is_inclusive_and_rejects_oversize() -> None:
    init_content = build_auth_init_frame()[:-1]
    exact_limit = init_content + b" " * (AUTH_FRAME_LIMIT - len(init_content)) + b"\n"
    oversized = exact_limit[:-1] + b" \n"

    validate_auth_init_frame(exact_limit)
    with pytest.raises(SkillFsPeerAuthError, match="exceeds 4096 byte limit"):
        validate_auth_init_frame(oversized)


@pytest.mark.parametrize(
    "frame",
    (
        b'{"authVersion":"1","type":"auth.init"}',
        b'{"authVersion":"1","type":"auth.init"}\nignored',
        b'{"authVersion":"1",\n"type":"auth.init"}\n',
    ),
)
def test_auth_frame_rejects_eof_or_embedded_newline(frame: bytes) -> None:
    with pytest.raises(SkillFsPeerAuthError, match="invalid authentication frame"):
        validate_auth_init_frame(frame)


def test_initial_frame_classification_rejects_auth_lookalikes() -> None:
    assert classify_initial_frame(build_auth_init_frame())
    assert not classify_initial_frame(b'{"method":"daemon.health"}\n')

    for frame in (
        b'{"authVersion":"1","type":"auth.init","extra":true}\n',
        b'{"authVersion":"1","type":"auth.proof"}\n',
        b'{"authVersion":"1","type":\n',
        b'{"type" \t : \t "auth.init"\n',
        b'{"method":"daemon.health","ty\\u0070e":"auth.init",'
        b'"ty\\u0070e":"ordinary"}\n',
    ):
        with pytest.raises(SkillFsPeerAuthError):
            classify_initial_frame(frame)


def test_auth_parsers_reject_excessive_json_nesting() -> None:
    nested = (b"[" * 1100) + b"0" + (b"]" * 1100)
    init = b'{"authVersion":"1","type":"auth.init","extra":' + nested + b"}\n"
    challenge = b'{"authVersion":"1","type":"auth.challenge","nonce":' + nested + b"}\n"

    with pytest.raises(SkillFsPeerAuthError, match="invalid authentication frame"):
        classify_initial_frame(init)
    with pytest.raises(SkillFsPeerAuthError, match="invalid authentication frame"):
        parse_auth_challenge_frame(challenge)


def test_configured_peer_paths_follow_environment(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(AGENT_SEC_SKILLFS_CONTROL_SOCKET, "/run/skillfs/control.sock")
    monkeypatch.setenv(AGENT_SEC_SKILLFS_CONTROL_AUTH_KEY_FILE, "/run/keys/control.key")
    monkeypatch.setenv(AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE, "/run/keys/notify.key")

    assert configured_control_socket_path() == Path("/run/skillfs/control.sock")
    assert configured_control_key_path() == Path("/run/keys/control.key")
    assert configured_notify_key_path() == Path("/run/keys/notify.key")

    monkeypatch.setenv(AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE, "")
    with pytest.raises(SkillFsPeerAuthError, match="must not be empty"):
        configured_notify_key_path()


@pytest.mark.parametrize("length", (MIN_SECRET_LENGTH, MAX_SECRET_LENGTH))
def test_key_loader_accepts_raw_secret_length_boundaries(
    tmp_path: Path, length: int
) -> None:
    key = _write_key(tmp_path / f"key-{length}", b"x" * length)

    secret = SharedSecret.load(key)

    assert repr(secret) == "SharedSecret([REDACTED])"


def test_key_loader_rejects_relative_path() -> None:
    with pytest.raises(SkillFsPeerAuthError, match="path must be absolute"):
        SharedSecret.load(Path("relative.key"))


def test_key_loader_rejects_final_component_symlink(tmp_path: Path) -> None:
    target = _write_key(tmp_path / "target.key")
    link = tmp_path / "link.key"
    link.symlink_to(target)

    with pytest.raises(SkillFsPeerAuthError, match="failed to open"):
        SharedSecret.load(link)


def test_key_loader_rejects_fifo_without_blocking(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fifo = tmp_path / "key.fifo"
    os.mkfifo(fifo, 0o600)
    real_open = os.open

    def checked_open(path: Path, flags: int) -> int:
        assert flags & os.O_NONBLOCK
        assert flags & os.O_CLOEXEC
        assert flags & os.O_NOFOLLOW
        return real_open(path, flags)

    monkeypatch.setattr(peer_auth.os, "open", checked_open)

    with pytest.raises(SkillFsPeerAuthError, match="regular file"):
        SharedSecret.load(fifo)


def test_key_loader_rejects_directory(tmp_path: Path) -> None:
    directory = tmp_path / "key-directory"
    directory.mkdir(mode=0o700)

    with pytest.raises(SkillFsPeerAuthError, match="regular file"):
        SharedSecret.load(directory)


@pytest.mark.parametrize("mode", (0o640, 0o604, 0o601))
def test_key_loader_rejects_group_or_other_permissions(
    tmp_path: Path, mode: int
) -> None:
    key = _write_key(tmp_path / f"key-{mode:o}", mode=mode)

    with pytest.raises(SkillFsPeerAuthError, match="group or other permissions"):
        SharedSecret.load(key)


@pytest.mark.parametrize("length", (MIN_SECRET_LENGTH - 1, MAX_SECRET_LENGTH + 1))
def test_key_loader_rejects_short_or_oversized_secret(
    tmp_path: Path, length: int
) -> None:
    key = _write_key(tmp_path / f"key-{length}", b"x" * length)

    with pytest.raises(SkillFsPeerAuthError, match="between 32 and 4096 bytes"):
        SharedSecret.load(key)


def test_key_loader_rejects_wrong_owner(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    key = _write_key(tmp_path / "key")
    wrong_uid = os.geteuid() + 1
    monkeypatch.setattr(peer_auth.os, "geteuid", lambda: wrong_uid)

    with pytest.raises(SkillFsPeerAuthError, match="effective user"):
        SharedSecret.load(key)
