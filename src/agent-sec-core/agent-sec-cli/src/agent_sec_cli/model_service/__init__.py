"""Shared model service client for all scanners.

Provides a unified interface to local model inference backends (Ollama, vLLM, etc.).
Each scanner instantiates a client via ``create_client()`` and passes its own model name.

An environment-supplied ``base_url`` pointing at a non-loopback host is refused:
the model service must run locally, and scanned content can carry credentials
and PII, so a hijacked variable must not silently exfiltrate it.  This mirrors
the Rust ``model-service`` crate.
"""

import ipaddress
import os
from typing import Any
from urllib.parse import urlsplit

from agent_sec_cli.model_service.base import ModelServiceClient

__all__ = ["ModelServiceClient", "create_client"]

# ---------------------------------------------------------------------------
# Environment variables for default configuration
# ---------------------------------------------------------------------------

_ENV_BACKEND = "AGENT_SEC_MODEL_SERVICE_BACKEND"
_ENV_BASE_URL = "AGENT_SEC_MODEL_SERVICE_BASE_URL"
_ENV_TIMEOUT = "AGENT_SEC_MODEL_SERVICE_TIMEOUT"

_DEFAULT_BACKEND = "ollama"
_DEFAULT_BASE_URL = "http://localhost:11434"
_DEFAULT_TIMEOUT = 30


def _is_loopback_url(base_url: str) -> bool:
    """Whether the URL's host is ``localhost`` or a loopback IP (``127.x.x.x``, ``::1``)."""
    host = urlsplit(base_url).hostname
    if not host:
        return False
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        # A non-literal hostname; treated as remote.
        return False


def _validate_base_url(base_url: str) -> None:
    """Reject an unsupported scheme, or any host other than loopback.

    Raises:
        ValueError: the scheme is neither ``http://`` nor ``https://``, or the
            host is not loopback.
    """
    if not base_url.startswith(("http://", "https://")):
        raise ValueError(f"base_url must use http:// or https:// scheme: {base_url!r}")
    if not _is_loopback_url(base_url):
        raise ValueError(
            f"refusing non-loopback model service base_url {base_url!r}: only a "
            "local model service is supported, and scanned content must not "
            "leave the host"
        )


def create_client(
    backend: str | None = None,
    base_url: str | None = None,
    timeout: int | None = None,
    **kwargs: Any,
) -> ModelServiceClient:
    """Create a model service client instance.

    Parameters are resolved in order: explicit argument > environment variable > default.

    An environment-derived *base_url* is validated; an explicit *base_url*
    argument is trusted, since it comes from calling code rather than the
    attacker-influenced environment.

    Raises:
        ValueError: the backend is unsupported, or the environment-derived
            ``base_url`` is rejected by :func:`_validate_base_url`.
    """
    resolved_backend = (
        backend
        if backend is not None
        else os.environ.get(_ENV_BACKEND, _DEFAULT_BACKEND)
    )
    if base_url is not None:
        resolved_base_url = base_url
    else:
        resolved_base_url = os.environ.get(_ENV_BASE_URL, _DEFAULT_BASE_URL)
        _validate_base_url(resolved_base_url)
    try:
        resolved_timeout = (
            timeout
            if timeout is not None
            else int(os.environ.get(_ENV_TIMEOUT, str(_DEFAULT_TIMEOUT)))
        )
    except ValueError:
        resolved_timeout = _DEFAULT_TIMEOUT

    if resolved_backend == "ollama":
        from agent_sec_cli.model_service.ollama import (  # noqa: PLC0415
            OllamaClient,
        )

        return OllamaClient(
            base_url=resolved_base_url, timeout=resolved_timeout, **kwargs
        )

    raise ValueError(f"Unsupported model service backend: {resolved_backend!r}")
