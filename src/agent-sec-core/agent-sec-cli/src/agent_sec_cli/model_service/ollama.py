"""Ollama HTTP backend — implements ModelServiceClient protocol."""

import json
import logging
import urllib.error
import urllib.request
from typing import Any

log = logging.getLogger(__name__)


class OllamaClient:
    """Ollama HTTP backend for model inference.

    Communicates with an Ollama instance via its REST API.
    Implements the ModelServiceClient protocol.
    """

    def __init__(
        self, base_url: str = "http://localhost:11434", timeout: int = 30
    ) -> None:
        self._base_url = base_url.rstrip("/")
        self._timeout = timeout

    def check_model(self, model: str) -> bool:
        """GET /api/tags — check if *model* is loaded in Ollama.

        Returns True if found, False on any error or if model is absent.
        """
        try:
            req = urllib.request.Request(f"{self._base_url}/api/tags", method="GET")
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                data = json.loads(resp.read())
                names = [m.get("name", "") for m in data.get("models", [])]
                # Match exact name or name:tag prefix (e.g. "warden"
                # matches "warden:latest" but not "warden-tmp").
                found = any(n == model or n.startswith(model + ":") for n in names)
                if found:
                    log.info("Model '%s' verified in Ollama.", model)
                else:
                    log.warning(
                        "Ollama reachable but model '%s' not in: %s", model, names
                    )
                return found
        except Exception as exc:
            log.warning("Ollama check_model failed (url=%s): %s", self._base_url, exc)
            return False

    def generate(
        self,
        model: str,
        prompt: str,
        *,
        raw: bool = True,
        stream: bool = False,
        logprobs: bool = False,
        top_logprobs: int = 0,
        options: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """POST /api/generate — send a generation request to Ollama.

        Returns the parsed JSON response body.
        Raises RuntimeError if Ollama is unreachable or returns an HTTP error.
        """
        payload_dict: dict[str, Any] = {
            "model": model,
            "prompt": prompt,
            "stream": stream,
            "raw": raw,
        }
        if logprobs:
            payload_dict["logprobs"] = True
            payload_dict["top_logprobs"] = top_logprobs
        if options:
            payload_dict["options"] = options

        payload = json.dumps(payload_dict).encode("utf-8")

        req = urllib.request.Request(
            f"{self._base_url}/api/generate",
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST",
        )

        try:
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                return json.loads(resp.read())
        except urllib.error.URLError as exc:
            raise RuntimeError(
                f"Ollama request failed (url={self._base_url}): {exc}"
            ) from exc
        except Exception as exc:
            raise RuntimeError(f"Ollama request error: {exc}") from exc

    def chat(
        self,
        model: str,
        messages: list[dict[str, str]],
        *,
        format: str | None = None,
        options: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """POST /api/chat — send a chat request with structured messages.

        Returns the parsed JSON response body.
        Raises RuntimeError if Ollama is unreachable or returns an HTTP error.
        """
        payload_dict: dict[str, Any] = {
            "model": model,
            "messages": messages,
            "stream": False,
        }
        if format is not None:
            payload_dict["format"] = format
        if options is not None:
            payload_dict["options"] = options

        payload = json.dumps(payload_dict).encode("utf-8")

        req = urllib.request.Request(
            f"{self._base_url}/api/chat",
            data=payload,
            headers={"Content-Type": "application/json"},
            method="POST",
        )

        try:
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                return json.loads(resp.read())
        except urllib.error.URLError as exc:
            raise RuntimeError(
                f"Ollama chat request failed (url={self._base_url}): {exc}"
            ) from exc
        except Exception as exc:
            raise RuntimeError(f"Ollama chat request error: {exc}") from exc
