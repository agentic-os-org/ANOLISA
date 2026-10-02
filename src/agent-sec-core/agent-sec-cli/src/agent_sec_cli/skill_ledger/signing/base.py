"""Abstract base class for pluggable signing backends."""

from abc import ABC, abstractmethod
from typing import Any


class SigningBackend(ABC):
    """Interface that all signing backends must implement.

    The default backend is :class:`NativeEd25519Backend`.  Additional backends
    (GPG, PKCS#11) can be implemented against this interface and selected via
    ``~/.config/agent-sec/skill-ledger/config.json``.
    """

    @property
    @abstractmethod
    def name(self) -> str:
        """Human-readable backend identifier (e.g. ``"ed25519"``, ``"gpg"``)."""
        pass

    @abstractmethod
    def generate_keys(self, passphrase: str | None = None) -> dict[str, Any]:
        """Generate a new key pair and persist to storage.

        Returns a dict with at least ``{"fingerprint": "sha256:..."}``.
        """
        pass

    @abstractmethod
    def sign(self, data: bytes) -> tuple[str, str]:
        """Sign *data* and return ``(base64_signature, key_fingerprint)``.

        The private key may require passphrase decryption on first use.
        """
        pass

    @abstractmethod
    def verify(self, data: bytes, signature_b64: str, fingerprint: str) -> bool:
        """Verify *signature_b64* over *data* using the public key matching *fingerprint*.

        Returns ``True`` on success.  Raises :class:`SignatureInvalidError` on
        failure, or returns ``False`` if the fingerprint is unknown.
        """
        pass

    @abstractmethod
    def get_public_key_fingerprint(self) -> str:
        """Return the fingerprint of the current signing public key.

        Format: ``"sha256:<hex>"``.
        """
        pass
