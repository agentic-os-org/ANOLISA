/** Bounded Linux Unix-socket transport. Each request is sent once. */
import net from "node:net";
import { lstat } from "node:fs/promises";
import { dirname, isAbsolute } from "node:path";

export const SERVICE = "aw-service/v1alpha7";
export const SDK = "aw-sdk/v1";
const MAX_FRAME = 9 * 1024 * 1024;
const unavailable = new Set([
  "ENOENT",
  "ECONNREFUSED",
  "ECONNRESET",
  "EPIPE",
  "ETIMEDOUT",
]);
export class AwError extends Error {
  constructor(code, { transport = false, cause, message } = {}) {
    super(message ?? code, { cause });
    this.name = "AwError";
    this.code = code;
    this.unavailable = transport && unavailable.has(code);
  }
}
export function sameIdentity(a, b) {
  return (
    a &&
    b &&
    a.generation === b.generation &&
    a.config_revision === b.config_revision
  );
}
export async function rpc(endpoint, identity, operation, timeoutMs, signal) {
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 60000)
    throw new AwError("invalid_timeout");
  if (!isAbsolute(endpoint)) throw new AwError("endpoint_must_be_absolute");
  signal?.throwIfAborted();
  const expires = process.hrtime.bigint() + BigInt(timeoutMs) * 1000000n;
  try {
    const parent = await lstat(dirname(endpoint));
    const socket = await lstat(endpoint);
    if (
      !parent.isDirectory() ||
      parent.uid !== process.geteuid() ||
      parent.mode & 0o077 ||
      !socket.isSocket() ||
      socket.uid !== process.geteuid() ||
      socket.mode & 0o077
    )
      throw new AwError("endpoint_identity");
  } catch (error) {
    if (error instanceof AwError) throw error;
    throw new AwError(error.code, { transport: true, cause: error });
  }
  signal?.throwIfAborted();
  // Encode the monotonic integer exactly, including after 104 days of host uptime.
  const payload = Buffer.from(
    `{"api_version":${JSON.stringify(SERVICE)},"identity":${JSON.stringify(identity ?? null)},"deadline_ns":${expires},"operation":${JSON.stringify(operation)}}`,
  );
  if (payload.length > MAX_FRAME) throw new AwError("frame_limit");
  const prefix = Buffer.alloc(4);
  prefix.writeUInt32BE(payload.length);
  return await new Promise((resolve, reject) => {
    const socket = net.createConnection({ path: endpoint });
    let data = Buffer.alloc(0),
      size,
      settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
      socket.destroy();
      if (error) reject(error);
      else resolve(value);
    };
    const abort = () =>
      finish(signal.reason ?? new DOMException("Cancelled", "AbortError"));
    const left = Number((expires - process.hrtime.bigint()) / 1000000n);
    const timer = setTimeout(
      () => finish(new AwError("ETIMEDOUT", { transport: true })),
      Math.max(0, left),
    );
    signal?.addEventListener("abort", abort, { once: true });
    socket.on("connect", () => {
      if (signal?.aborted) return abort();
      socket.write(Buffer.concat([prefix, payload]));
    });
    socket.on("error", (error) =>
      finish(new AwError(error.code, { transport: true, cause: error })),
    );
    socket.on("end", () => {
      if (!settled) finish(new AwError("ECONNRESET", { transport: true }));
    });
    socket.on("data", (chunk) => {
      if (settled) return;
      if (process.hrtime.bigint() >= expires)
        return finish(new AwError("ETIMEDOUT", { transport: true }));
      if (data.length + chunk.length > MAX_FRAME + 4)
        return finish(new AwError("frame_limit"));
      data = Buffer.concat([data, chunk]);
      if (data.length >= 4 && size === undefined) {
        size = data.readUInt32BE(0);
        if (size > MAX_FRAME || size === 0)
          return finish(new AwError("frame_limit"));
      }
      if (size === undefined || data.length < size + 4) return;
      try {
        if (data.length !== size + 4) throw new AwError("invalid_frame");
        const response = JSON.parse(data.subarray(4).toString("utf8"));
        if (response.api_version !== SERVICE)
          throw new AwError("protocol_version");
        if (
          !response.identity?.generation ||
          !response.identity?.config_revision
        )
          throw new AwError("invalid_response");
        if (identity && !sameIdentity(identity, response.identity))
          throw new AwError("ECONNRESET", { transport: true });
        if (response.error !== null) {
          if (typeof response.error !== "string" || response.result !== null)
            throw new AwError("invalid_response");
          throw new AwError(response.error);
        }
        if (!response.result || typeof response.result !== "object")
          throw new AwError("invalid_response");
        finish(null, response);
      } catch (error) {
        finish(
          error instanceof AwError
            ? error
            : new AwError("invalid_response", { cause: error }),
        );
      }
    });
    if (signal?.aborted) abort();
  });
}
