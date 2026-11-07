"""mitmproxy addon for the AgentSecCore secret gateway.

Each listener port belongs to one canonical Scope. The addon serves the
generation named by `<runtime>/active.json`, replaces a domain's fake with the
real credential only on requests its plan allows, and reports what it loaded in
`<runtime>/status/health.json`. Without a loaded generation every request is
rejected, and any failure inside a hook rejects or kills the flow: the gateway
never degrades to passthrough.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import stat
import time
import zlib

import asc_gateway_policy as policy
from mitmproxy import ctx, exceptions, http
from mitmproxy.addons.termlog import TermLogHandler
from mitmproxy.log import LegacyLogEvents
from mitmproxy.net.http.http1 import expected_http_body_size
from mitmproxy.proxy import commands, events, layer
from mitmproxy.proxy.layers.http import (
    HttpLayer,
    HttpStream,
    RequestData,
    ResponseData,
)

POLL_SECONDS = 0.5
RETRY_SECONDS = 5.0
RESPONSE_BUFFER_LIMIT = 8 * 1024 * 1024
MANIFEST_NAME = "active.json"
STATUS_DIR = "status"
HEALTH_NAME = "health.json"
CA_NAME = "ca-cert.pem"
HEALTH_VERSION = 1
METADATA_KEY = "agent_sec"
PII_METADATA_KEY = "agent_sec_pii"
PII_BODY_LIMIT = 256 * 1024
PII_TIMEOUT = 5
PII_MAX_ACTIVE = 2

# Codes a client should retry on a fresh connection instead of treating as final.
_STATUS_BY_CODE = {"sni_host_mismatch": 421}

logger = logging.getLogger("agent_sec_gateway")
_SAFE_DIAGNOSTICS = frozenset(
    {
        "agent-sec-gateway cannot publish the CA certificate",
        "agent-sec-gateway reload failed",
        "agent-sec-gateway rejected generation",
        "agent-sec-gateway loaded generation",
        "agent-sec-gateway cannot write health",
        "agent-sec-gateway request hook failed",
        "agent-sec-gateway rejected request",
        "agent-sec-gateway responseheaders hook failed",
        "agent-sec-gateway response hook failed",
        "agent-sec-gateway upstream error; injected flow killed",
    }
)


class _SafeDiagnostics(logging.Filter):
    def filter(self, record: logging.LogRecord) -> bool:
        # Never format dependency messages, raw bytes, arguments or exception text.
        return (
            record.name == logger.name
            and isinstance(record.msg, str)
            and record.msg in _SAFE_DIAGNOSTICS
            and not record.args
            and not record.exc_info
            and not record.exc_text
            and not record.stack_info
        )


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


def _abort_flow(flow: http.HTTPFlow) -> None:
    if flow.killable:
        flow.kill()
    handler = ctx.master.addons.get("proxyserver").connections.get(flow.client_conn.id)
    if handler is not None:
        for connection in (flow.client_conn, flow.server_conn):
            transport = handler.transports.get(connection)
            if transport is not None and transport.writer is not None:
                transport.writer.close()
            handler.close_connection(connection)


class _GatewayHttpStream(HttpStream):
    def state_consume_request_body(
        self, event: events.Event
    ) -> layer.CommandGenerator[None]:
        if (
            isinstance(event, RequestData)
            and self.flow.metadata.get(PII_METADATA_KEY)
            and len(self.request_body_buf) + len(event.data) > PII_BODY_LIMIT
        ):
            # Bound actual bytes, including chunked uploads, before native buffering.
            self.request_body_buf.clear()
            self.client_state = self.server_state = self.state_errored
            _abort_flow(self.flow)
            return
        yield from super().state_consume_request_body(event)

    def send_response(
        self, already_streamed: bool = False
    ) -> layer.CommandGenerator[None]:
        yield from super().send_response(already_streamed)
        if (
            _injected(self.flow) is not None
            and self.flow.live
            and self.server_state == self.state_done
            and self.client_state != self.state_done
        ):
            # Fence queued upload events before finishing; HTTP/2 may still await WINDOW_UPDATE.
            self.client_state = self.state_errored
            yield from self.flow_done()
            if not self.flow.request.is_http2:
                yield commands.CloseConnection(self.context.client)
            yield commands.CloseConnection(self.context.server)


class _GatewayHttpLayer(HttpLayer):
    def make_stream(self, stream_id: int) -> layer.CommandGenerator[None]:
        self.streams[stream_id] = _GatewayHttpStream(self.context.fork(), stream_id)
        yield from self.event_to_child(self.streams[stream_id], events.Start())

    def event_to_child(
        self, child: layer.Layer, event: events.Event
    ) -> layer.CommandGenerator[None]:
        # The stream callback reserves b"" for EOF, but HTTP/2 allows empty nonterminal DATA.
        if isinstance(event, ResponseData) and not event.data:
            return
        yield from super().event_to_child(child, event)


class _BufferedResponse:
    def __init__(
        self,
        flow: http.HTTPFlow,
        decision: policy.Decision,
        encoding: str,
        rejection: bytes | None = None,
    ) -> None:
        self.flow = flow
        self.decision = decision
        self.body = bytearray()
        self.received = 0
        self.failed = False
        self.rejection = rejection
        self.decoder = zlib.decompressobj(31) if encoding == "gzip" else None

    def __call__(self, chunk: bytes) -> bytes | list[bytes]:
        if self.failed:
            return []
        try:
            if chunk:
                self.received += len(chunk)
                if self.received > RESPONSE_BUFFER_LIMIT:
                    raise ValueError("encoded response exceeds limit")
                if self.rejection is not None:
                    return []
                decoded = (
                    self.decoder.decompress(
                        chunk, RESPONSE_BUFFER_LIMIT + 1 - len(self.body)
                    )
                    if self.decoder is not None
                    else chunk
                )
                if len(self.body) + len(decoded) > RESPONSE_BUFFER_LIMIT:
                    raise ValueError("decoded response exceeds limit")
                self.body.extend(decoded)
                if self.decoder is not None and self.decoder.unused_data:
                    raise ValueError("trailing compressed response data")
                return []
            if self.rejection is not None:
                return self.rejection
            if self.decoder is not None and not self.decoder.eof:
                raise ValueError("incomplete compressed response")
            body = policy.rewrite_bytes(bytes(self.body), self.decision.replacements)
            self.body.clear()
            if self.decoder is not None:
                encoder = zlib.compressobj(wbits=31)
                body = encoder.compress(body) + encoder.flush()
            return body
        except Exception:  # noqa: BLE001 - unchecked bodies must never reach clients
            self.failed = True
            self.body.clear()
            logger.error("agent-sec-gateway response hook failed")
            _abort_flow(self.flow)
            return []


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
        self._log_filter = _SafeDiagnostics()
        self._pii_active = 0

    def load(self, loader: object) -> None:
        loader.add_option(  # type: ignore[attr-defined]
            "agent_sec_runtime_dir", str, "", "Runtime directory holding active.json"
        )
        loader.add_option(  # type: ignore[attr-defined]
            "agent_sec_pii_scan",
            bool,
            False,
            "Require a complete PII pass before forwarding request bodies",
        )
        loader.add_option(  # type: ignore[attr-defined]
            "agent_sec_pii_cli",
            str,
            "/usr/bin/agent-sec-cli",
            "Absolute path to the Rust PII CLI",
        )

    def next_layer(self, nextlayer: layer.NextLayer) -> None:
        # Script hooks run before the default selector; retain its protocol and TLS decisions.
        ctx.master.addons.get("nextlayer").next_layer(nextlayer)
        parent = None
        current = nextlayer.layer
        while current is not None and not isinstance(current, HttpLayer):
            parent, current = current, getattr(current, "child_layer", None)
        if isinstance(current, HttpLayer):
            current.context.layers.remove(current)
            wrapped = _GatewayHttpLayer(current.context, current.mode)
            if parent is None:
                nextlayer.layer = wrapped
            else:
                parent.child_layer = wrapped

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
        if options.store_streamed_bodies:
            raise exceptions.OptionsError(
                "agent-sec-gateway forbids store_streamed_bodies"
            )
        if options.save_stream_file:
            raise exceptions.OptionsError("agent-sec-gateway forbids save_stream_file")
        if options.flow_detail != 0:
            raise exceptions.OptionsError("agent-sec-gateway requires flow_detail=0")
        if not options.agent_sec_runtime_dir:
            raise exceptions.OptionsError(
                "agent-sec-gateway requires agent_sec_runtime_dir"
            )
        if not os.path.isabs(options.agent_sec_pii_cli):
            raise exceptions.OptionsError("agent_sec_pii_cli must be an absolute path")
        if options.termlog_verbosity not in ("warn", "error"):
            options.update(termlog_verbosity="warn")
        # Handler filters cover propagated logs, including pre-flow parser logs and ERROR tracebacks.
        # Keep ErrorCheck unfiltered so dependency startup failures still stop the process.
        for handler in logging.getLogger().handlers:
            if isinstance(handler, (TermLogHandler, LegacyLogEvents)):
                handler.addFilter(self._log_filter)

    async def running(self) -> None:
        self.reload()
        try:
            self._publish_ca()
        except OSError:
            self._ca_error = "cannot publish the CA certificate"
            logger.warning("agent-sec-gateway cannot publish the CA certificate")
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
                logger.error("agent-sec-gateway reload failed")

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
                logger.warning("agent-sec-gateway rejected generation")
            self._last_error, self._failed = message, (key, now)
        else:
            self.snapshot, self._loaded_key = snapshot, key
            self._last_error, self._failed = None, None
            logger.info("agent-sec-gateway loaded generation")
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

    def requestheaders(self, flow: http.HTTPFlow) -> None:
        if flow.response is not None:
            return
        try:
            self._decide(flow)
            flow.request.stream = flow.response is None
            if flow.response is None and ctx.options.agent_sec_pii_scan:
                if "upgrade" in flow.request.headers:
                    flow.response = _reject_response("pii_request_unsupported", 415)
                elif (expected_http_body_size(flow.request) or 0) > PII_BODY_LIMIT:
                    flow.response = _reject_response("pii_body_too_large", 413)
                else:
                    flow.metadata[PII_METADATA_KEY] = True
                    flow.request.stream = False
        except Exception:  # noqa: BLE001 - an undecided request must not be forwarded
            logger.error("agent-sec-gateway request hook failed")
            flow.metadata.pop(METADATA_KEY, None)
            flow.response = _reject_response("gateway_error", 500)
        if flow.response is not None and (
            flow.request.is_http2
            or flow.request.is_http3
            or expected_http_body_size(flow.request) != 0
        ):
            # Header rejection must not wait for or buffer an untrusted upload.
            _abort_flow(flow)

    async def request(self, flow: http.HTTPFlow) -> None:
        if flow.response is not None or not flow.metadata.get(PII_METADATA_KEY):
            return
        try:
            body = flow.request.raw_content
            encoding = flow.request.headers.get("Content-Encoding", "identity").lower()
            content_type = (
                flow.request.headers.get("Content-Type", "")
                .split(";", 1)[0]
                .strip()
                .lower()
            )
            if (
                body is None
                or len(body) > PII_BODY_LIMIT
                or encoding not in ("", "identity")
                or flow.request.trailers
                or (body and content_type not in ("text/plain", "application/json"))
            ):
                flow.response = _reject_response("pii_request_unsupported", 415)
                return
            text = body.decode("utf-8")
            if body and content_type == "application/json":
                # Preserve duplicate keys and decode escapes before detection.
                text = _pii_json_text(json.loads(text, object_pairs_hook=list))
            scanned = text.encode("utf-8")
            if len(scanned) > PII_BODY_LIMIT:
                flow.response = _reject_response("pii_body_too_large", 413)
                return
        except (ValueError, RecursionError):
            flow.response = _reject_response("pii_request_invalid", 400)
            return

        if self._pii_active >= PII_MAX_ACTIVE:
            flow.response = _reject_response("pii_scan_busy", 503)
            return
        self._pii_active += 1
        try:
            rejection = await self._scan_pii(scanned)
            if rejection is not None:
                flow.response = _reject_response(*rejection)
        except asyncio.CancelledError:
            _abort_flow(flow)
            raise
        except Exception:  # noqa: BLE001 - no scanner failure may forward the request
            flow.response = _reject_response("pii_scan_unavailable", 503)
        finally:
            self._pii_active -= 1

    async def _scan_pii(self, text: bytes) -> tuple[str, int] | None:
        # ponytail: one CLI process per scan; replace with direct UDS only if measured throughput requires it.
        process = None
        try:
            async with asyncio.timeout(PII_TIMEOUT):
                process = await asyncio.create_subprocess_exec(
                    ctx.options.agent_sec_pii_cli,
                    "--timeout-ms",
                    str(PII_TIMEOUT * 1000),
                    "scan-pii",
                    "--stdin",
                    "--format",
                    "json",
                    stdin=asyncio.subprocess.PIPE,
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.DEVNULL,
                )
                output, _ = await process.communicate(text)
            if process.returncode != 0 or len(output) > 1024 * 1024:
                return "pii_scan_unavailable", 503
            report = json.loads(output)
            if report["ok"] is not True:
                return "pii_scan_unavailable", 503
            if report["verdict"] in ("warn", "deny"):
                return "pii_blocked", 403
            summary = report["summary"]
            if (
                report["verdict"] != "pass"
                or summary["execution_status"] != "completed"
                or summary["coverage"]["status"] != "complete"
                or summary["truncated"] is not False
                or summary["scanned_bytes"] != len(text)
            ):
                return "pii_scan_incomplete", 503
            return None
        finally:
            if process is not None and process.returncode is None:
                with contextlib.suppress(ProcessLookupError):
                    process.kill()
                await process.wait()

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
        )
        decision = policy.decide_request(self.snapshot, facts)
        if decision.action == policy.ACTION_REJECT:
            # Untrusted request fields can contain fake or real values even on unrelated failures.
            logger.warning("agent-sec-gateway rejected request")
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
            response.headers.pop("ETag", None)
            response.headers.pop("Content-MD5", None)
            if expected_http_body_size(flow.request, response) == 0:
                response.headers.pop("Transfer-Encoding", None)
                response.stream = True
                return
            length = response.headers.get("Content-Length", "")
            if length.isdigit() and int(length) > RESPONSE_BUFFER_LIMIT:
                flow.kill()
                return
            encoding = (
                response.headers.get("Content-Encoding", "identity").strip().lower()
            )
            if encoding not in ("identity", "gzip", ""):
                rejected = _reject_response("response_encoding_unsupported", 502)
                flow.response = rejected
                rejected.stream = _BufferedResponse(
                    flow, decision, "identity", rejected.raw_content
                )
                return
            response.headers.pop("Content-Length", None)
            if flow.request.is_http2 or flow.request.is_http3:
                response.headers.pop("Transfer-Encoding", None)
            else:
                response.headers["Transfer-Encoding"] = "chunked"
            response.stream = _BufferedResponse(flow, decision, encoding)
        except Exception:  # noqa: BLE001 - an unchecked response must not be delivered
            logger.error("agent-sec-gateway responseheaders hook failed")
            flow.kill()

    def response(self, flow: http.HTTPFlow) -> None:
        decision = _injected(flow)
        if decision is None or flow.response is None:
            return
        try:
            self._inspect_response(flow, decision)
        except Exception:  # noqa: BLE001 - an unchecked response must not be delivered
            logger.error("agent-sec-gateway response hook failed")
            flow.kill()

    def error(self, flow: http.HTTPFlow) -> None:
        if _injected(flow) is None:
            return
        # Validation errors ignore kill; close transports before their error page resumes.
        _abort_flow(flow)
        logger.warning("agent-sec-gateway upstream error; injected flow killed")

    def _inspect_response(self, flow: http.HTTPFlow, decision: policy.Decision) -> None:
        response = flow.response
        assert response is not None
        if response.trailers is not None:
            _rewrite_fields(response.trailers, decision)
        if (
            decision.response_rewrite == policy.REWRITE_BUFFERED
            and not isinstance(response.stream, _BufferedResponse)
            and expected_http_body_size(flow.request, response) != 0
        ):
            flow.kill()


def _pii_json_text(value: object) -> str:
    if isinstance(value, str):
        return value
    if isinstance(value, tuple):
        return ": ".join(_pii_json_text(part) for part in value)
    if isinstance(value, list):
        return "\n".join(_pii_json_text(part) for part in value)
    return json.dumps(value, allow_nan=False)


addons = [SecretGateway()]
