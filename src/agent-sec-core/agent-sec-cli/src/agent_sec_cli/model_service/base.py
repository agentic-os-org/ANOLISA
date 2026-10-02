"""Model service client protocol — unified interface for local inference backends."""

from typing import Any, Protocol


class ModelServiceClient(Protocol):
    """Unified interface for local model inference services (Ollama, vLLM, etc.)."""

    def check_model(self, model: str) -> bool:
        """Check whether the specified model is loaded and ready.

        Returns True if the model is available, False otherwise.
        Should not raise on network errors — returns False instead.
        """
        ...  # pragma: no cover

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
        """Send a generation request and return the response body as a dict.

        Raises RuntimeError if the service is unreachable or returns an error.
        """
        ...  # pragma: no cover

    def chat(
        self,
        model: str,
        messages: list[dict[str, str]],
        *,
        format: str | None = None,
        options: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """Send a chat request with structured messages and return the response.

        Messages should follow the [{"role": ..., "content": ...}] format.
        Raises RuntimeError if the service is unreachable or returns an error.
        """
        ...  # pragma: no cover
