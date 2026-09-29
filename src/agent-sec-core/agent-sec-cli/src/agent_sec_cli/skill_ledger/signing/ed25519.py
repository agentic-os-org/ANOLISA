"""NativeEd25519Backend — default signing backend using Python ``cryptography``."""

import base64
import hashlib
import logging
import os

from agent_sec_cli.skill_ledger.errors import (
    PassphraseError,
    SignatureInvalidError,
)
from agent_sec_cli.skill_ledger.signing.base import SigningBackend
from agent_sec_cli.skill_ledger.signing.key_manager import (
    clear_passphrase_cache,
    get_passphrase,
    load_keyring_public_keys,
    read_key_enc,
    read_key_pub,
    write_key_enc,
    write_key_pub,
)
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import (
    Ed25519PrivateKey,
    Ed25519PublicKey,
)
from cryptography.hazmat.primitives.ciphers.aead import AESGCM
from cryptography.hazmat.primitives.kdf.scrypt import Scrypt
from cryptography.hazmat.primitives.serialization import (
    Encoding,
    NoEncryption,
    PrivateFormat,
    PublicFormat,
)

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Key encryption constants (design doc §1)
# ---------------------------------------------------------------------------
_SALT_LEN = 16
_IV_LEN = 12
_SCRYPT_N = 2**17  # 131072
_SCRYPT_R = 8
_SCRYPT_P = 1
_KDF_KEY_LEN = 32  # AES-256


# ---------------------------------------------------------------------------
# Low-level crypto helpers
# ---------------------------------------------------------------------------


def _derive_key(passphrase: str, salt: bytes) -> bytes:
    """Derive a 256-bit AES key from *passphrase* + *salt* using scrypt."""
    kdf = Scrypt(salt=salt, length=_KDF_KEY_LEN, n=_SCRYPT_N, r=_SCRYPT_R, p=_SCRYPT_P)
    return kdf.derive(passphrase.encode("utf-8"))


def _encrypt_private_key(raw_private: bytes, passphrase: str) -> bytes:
    """Encrypt Ed25519 raw private key → ``salt(16) + iv(12) + ciphertext_with_tag``.

    The file layout matches the design doc §1 密钥管理.
    """
    salt = os.urandom(_SALT_LEN)
    iv = os.urandom(_IV_LEN)
    dk = _derive_key(passphrase, salt)
    aesgcm = AESGCM(dk)
    ct_with_tag = aesgcm.encrypt(iv, raw_private, None)
    # Layout: salt | iv | ciphertext+authTag
    return salt + iv + ct_with_tag


def _decrypt_private_key(encrypted: bytes, passphrase: str) -> bytes:
    """Decrypt the encrypted private key file back to raw Ed25519 seed."""
    if len(encrypted) < _SALT_LEN + _IV_LEN + 1:
        raise PassphraseError("Encrypted key file is too short — corrupted?")
    salt = encrypted[:_SALT_LEN]
    iv = encrypted[_SALT_LEN : _SALT_LEN + _IV_LEN]
    ct_with_tag = encrypted[_SALT_LEN + _IV_LEN :]
    dk = _derive_key(passphrase, salt)
    aesgcm = AESGCM(dk)
    try:
        return aesgcm.decrypt(iv, ct_with_tag, None)
    except Exception as exc:
        raise PassphraseError("Decryption failed — wrong passphrase?") from exc


def compute_fingerprint(public_key_bytes: bytes) -> str:
    """Compute ``"sha256:<hex>"`` fingerprint of raw public key bytes."""
    return f"sha256:{hashlib.sha256(public_key_bytes).hexdigest()}"


# ---------------------------------------------------------------------------
# NativeEd25519Backend
# ---------------------------------------------------------------------------


class NativeEd25519Backend(SigningBackend):
    """Ed25519 signing using Python's ``cryptography`` library.

    - Key generation: :meth:`generate_keys`
    - Private key: AES-256-GCM encrypted (scrypt KDF), passphrase cached in-process
    - Public key: raw 32-byte Ed25519 public key
    - Verification: public-key only, no passphrase required (~sub-ms)
    """

    def __init__(self) -> None:
        # Lazily loaded on first sign/verify
        self._private_key: Ed25519PrivateKey | None = None
        self._public_key: Ed25519PublicKey | None = None
        self._fingerprint: str | None = None

    @property
    def name(self) -> str:
        return "ed25519"

    # ------------------------------------------------------------------
    # Key generation (used by init)
    # ------------------------------------------------------------------

    def generate_keys(self, passphrase: str | None = None) -> dict[str, str]:
        """Generate a new Ed25519 key pair and persist.

        If *passphrase* is ``None`` or empty the raw 32-byte seed is stored
        directly (no encryption, no interactive prompt).  Otherwise the seed
        is encrypted with AES-256-GCM (scrypt KDF).

        Returns ``{"fingerprint": "sha256:...", "publicKeyPath": "...",
                    "privateKeyPath": "...", "encrypted": <bool>}``.
        """
        private_key = Ed25519PrivateKey.generate()
        raw_private = private_key.private_bytes(
            Encoding.Raw, PrivateFormat.Raw, NoEncryption()
        )
        raw_public = private_key.public_key().public_bytes(
            Encoding.Raw, PublicFormat.Raw
        )

        if passphrase:
            key_bytes = _encrypt_private_key(raw_private, passphrase)
            encrypted = True
        else:
            key_bytes = raw_private  # raw 32-byte seed
            encrypted = False
            logger.warning(
                "Private key is stored WITHOUT passphrase encryption. "
                "Key file security relies on filesystem permissions (mode 0600). "
                "Run 'agent-sec-cli skill-ledger init --force-keys --passphrase' "
                "to add passphrase protection."
            )

        enc_path = write_key_enc(key_bytes)
        pub_path = write_key_pub(raw_public)

        fp = compute_fingerprint(raw_public)

        # Cache for immediate use
        self._private_key = private_key
        self._public_key = private_key.public_key()
        self._fingerprint = fp

        return {
            "fingerprint": fp,
            "publicKeyPath": str(pub_path),
            "privateKeyPath": str(enc_path),
            "encrypted": encrypted,
        }

    # ------------------------------------------------------------------
    # Private key loading (lazy, passphrase-cached)
    # ------------------------------------------------------------------

    # Raw Ed25519 seed is exactly 32 bytes; encrypted file is always larger
    # (salt=16 + iv=12 + ciphertext=32 + tag=16 = 76 bytes minimum).
    _RAW_SEED_LEN = 32

    def _ensure_private_key(self) -> Ed25519PrivateKey:
        if self._private_key is not None:
            return self._private_key

        key_data = read_key_enc()

        if len(key_data) == self._RAW_SEED_LEN:
            # Unencrypted raw seed — no passphrase needed
            raw_private = key_data
        else:
            # Encrypted — need passphrase
            passphrase = get_passphrase()
            try:
                raw_private = _decrypt_private_key(key_data, passphrase)
            except PassphraseError:
                clear_passphrase_cache()
                raise

        self._private_key = Ed25519PrivateKey.from_private_bytes(raw_private)
        return self._private_key

    def _ensure_public_key(self) -> Ed25519PublicKey:
        if self._public_key is not None:
            return self._public_key

        raw_public = read_key_pub()
        self._public_key = Ed25519PublicKey.from_public_bytes(raw_public)
        return self._public_key

    def _ensure_fingerprint(self) -> str:
        if self._fingerprint is not None:
            return self._fingerprint

        raw_public = read_key_pub()
        self._fingerprint = compute_fingerprint(raw_public)
        return self._fingerprint

    # ------------------------------------------------------------------
    # SigningBackend interface
    # ------------------------------------------------------------------

    def sign(self, data: bytes) -> tuple[str, str]:
        """Sign *data* with the Ed25519 private key.

        Returns ``(base64_signature, "sha256:<fingerprint>")``.
        """
        pk = self._ensure_private_key()
        raw_sig = pk.sign(data)
        sig_b64 = base64.b64encode(raw_sig).decode("ascii")
        fp = self.get_public_key_fingerprint()
        return sig_b64, fp

    def verify(self, data: bytes, signature_b64: str, fingerprint: str) -> bool:
        """Verify *signature_b64* over *data*.

        Tries the primary public key first, then falls back to the keyring.
        """
        try:
            raw_sig = base64.b64decode(signature_b64)
        except Exception as exc:
            raise SignatureInvalidError(
                f"Signature is not valid base64: {exc}"
            ) from exc

        # Try the primary public key
        try:
            primary_fp = self._ensure_fingerprint()
            if primary_fp == fingerprint:
                pub = self._ensure_public_key()
                pub.verify(raw_sig, data)
                return True
        except InvalidSignature:
            raise SignatureInvalidError("Ed25519 signature does not match primary key")

        # Try keyring keys
        for key_bytes in load_keyring_public_keys():
            kf = compute_fingerprint(key_bytes)
            if kf != fingerprint:
                continue
            try:
                pub = Ed25519PublicKey.from_public_bytes(key_bytes)
                pub.verify(raw_sig, data)
                return True
            except InvalidSignature:
                raise SignatureInvalidError(
                    f"Ed25519 signature does not match keyring key {fingerprint}"
                )

        raise SignatureInvalidError(
            f"No key with fingerprint {fingerprint} found in key store or keyring"
        )

    def get_public_key_fingerprint(self) -> str:
        return self._ensure_fingerprint()
