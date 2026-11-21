/** Lossless, Fetch-semantic request/response instrumentation for source-preload.mjs. */
import { isPlainObject, jsonEqual, toolExecuted } from "./source-helpers.mjs";
import { AwError } from "./transport.mjs";

const NULL_BODY_STATUSES = new Set([101, 204, 205, 304]);
const ALLOWED_INPUT_KEYS = new Set(["url", "method", "headers", "body"]);
const HTTP_TOKEN_RE = /^[!#$%&'*+\-.^_`|~0-9A-Za-z]+$/;
const BASE64_RE = /^[A-Za-z0-9+/]*={0,2}$/;

function headersToObject(headers) {
  const out = {};
  headers.forEach((value, key) => {
    out[key] = value;
  });
  return out;
}

async function readBoundedBytes(bodyStream, maxBytes, errorCode) {
  if (!bodyStream) return null;
  const reader = bodyStream.getReader();
  const chunks = [];
  let total = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > maxBytes) {
        await reader.cancel().catch(() => {});
        throw new AwError(errorCode);
      }
      chunks.push(Buffer.from(value.buffer, value.byteOffset, value.byteLength));
    }
  } finally {
    reader.releaseLock();
  }
  return Buffer.concat(chunks, total);
}

function encodeBodySnapshot(bodyBytes) {
  if (bodyBytes === null) return null;
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bodyBytes);
  } catch {
    return { encoding: "base64", data: bodyBytes.toString("base64") };
  }
}

function decodeBase64Strict(data, errorCode) {
  if (
    typeof data !== "string" ||
    data.length % 4 !== 0 ||
    !BASE64_RE.test(data)
  )
    throw new AwError(errorCode);
  return Buffer.from(data, "base64");
}

function projectFetchSnapshot(req, bodyBytes) {
  return {
    url: req.url,
    method: req.method,
    headers: headersToObject(req.headers),
    body: encodeBodySnapshot(bodyBytes),
  };
}

function validateInitialArgs(input, init) {
  const validInput =
    typeof input === "string" ||
    input instanceof URL ||
    input instanceof Request;
  if (!validInput) throw new AwError("source_input_snapshot_required");
  if (init !== undefined && init !== null && typeof init !== "object")
    throw new AwError("source_input_snapshot_required");
  if (init && init.body !== undefined && init.body !== null) {
    const b = init.body;
    const supported =
      typeof b === "string" ||
      ArrayBuffer.isView(b) ||
      b instanceof ArrayBuffer ||
      b instanceof URLSearchParams;
    if (!supported) throw new AwError("source_input_snapshot_required");
  } else if (input instanceof Request && input.body !== null && input.bodyUsed) {
    throw new AwError("source_input_snapshot_required");
  }
}

function buildRequestFromBase(baseReq, { url, method, headers, bodyBytes }) {
  const upper = method.toUpperCase();
  if (bodyBytes !== null && (upper === "GET" || upper === "HEAD"))
    throw new AwError("source_input_replace_unsupported");
  const reqInit = {
    method: upper,
    headers: new Headers(headers),
    signal: baseReq.signal,
    redirect: baseReq.redirect,
    credentials: baseReq.credentials,
    cache: baseReq.cache,
    referrer: baseReq.referrer,
    referrerPolicy: baseReq.referrerPolicy,
    integrity: baseReq.integrity,
    keepalive: baseReq.keepalive,
  };
  if (baseReq.mode && baseReq.mode !== "navigate") reqInit.mode = baseReq.mode;
  if (bodyBytes !== null) reqInit.body = bodyBytes;
  return new Request(url, reqInit);
}

function parseUpdatedFetchInput(updated, maxBodyBytes) {
  if (!isPlainObject(updated))
    throw new AwError("source_input_replace_unsupported");
  for (const key of Object.keys(updated)) {
    if (!ALLOWED_INPUT_KEYS.has(key))
      throw new AwError("source_input_replace_unsupported");
  }
  if (typeof updated.url !== "string" || !updated.url.trim())
    throw new AwError("source_input_replace_unsupported");
  try {
    new URL(updated.url);
  } catch (error) {
    throw new AwError("source_input_replace_unsupported", { cause: error });
  }
  if (
    typeof updated.method !== "string" ||
    !HTTP_TOKEN_RE.test(updated.method)
  )
    throw new AwError("source_input_replace_unsupported");
  if (!isPlainObject(updated.headers))
    throw new AwError("source_input_replace_unsupported");
  const headers = {};
  for (const [k, v] of Object.entries(updated.headers)) {
    if (typeof v !== "string")
      throw new AwError("source_input_replace_unsupported");
    headers[k] = v;
  }
  try {
    new Headers(headers);
  } catch (error) {
    throw new AwError("source_input_replace_unsupported", { cause: error });
  }
  let bodyBytes = null;
  if (updated.body === null || updated.body === undefined) {
    bodyBytes = null;
  } else if (typeof updated.body === "string") {
    bodyBytes = Buffer.from(updated.body, "utf8");
  } else if (
    isPlainObject(updated.body) &&
    updated.body.encoding === "base64" &&
    typeof updated.body.data === "string" &&
    Object.keys(updated.body).length === 2
  ) {
    bodyBytes = decodeBase64Strict(
      updated.body.data,
      "source_input_replace_unsupported",
    );
  } else {
    throw new AwError("source_input_replace_unsupported");
  }
  if (bodyBytes !== null && bodyBytes.byteLength > maxBodyBytes)
    throw new AwError("source_input_too_large");
  return { url: updated.url, method: updated.method, headers, bodyBytes };
}

function buildReplacedResponse(response, rep) {
  let outBody;
  let customHeaders;
  if (typeof rep === "string") {
    outBody = Buffer.from(rep, "utf8");
  } else if (isPlainObject(rep)) {
    if (rep.headers !== undefined) {
      if (!isPlainObject(rep.headers))
        throw new AwError("source_result_replace_unsupported");
      for (const v of Object.values(rep.headers)) {
        if (typeof v !== "string")
          throw new AwError("source_result_replace_unsupported");
      }
      customHeaders = rep.headers;
    }
    if (rep.body_encoding === "empty" && (rep.text === "" || rep.text === undefined)) {
      outBody = null;
    } else if (
      rep.body_encoding === "base64" &&
      typeof rep.body_base64 === "string"
    ) {
      outBody = decodeBase64Strict(
        rep.body_base64,
        "source_result_replace_unsupported",
      );
    } else if (typeof rep.text === "string") {
      outBody = Buffer.from(rep.text, "utf8");
    } else {
      throw new AwError("source_result_replace_unsupported");
    }
  } else {
    throw new AwError("source_result_replace_unsupported");
  }
  if (NULL_BODY_STATUSES.has(response.status)) {
    if (outBody !== null && outBody.byteLength > 0)
      throw new AwError("source_result_replace_unsupported");
    outBody = null;
  }
  const nextHeaders = new Headers(customHeaders ?? response.headers);
  nextHeaders.delete("content-length");
  nextHeaders.delete("content-encoding");
  nextHeaders.delete("etag");
  nextHeaders.delete("content-md5");
  return new Response(outBody, {
    status: response.status,
    statusText: response.statusText,
    headers: nextHeaders,
  });
}

function cancelResponseQuietly(response) {
  try {
    response.body?.cancel?.().catch(() => {});
  } catch {}
}

export function wrapFetch(handle, targetSpec) {
  const originalFetch = globalThis.fetch;
  if (typeof originalFetch !== "function")
    throw new AwError("source_attribute_missing");
  const wrapped = async function awWrappedFetch(input, init) {
    if (!handle.active) return originalFetch.call(this, input, init);
    validateInitialArgs(input, init);
    let effectiveReq;
    try {
      effectiveReq = new Request(input, init);
    } catch (error) {
      throw new AwError("source_input_snapshot_required", { cause: error });
    }
    const signal = effectiveReq.signal;
    const origBodyBytes =
      effectiveReq.body === null
        ? null
        : await readBoundedBytes(
            effectiveReq.body,
            targetSpec.maxBodyBytes,
            "source_input_too_large",
          );
    const snap = projectFetchSnapshot(effectiveReq, origBodyBytes);
    const session = await handle._getSession();
    const {
      callId,
      input: effective,
      decision,
    } = await session.beforeTool(targetSpec.name, snap, { signal });
    let finalReq;
    if (Object.hasOwn(decision, "updated_input")) {
      const revised = parseUpdatedFetchInput(
        decision.updated_input,
        targetSpec.maxBodyBytes,
      );
      try {
        finalReq = buildRequestFromBase(effectiveReq, revised);
      } catch (error) {
        if (error instanceof AwError) throw error;
        throw new AwError("source_input_replace_unsupported", { cause: error });
      }
      const reprojected = projectFetchSnapshot(finalReq, revised.bodyBytes);
      if (!jsonEqual(reprojected, effective))
        throw new AwError("source_input_replace_mismatch");
    } else {
      finalReq = buildRequestFromBase(effectiveReq, {
        url: effectiveReq.url,
        method: effectiveReq.method,
        headers: headersToObject(effectiveReq.headers),
        bodyBytes: origBodyBytes,
      });
    }
    const response = await originalFetch.call(this, finalReq);
    if (!session._observeAfter()) return response;
    let respBytes = null;
    try {
      const inspectCopy = response.clone();
      if (!NULL_BODY_STATUSES.has(response.status) && inspectCopy.body !== null) {
        respBytes = await readBoundedBytes(
          inspectCopy.body,
          targetSpec.maxBodyBytes,
          "source_result_too_large",
        );
      }
      const headerObj = headersToObject(response.headers);
      let afterPayload;
      if (respBytes === null) {
        afterPayload = {
          status: response.status,
          ok: response.ok,
          url: response.url,
          headers: headerObj,
          text: "",
          body_encoding: "empty",
        };
      } else {
        try {
          const text = new TextDecoder("utf-8", { fatal: true }).decode(
            respBytes,
          );
          afterPayload = {
            status: response.status,
            ok: response.ok,
            url: response.url,
            headers: headerObj,
            text,
          };
        } catch {
          afterPayload = {
            status: response.status,
            ok: response.ok,
            url: response.url,
            headers: headerObj,
            body_encoding: "base64",
            body_base64: respBytes.toString("base64"),
          };
        }
      }
      const after = await session.afterTool(
        targetSpec.name,
        effective,
        afterPayload,
        { callId, signal },
      );
      if (!Object.hasOwn(after, "replaced_result")) return response;
      cancelResponseQuietly(response);
      return buildReplacedResponse(response, after.replaced_result);
    } catch (error) {
      cancelResponseQuietly(response);
      throw toolExecuted(error);
    }
  };
  globalThis.fetch = wrapped;
  return () => {
    if (globalThis.fetch === wrapped) globalThis.fetch = originalFetch;
  };
}
