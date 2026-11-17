/** Bounded readiness and one immutable Task at an installed protection boundary. */
import { addAbortListener } from "node:events";
import { setTimeout as delay } from "node:timers/promises";
import { AwError } from "./transport.mjs";

export class Admission {
  constructor(instance, { events, timeoutMs = 5000, cwd, environment }) {
    if (!Number.isInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 60000)
      throw new AwError("invalid_attach_timeout");
    if (
      !Array.isArray(events) ||
      events.some(
        (event) => !Object.hasOwn(instance.options.capabilities.events, event),
      )
    )
      throw new AwError("admission_event_unavailable");
    this.instance = instance;
    this.options = {
      events: [...new Set(events)],
      timeoutMs,
      cwd,
      environment: environment && { ...environment },
    };
    this.controller = new AbortController();
    // Native Sources install these callbacks before the Task routes are available.
    this.initial = {
      events: {},
      hooks: this.options.events.map((event) => ({
        event,
        budget_ms: 55000 + timeoutMs,
      })),
    };
  }
  get fallback() {
    return this.task?.fallback ?? false;
  }
  get routes() {
    return this.task?.routes ?? this.initial;
  }
  prepare(signal) {
    signal?.throwIfAborted();
    this.controller.signal.throwIfAborted();
    this.preparing ??= this.open();
    if (!signal) return this.preparing;
    return new Promise((resolve, reject) => {
      const listener = addAbortListener(signal, () => reject(signal.reason));
      this.preparing.then(
        (value) => {
          listener[Symbol.dispose]();
          resolve(value);
        },
        (error) => {
          listener[Symbol.dispose]();
          reject(error);
        },
      );
    });
  }
  async open() {
    const required = this.instance.options.availability === "required";
    if (required || this.options.events.includes("input.submit")) {
      const deadline = AbortSignal.timeout(this.options.timeoutMs);
      const bounded = AbortSignal.any([this.controller.signal, deadline]);
      try {
        while (this.instance.state !== "ready") {
          if (this.instance.closed) throw new AwError("instance_closed");
          const error = this.instance.lastError;
          if (error && !error.unavailable) throw error;
          await delay(10, undefined, { signal: bounded });
        }
      } catch (error) {
        // Optional input checks still wait for preparation; only their deadline
        // may fall back. Explicit errors and cancellation retain precedence.
        if (
          required ||
          error.name !== "AbortError" ||
          !deadline.aborted ||
          this.controller.signal.aborted
        )
          throw error;
      }
    }
    this.controller.signal.throwIfAborted();
    this.task = await this.instance.task({
      signal: this.controller.signal,
      cwd: this.options.cwd,
      environment: this.options.environment,
    });
  }
  async emit(event, payload, signal) {
    await this.prepare(signal);
    return this.task.emit(event, payload, signal);
  }
  invoke(hook, payload, signal) {
    return this.task.invoke(hook, payload, signal);
  }
  abortEvent(hook, payload) {
    return this.task?.abortEvent(hook, payload);
  }
  cancel() {
    this.controller.abort();
  }
  close() {
    this.closing ??= (async () => {
      this.cancel();
      try {
        await this.preparing;
      } catch {
        /* Preserve the admission failure. */
      }
      await this.task?.close();
    })();
    return this.closing;
  }
}
