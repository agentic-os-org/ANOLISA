/** Application-owned instances and immutable per-task availability. */
import { readFile, realpath } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { AwError, rpc, SDK, sameIdentity } from "./transport.mjs";
export { AwError } from "./transport.mjs";
export { Admission } from "./admission.mjs";
import { Controller } from "./control.mjs";
const neutral = () => ({
  code: 0,
  stdout: Buffer.alloc(0),
  stderr: Buffer.alloc(0),
});

export class Instance {
  constructor(options) {
    if (!["required", "native_fallback"].includes(options.availability))
      throw new AwError("availability_required");
    if (typeof options.install !== "function")
      throw new AwError("ready_confirmation_required");
    this.options = Object.freeze({
      timeoutMs: 1000,
      reconnectMs: 1000,
      ...options,
      capabilities: structuredClone(options.capabilities),
      environment: options.environment && { ...options.environment },
    });
    if (
      !Number.isInteger(this.options.reconnectMs) ||
      this.options.reconnectMs < 100 ||
      this.options.reconnectMs > 60000
    )
      throw new AwError("invalid_reconnect_interval");
    this.state = "detached";
    this.tasks = new Set();
    this.closed = false;
  }
  async connect() {
    if (this.closed) throw new AwError("instance_closed");
    if (this.state === "ready") return true;
    if (this.connecting) return this.connecting;
    this.connecting = this.prepare().finally(() => {
      this.connecting = undefined;
    });
    return this.connecting;
  }
  async prepare() {
    const o = this.options;
    try {
      const discovery = await rpc(
        o.endpoint,
        null,
        { method: "status" },
        o.timeoutMs,
      );
      if (discovery.result.sdk_protocol !== SDK)
        throw new AwError("sdk_protocol_version");
      const identity = discovery.identity;
      if (this.retired && sameIdentity(this.retired.identity, identity)) {
        await rpc(
          o.endpoint,
          identity,
          {
            method: "registration",
            command: {
              action: "release",
              handle: this.retired.handle,
            },
          },
          o.timeoutMs,
        );
        this.retired.released = true;
      }
      this.retired = undefined;
      this.cwd = await realpath(o.cwd ?? process.cwd());
      this.environment = { ...(o.environment ?? process.env) };
      const stat = await readFile("/proc/self/stat", "utf8");
      const ticks = stat
        .slice(stat.lastIndexOf(")") + 1)
        .trim()
        .split(/\s+/)[19];
      const owner = { pid: process.pid, start_ticks: ticks };
      const recovered =
        this.pending && sameIdentity(this.pending.identity, identity);
      if (!recovered)
        this.pending = { identity, key: { request_id: randomUUID(), owner } };
      const { key } = this.pending;
      const command = recovered
        ? { action: "query", key }
        : {
            action: "register_sdk",
            sdk_version: SDK,
            availability: o.availability,
            key,
            target: o.target,
            capabilities: o.capabilities,
            cwd: this.cwd,
            environment: this.environment,
            ...(o.config === undefined ? {} : { config: o.config }),
          };
      const call = async (operation) =>
        (await rpc(o.endpoint, identity, operation, o.timeoutMs)).result;
      let result;
      try {
        result = await call({ method: "registration", command });
      } catch (error) {
        if (recovered && error.code === "registration_unknown") {
          this.pending = undefined;
          return false;
        }
        throw error;
      }
      if (result.state === "preparing") return false;
      if (["released", "expired"].includes(result.state)) {
        this.pending = undefined;
        return false;
      }
      if (result.state === "failed")
        throw new AwError(result.error ?? "registration_failed");
      if (!["pending_ready", "ready"].includes(result.state))
        throw new AwError("registration_unavailable");
      this.pending.handle = result.handle;
      if ((await o.install(result.routes)) !== true)
        throw new AwError("ready_not_confirmed");
      if (this.closed) throw new AwError("instance_closed");
      const ready = await call({
        method: "registration",
        command: {
          action: "ready",
          handle: result.handle,
          policy_revision: result.policy_revision,
        },
      });
      if (ready.state !== "ready") throw new AwError("ready_not_confirmed");
      const connection = {
        identity,
        handle: result.handle,
        routes: result.routes,
      };
      if (this.confirmControl) await this.confirmControl(connection);
      this.connection = connection;
      this.pending = undefined;
      this.state = "ready";
      this.lastError = undefined;
      return true;
    } catch (error) {
      this.lastError = error;
      this.state = "unavailable";
      if (o.availability === "native_fallback" && error.unavailable)
        return false;
      throw error;
    }
  }
  listen({ automatic = false } = {}) {
    if (this.closed) throw new AwError("instance_closed");
    if (this.controller && !this.controller.finished)
      throw new AwError("sdk_control_already_listening");
    if (this.connecting) throw new AwError("sdk_control_busy");
    clearInterval(this.timer);
    this.timer = undefined;
    this.controller = new Controller(this, automatic);
  }
  async detachControl() {
    if ([...this.tasks].some((task) => !task.closed && !task.fallback))
      throw new AwError("sdk_control_busy");
    this.detaching = true;
    try {
      const connection =
        this.connection ??
        this.retired ??
        (this.pending?.handle ? this.pending : undefined);
      if (connection) {
        if (
          !this.controller?.identity ||
          sameIdentity(connection.identity, this.controller.identity)
        ) {
          await rpc(
            this.options.endpoint,
            connection.identity,
            {
              method: "registration",
              command: { action: "release", handle: connection.handle },
            },
            this.options.timeoutMs,
          );
        }
      }
      this.connection = this.retired = this.pending = undefined;
      this.state = "detached";
      this.lastError = undefined;
    } finally {
      this.detaching = false;
    }
  }
  start() {
    if (this.controller && !this.controller.finished)
      throw new AwError("sdk_control_already_listening");
    if (this.closed) throw new AwError("instance_closed");
    if (this.timer) return;
    const attempt = () => {
      this.connect().catch((error) => {
        this.lastError = error;
        if (!error.unavailable) this.stop();
      });
    };
    attempt();
    this.timer = setInterval(attempt, this.options.reconnectMs);
    this.timer.unref();
  }
  stop() {
    clearInterval(this.timer);
    this.timer = undefined;
    return (
      this.controller?.stop().catch((error) => {
        this.lastError = error;
      }) ?? Promise.resolve()
    );
  }
  lost(connection, error = new AwError("ECONNRESET", { transport: true })) {
    if (this.connection === connection) {
      this.state = "unavailable";
      this.retired = connection;
      this.connection = undefined;
    }
    for (const task of this.tasks)
      if (task.connection === connection) {
        task.connectionError = error;
        task.fallback = this.options.availability === "native_fallback";
      }
  }
  async task({ signal, cwd, environment } = {}) {
    signal?.throwIfAborted();
    if (this.detaching) throw new AwError("sdk_control_busy");
    if (this.closed) throw new AwError("instance_closed");
    if (this.state !== "ready" && this.options.availability === "required")
      throw new AwError("instance_not_ready");
    if (this.lastError && !this.lastError.unavailable) throw this.lastError;
    const task = new Task(this, this.connection, signal, cwd, environment);
    this.tasks.add(task);
    try {
      await task.open();
      return task;
    } catch (error) {
      try {
        await task.close();
      } catch {}
      throw error;
    }
  }
  async close() {
    this.stop();
    this.closed = true;
    await this.controller?.stop();
    if (this.connecting) await this.connecting.catch(() => {});
    await Promise.all([...this.tasks].map((task) => task.close()));
    const connection =
      this.connection ??
      this.retired ??
      (this.pending?.handle ? this.pending : undefined);
    this.connection = undefined;
    this.state = "closed";
    if (connection) {
      try {
        await rpc(
          this.options.endpoint,
          connection.identity,
          {
            method: "registration",
            command: {
              action: "release",
              handle: connection.handle,
            },
          },
          this.options.timeoutMs,
        );
      } catch (error) {
        if (!error.unavailable) throw error;
      }
    }
  }
}

export class Task {
  constructor(instance, connection, signal, cwd, environment) {
    this.instance = instance;
    this.connection = connection;
    this.signal = signal;
    this.fallback = !connection;
    this.closed = false;
    this.routes = { events: {}, hooks: [] };
    this.cwd = cwd ?? instance.cwd;
    this.environment = { ...(environment ?? instance.environment) };
  }
  async call(
    operation,
    signal = this.signal,
    timeoutMs = this.instance.options.timeoutMs,
  ) {
    if (signal !== null) {
      signal =
        this.signal && signal && this.signal !== signal
          ? AbortSignal.any([this.signal, signal])
          : (signal ?? this.signal);
    }
    signal?.throwIfAborted();
    if (this.closed || this.closing) throw new AwError("task_closed");
    if (this.fallback) return null;
    if (this.connectionError) throw this.connectionError;
    try {
      const response = await rpc(
        this.instance.options.endpoint,
        this.connection.identity,
        operation,
        timeoutMs,
        signal,
      );
      signal?.throwIfAborted();
      if (this.connectionError && !this.fallback) throw this.connectionError;
      return this.fallback ? null : response.result;
    } catch (error) {
      if (signal?.aborted) throw signal.reason;
      if (error.unavailable) {
        this.instance.lost(this.connection, error);
        if (this.fallback) return null;
      }
      throw error;
    }
  }
  async open() {
    if (this.fallback) return;
    this.cwd = await realpath(this.cwd);
    const binding = await this.call({
      method: "open_scope",
      parent_id: this.connection.handle.binding.instance_id,
      pid: process.pid,
      cwd: this.cwd,
      environment: this.environment,
    });
    if (!binding) return;
    this.binding = binding;
    this.routes =
      (await this.call({
        method: "scope_policy",
        instance_id: binding.instance_id,
      })) ?? this.routes;
    // Updates can change routes between tasks. Confirm the current snapshot.
    if (!this.fallback) {
      if ((await this.instance.options.install(this.routes)) !== true)
        throw new AwError("ready_not_confirmed");
      await this.call({
        method: "acknowledge_scope",
        instance_id: binding.instance_id,
      });
    }
  }
  async invoke(hook, payload, signal = this.signal) {
    try {
      return await this.invokeStep(hook, payload, signal);
    } catch (error) {
      await this.abortEvent(hook, payload);
      throw error;
    }
  }
  async abortEvent(hook, payload) {
    try {
      await this.closeEvent(hook, payload);
    } catch (error) {
      // Preserve the original failure/cancellation and stop new task admission.
      this.instance.lastError = error;
    }
  }
  /** Drain an interrupted event independently of the caller's aborted signal. */
  async closeEvent(hook, payload) {
    if (this.closing) return this.closing;
    if (!this.binding || this.closed || this.fallback || this.connectionError)
      return;
    await this.call(
      {
        method: "sdk_callback",
        instance_id: this.binding.instance_id,
        event: hook.event,
        step_id: hook.step,
        native_input: payload,
        close: true,
      },
      null,
    );
  }
  async invokeStep(hook, payload, signal) {
    signal?.throwIfAborted();
    if (this.closed || this.closing) throw new AwError("task_closed");
    if (this.fallback) return neutral();
    const result = await this.call(
      {
        method: "sdk_callback",
        instance_id: this.binding.instance_id,
        event: hook.event,
        step_id: hook.step,
        native_input: payload,
        close: false,
      },
      signal,
      Math.max(this.instance.options.timeoutMs, (hook.budget_ms ?? 0) + 1000),
    );
    if (!result) return neutral();
    const bytes = (value) =>
      Array.isArray(value) &&
      value.length <= 1024 * 1024 &&
      value.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255);
    if (
      ![0, 2].includes(result.code) ||
      !bytes(result.stdout) ||
      !bytes(result.stderr)
    )
      throw new AwError("invalid_callback_response");
    return {
      code: result.code,
      stdout: Buffer.from(result.stdout),
      stderr: Buffer.from(result.stderr),
    };
  }
  /** Emit from an installed semantic boundary and adopt block before native work. */
  async emit(event, payload, signal = this.signal) {
    signal?.throwIfAborted();
    if (this.closed || this.closing) throw new AwError("task_closed");
    if (this.instance.options.capabilities.adapter !== "sdk")
      throw new AwError("semantic_profile_required");
    if (!Object.hasOwn(this.instance.options.capabilities.events, event))
      throw new AwError("semantic_event_unavailable");
    const native = {
      ...structuredClone(payload),
      api_version: "aw-hook-source/v1",
      event,
    };
    const hooks = this.routes.hooks.filter((hook) => hook.event === event);
    for (const hook of hooks) {
      const response = await this.invoke(hook, native, signal);
      if (response.code === 2)
        return { decision: "block", coverage: "checked" };
      if (this.fallback) break;
    }
    return {
      decision: "continue",
      coverage: this.fallback
        ? "unavailable"
        : hooks.length
          ? "checked"
          : "not_configured",
    };
  }
  async close() {
    if (this.closing) return this.closing;
    if (this.closed) return;
    this.closing = this.release();
    try {
      await this.closing;
    } finally {
      this.closing = undefined;
    }
  }
  async release() {
    if (this.binding && !this.connection.released) {
      try {
        await rpc(
          this.instance.options.endpoint,
          this.connection.identity,
          {
            method: "release_instance",
            instance_id: this.binding.instance_id,
          },
          this.instance.options.timeoutMs,
        );
      } catch (error) {
        if (
          !(this.fallback || this.connectionError) ||
          !(error.unavailable || error.code === "stale_service")
        )
          throw error;
      }
    }
    this.closed = true;
    this.instance.tasks.delete(this);
  }
}
