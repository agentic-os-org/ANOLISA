"""mitmproxy addon for the AgentSecCore secret gateway.

Each listener port belongs to one Agent domain. The addon serves the
generation named by `<runtime>/active.json`, replaces a domain's fake with the
real credential only on requests its plan allows, and reports what it loaded in
`<runtime>/status/health.json`. Without a loaded generation every request is
rejected, and any failure inside a hook rejects or kills the flow: the gateway
never degrades to passthrough.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import stat
import time

import asc_gateway_policy as policy
from mitmproxy import ctx, exceptions, http

POLL_SECONDS = 0.5
RETRY_SECONDS = 5.0
RESPONSE_BUFFER_LIMIT = 8 * 1024 * 1024
MANIFEST_NAME = "active.json"
STATUS_DIR = "status"
HEALTH_NAME = "health.json"
CA_NAME = "ca-cert.pem"
HEALTH_VERSION = 1
METADATA_KEY = "agent_sec"

# Codes a client should retry on a fresh connection instead of treating as final.
_STATUS_BY_CODE = {"sni_host_mismatch": 421}

logger = logging.getLogger("agent_sec_gateway")


def _read_runtime_file(directory: str, name: str, limit: int) -> bytes:
    flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC
    with os.fdopen(os.open(os.path.join(directory, name), flags), "rb") as handle:
        info = os.fstat(handle.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > limit:
            raise policy.LoadError(
                f"{name} is not a regular file within its size limit"
            )
        data = handle.read(limit + 1)
    if len(data) > limit:
        raise policy.LoadError(f"{name} exceeds its size limit")
    return data


def _write_atomic(directory: str, name: str, data: bytes) -> None:
    temporary = os.path.join(directory, f".tmp-{name}")
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW | os.O_CLOEXEC
    with os.fdopen(os.open(temporary, flags, 0o644), "wb") as handle:
        handle.write(data)
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, os.path.join(directory, name))


def _reject_response(code: str, status: int = 403) -> http.Response:
    body = json.dumps({"error": code, "source": "agent-sec-gateway"}).encode()
    return http.Response.make(status, body, {"Content-Type": "application/json"})


def _peek_generation(raw: bytes) -> int | None:
    try:
        value = json.loads(raw).get("generation")
    except (ValueError, AttributeError):
        return None
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def _injected(flow: http.HTTPFlow) -> policy.Decision | None:
    decision = flow.metadata.get(METADATA_KEY)
    if (
        isinstance(decision, policy.Decision)
        and decision.action == policy.ACTION_INJECT
    ):
        return decision
    return None


def _rewrite_fields(headers: http.Headers, decision: policy.Decision) -> None:
    fields = tuple(
        (name, policy.rewrite_bytes(value, decision.replacements))
        for name, value in headers.fields
    )
    if fields != headers.fields:
        headers.fields = fields


class SecretGateway:
    def __init__(self) -> None:
        self.snapshot: policy.Snapshot | None = None
        self._loaded_key: tuple[int, int, int] | None = None
        self._failed: tuple[tuple[int, int, int], float] | None = None
        self._last_error: str | None = None
        self._attempted: int | None = None
        self._ca_error: str | None = None
        self._health: bytes | None = None
        self._watcher: asyncio.Task[None] | None = None

    def load(self, loader: object) -> None:
        loader.add_option(  # type: ignore[attr-defined]
            "agent_sec_runtime_dir", str, "", "Runtime directory holding active.json"
        )

    def configure(self, updated: set[str]) -> None:
        options = ctx.options
        # Transparent mode connects to the original destination IP; upstream
        # certificate verification is the only defense against a forged Host.
        if options.ssl_insecure:
            raise exceptions.OptionsError(
                "agent-sec-gateway requires ssl_insecure=false"
            )
        # Streamed bodies skip inspection; dumps and verbose output would
        # record injected credentials.
        if options.stream_large_bodies:
            raise exceptions.OptionsError(
                "agent-sec-gateway forbids stream_large_bodies"
            )
        if options.save_stream_file:
            raise exceptions.OptionsError("agent-sec-gateway forbids save_stream_file")
        if options.flow_detail != 0:
            raise exceptions.OptionsError("agent-sec-gateway requires flow_detail=0")
        if not options.agent_sec_runtime_dir:
            raise exceptions.OptionsError(
                "agent-sec-gateway requires agent_sec_runtime_dir"
            )

    async def running(self) -> None:
        self.reload()
        try:
            self._publish_ca()
        except OSError:
            self._ca_error = "cannot publish the CA certificate"
            logger.warning("agent-sec-gateway %s", self._ca_error)
        self._write_health()
        self._watcher = asyncio.get_running_loop().create_task(self._watch())

    async def done(self) -> None:
        if self._watcher is not None:
            self._watcher.cancel()

    async def _watch(self) -> None:
        while True:
            await asyncio.sleep(POLL_SECONDS)
            try:
                self.reload()
            except Exception:  # noqa: BLE001 - the watcher must outlive any failure
                logger.exception("agent-sec-gateway reload failed")

    @property
    def _runtime(self) -> str:
        return str(ctx.options.agent_sec_runtime_dir)

    def reload(self) -> None:
        """Swaps in the manifest's generation; a failed load keeps the previous one."""
        try:
            info = os.lstat(os.path.join(self._runtime, MANIFEST_NAME))
        except FileNotFoundError:
            self.snapshot, self._loaded_key, self._failed = None, None, None
            self._last_error, self._attempted = None, None
            self._write_health()
            return
        key = (info.st_ino, info.st_mtime_ns, info.st_size)
        now = time.monotonic()
        if key == self._loaded_key:
            return
        if (
            self._failed
            and self._failed[0] == key
            and now - self._failed[1] < RETRY_SECONDS
        ):
            return
        try:
            raw = _read_runtime_file(
                self._runtime, MANIFEST_NAME, policy.MAX_MANIFEST_BYTES
            )
            self._attempted = _peek_generation(raw)
            snapshot = policy.build_snapshot(
                raw, lambda name, limit: _read_runtime_file(self._runtime, name, limit)
            )
        except (policy.LoadError, OSError) as error:
            # Transient failures are retried; the key is recorded only on success.
            message = str(error) if isinstance(error, policy.LoadError) else "io error"
            if message != self._last_error:
                logger.warning("agent-sec-gateway rejected generation: %s", message)
            self._last_error, self._failed = message, (key, now)
        else:
            self.snapshot, self._loaded_key = snapshot, key
            self._last_error, self._failed = None, None
            logger.info("agent-sec-gateway loaded generation %d", snapshot.generation)
        self._write_health()

    def _write_health(self) -> None:
        snapshot = self.snapshot
        health = json.dumps(
            {
                "version": HEALTH_VERSION,
                "loadedGeneration": snapshot.generation if snapshot else None,
                "listeners": sorted(snapshot.listeners) if snapshot else [],
                "lastAttempt": {
                    "generation": self._attempted,
                    "error": self._last_error,
                },
                "caError": self._ca_error,
            }
        ).encode()
        if health == self._health:
            return
        try:
            _write_atomic(os.path.join(self._runtime, STATUS_DIR), HEALTH_NAME, health)
        except OSError:
            logger.warning("agent-sec-gateway cannot write health")
        else:
            self._health = health

    def _publish_ca(self) -> None:
        confdir = os.path.expanduser(ctx.options.confdir)
        with open(os.path.join(confdir, "mitmproxy-ca-cert.pem"), "rb") as handle:
            certificate = handle.read()
        _write_atomic(os.path.join(self._runtime, STATUS_DIR), CA_NAME, certificate)

    def request(self, flow: http.HTTPFlow) -> None:
        if flow.response is not None:
            return
        try:
            self._decide(flow)
        except Exception:  # noqa: BLE001 - an undecided request must not be forwarded
            logger.exception("agent-sec-gateway request hook failed")
            flow.metadata.pop(METADATA_KEY, None)
            flow.response = _reject_response("gateway_error", 500)

    def _decide(self, flow: http.HTTPFlow) -> None:
        request = flow.request
        facts = policy.RequestFacts(
            port=flow.client_conn.proxy_mode.listen_port(),
            tls=flow.client_conn.tls_established,
            sni=flow.client_conn.sni,
            authority=request.host_header,
            target_authority=request.authority or None,
            method=request.method,
            path=request.path,
            headers=tuple((k, v) for k, v in request.headers.items(multi=True)),
            body=request.get_content(strict=False) or b"",
        )
        decision = policy.decide_request(self.snapshot, facts)
        if decision.action == policy.ACTION_REJECT:
            # A rejected path may itself contain the exposed real value.
            path = "-" if decision.code == "real_credential_exposed" else request.path
            logger.warning(
                "agent-sec-gateway reject code=%s agent=%s method=%s host=%s path=%s",
                decision.code,
                decision.agent,
                request.method,
                request.host_header,
                path.partition("?")[0],
            )
            status = _STATUS_BY_CODE.get(decision.code, 403)
            flow.response = _reject_response(decision.code, status)
            return
        flow.metadata[METADATA_KEY] = decision
        if decision.action == policy.ACTION_INJECT:
            assert decision.authorization is not None
            request.headers["Authorization"] = decision.authorization
            if decision.response_rewrite == policy.REWRITE_BUFFERED:
                # Only encodings the rewriter can decode may come back.
                request.headers["Accept-Encoding"] = "gzip"

    def responseheaders(self, flow: http.HTTPFlow) -> None:
        response = flow.response
        if response is None:
            return
        decision = _injected(flow)
        if decision is None:
            response.stream = True
            return
        try:
            _rewrite_fields(response.headers, decision)
            if decision.response_rewrite != policy.REWRITE_BUFFERED:
                response.stream = True
                return
            response.stream = False
            length = response.headers.get("Content-Length", "")
            if length.isdigit() and int(length) > RESPONSE_BUFFER_LIMIT:
                # The body could echo the real value; refusing beats delivering it unchecked.
                flow.kill()
        except Exception:  # noqa: BLE001 - an unchecked response must not be delivered
            logger.exception("agent-sec-gateway responseheaders hook failed")
            flow.kill()

    def response(self, flow: http.HTTPFlow) -> None:
        decision = _injected(flow)
        if decision is None or flow.response is None:
            return
        try:
            self._inspect_response(flow, decision)
        except Exception:  # noqa: BLE001 - an unchecked response must not be delivered
            logger.exception("agent-sec-gateway response hook failed")
            flow.kill()

    def _inspect_response(self, flow: http.HTTPFlow, decision: policy.Decision) -> None:
        response = flow.response
        assert response is not None
        if response.trailers is not None:
            _rewrite_fields(response.trailers, decision)
        if decision.response_rewrite != policy.REWRITE_BUFFERED:
            return
        if response.stream:
            flow.kill()
            return
        encoding = response.headers.get("Content-Encoding", "identity").strip().lower()
        if encoding not in ("identity", "gzip", ""):
            flow.response = _reject_response("response_encoding_unsupported", 502)
            return
        try:
            body = response.get_content(strict=True) or b""
        except ValueError:
            flow.response = _reject_response("response_encoding_invalid", 502)
            return
        if len(body) > RESPONSE_BUFFER_LIMIT:
            flow.response = _reject_response("response_too_large", 502)
            return
        _rewrite_fields(response.headers, decision)
        rewritten = policy.rewrite_bytes(body, decision.replacements)
        if rewritten != body:
            response.set_content(rewritten)
            response.headers.pop("ETag", None)
            response.headers.pop("Content-MD5", None)


addons = [SecretGateway()]
