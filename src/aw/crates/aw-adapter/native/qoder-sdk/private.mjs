/** Explicit tool consumers own versioned credentials independently of daemon auth. */
import { AsyncLocalStorage } from "node:async_hooks";

export const PRIVATE_EXECUTION_VERSION = "aw.private-execution/v1";
const current = new AsyncLocalStorage();
let cleanupFailure;

/** Refuse new Queries after a lease could not be safely retired. */
export function checkExecutionCleanup() {
  if (cleanupFailure) throw cleanupFailure;
}

function failure(message) {
  // Application exceptions may contain credentials; never retain their cause.
  return new Error(`AW private execution ${message}`);
}

function environment(value) {
  if (!value || Object.getPrototypeOf(value) !== Object.prototype ||
      Object.entries(value).some(([key, item]) => !key || /[=\0]/.test(key) ||
        typeof item !== "string" || item.includes("\0")))
    throw failure("environment is invalid");
  return { ...value };
}

function admit(lease) {
  if (!lease || lease.version !== PRIVATE_EXECUTION_VERSION ||
      typeof lease.redact !== "function" || typeof lease.release !== "function")
    throw failure("contract is unsupported");
  const admitted = { environment: environment(lease.environment),
    hookEnvironment: environment(lease.hookEnvironment),
    secrets: environment(lease.secrets ?? {}), redact: lease.redact };
  if (Object.keys(admitted.secrets).some(key => Object.hasOwn(admitted.hookEnvironment, key)) ||
      Object.values(admitted.secrets).some(secret => secret &&
        Object.entries(admitted.hookEnvironment).flat().some(value => value.includes(secret))))
    throw failure("Hook environment contains a declared secret");
  return admitted;
}

function freeze(value) {
  if (value && typeof value === "object") {
    for (const item of Object.values(value)) freeze(item);
    Object.freeze(value);
  }
  return value;
}

function context(value) {
  if (!value || typeof value.invocationId !== "string" || !value.invocationId ||
      typeof value.toolName !== "string" || !value.toolName || !value.input ||
      Object.getPrototypeOf(value.input) !== Object.prototype)
    throw failure("invocation is invalid");
  // The adapter supplies verified JSON arguments; cloning prevents factory
  // writes from changing either the scan snapshot or the native handler input.
  return freeze(structuredClone(value));
}

function active() {
  const lease = current.getStore();
  if (!lease?.active) throw failure("has no active tool environment");
  return lease;
}

/** Copy the environment at an explicit subprocess or client construction seam. */
export function executionEnvironment() {
  const lease = active();
  return { ...lease.environment, ...lease.secrets };
}

/** Copy the admitted Hook view; private values never enter process.env. */
export function hookEnvironment() {
  return { ...active().hookEnvironment };
}

/** One Query owner tracks a separate lease for every explicit tool operation. */
export class ExecutionOwner {
  #factory;
  #timeout;
  #onFailure;
  #failure;
  #cleanupFailure;
  #closing = false;
  #closed;
  #pending = new Map();

  constructor(factory, { cleanupTimeoutMs = 60000, onFailure = () => {} } = {}) {
    if (typeof factory !== "function" || !Number.isSafeInteger(cleanupTimeoutMs) ||
        cleanupTimeoutMs < 1 || cleanupTimeoutMs > 60000 || typeof onFailure !== "function")
      throw failure("owner options are invalid");
    this.#factory = factory;
    this.#timeout = cleanupTimeoutMs;
    this.#onFailure = onFailure;
  }

  #fail(error, cleanup = false) {
    this.#failure ??= error;
    if (cleanup) {
      this.#cleanupFailure ??= error;
      cleanupFailure ??= error;
    }
    try { this.#onFailure(this.#failure); }
    catch { /* The owner remains fenced even if the notification fails. */ }
    return this.#failure;
  }

  /** Reject dispatch after failure, close, or uncertain process cleanup. */
  check() {
    if (cleanupFailure || this.#failure || this.#closing)
      throw cleanupFailure ?? this.#failure ?? failure("owner is closing");
  }

  /** Await the original factory and operation; callers must pass the raw handler. */
  async run(invocation, operation, { signal } = {}) {
    this.check();
    if (typeof operation !== "function") throw this.#fail(failure("operation is invalid"));
    let approved;
    try { approved = context(invocation); }
    catch { throw this.#fail(failure("invocation is invalid")); }
    if (this.#pending.has(approved.invocationId))
      throw this.#fail(failure("invocation is already active"));
    const controller = new AbortController();
    const abort = () => controller.abort();
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    const record = { controller };
    this.#pending.set(approved.invocationId, record);
    record.promise = Promise.resolve().then(() => this.#execute(approved, operation, controller.signal));
    try { return await record.promise; }
    finally {
      signal?.removeEventListener("abort", abort);
      this.#pending.delete(approved.invocationId);
    }
  }

  async #execute(approved, operation, signal) {
    let lease;
    let admitted;
    try {
      if (signal.aborted) throw failure("was cancelled before acquisition");
      lease = await this.#factory(approved, { signal });
      admitted = { ...admit(lease), active: true };
      this.check();
      if (signal.aborted) throw failure("was cancelled before dispatch");
      const value = await current.run(admitted, () => operation(approved, { signal }));
      if (value?.[Symbol.asyncIterator] || value?.[Symbol.iterator] && typeof value !== "string" &&
          !Array.isArray(value))
        throw failure("requires a complete result before delivery");
      if (signal.aborted) throw failure("was cancelled before delivery");
      const result = admitted.redact(structuredClone(value));
      if (result && typeof result.then === "function") {
        // Drain even an invalid async redactor before releasing its credentials.
        await result;
        throw failure("requires a synchronous redactor");
      }
      return result;
    } catch {
      throw this.#fail(failure("failed; tool result withheld"));
    } finally {
      if (lease !== undefined) {
        try {
          if (typeof lease?.release !== "function") throw failure("has no release callback");
          // The raw operation's finally has settled before the lease is revoked.
          await lease.release();
        } catch {
          throw this.#fail(failure("release failed; worker restart required"), true);
        } finally {
          if (admitted) {
            admitted.active = false;
            for (const view of [admitted.environment, admitted.hookEnvironment, admitted.secrets])
              for (const key of Object.keys(view)) delete view[key];
          }
        }
      }
    }
  }

  /** Abort owned work and bound the join without detaching its original finally. */
  close(timeoutMs = this.#timeout) {
    if (this.#closed) return this.#closed;
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > this.#timeout)
      throw failure("cleanup budget is invalid");
    this.#closing = true;
    for (const { controller } of this.#pending.values()) controller.abort();
    this.#closed = this.#drain(timeoutMs);
    return this.#closed;
  }

  async #drain(timeoutMs) {
    let timer;
    try {
      await Promise.race([
        Promise.allSettled([...this.#pending.values()].map(record => record.promise)),
        new Promise((_, reject) => {
          timer = setTimeout(() => reject(this.#fail(
            failure("cleanup deadline exceeded; worker restart required"), true)), timeoutMs);
        }),
      ]);
      // Dispatch failure fences this Query; confirmed cleanup still retires its scope.
      if (this.#cleanupFailure) throw this.#cleanupFailure;
    } finally { clearTimeout(timer); }
  }
}
