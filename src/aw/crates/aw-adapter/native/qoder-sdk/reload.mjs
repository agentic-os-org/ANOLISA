/** Hold AW request admission until the native command queue or close handshake drains. */
import { randomUUID } from "node:crypto";
import { lifecycle } from "./callback.mjs";

const bridges = new Map();
const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};

export class Reload {
  constructor(config) {
    this.config = config;
    this.id = randomUUID();
    this.requests = new Map();
    this.callbacks = new Set();
    this.completed = new Set();
    this.background = new Set();
    this.notifications = new Set();
    this.backgroundResult = false;
    this.admissions = new Set();
    this.initialized = deferred();
    this.changed = deferred();
    this.output = [];
    this.nextRead = Promise.resolve();
    this.available = deferred();
    this.closing = false;
    this.finished = false;
  }

  track(callback) {
    const pending = Promise.resolve().then(callback);
    this.callbacks.add(pending);
    return pending.finally(() => this.callbacks.delete(pending));
  }

  decorate(options) {
    if (!options.daemon || options.experimentalCloudAgent || options.goal || options.hostActions)
      throw new Error("Reloadable Qoder SDK requires an explicit daemon and prompt-driven Queries");
    if (Object.values(options.agents ?? {}).some((agent) =>
      agent.background === true || agent.isolation === "worktree"))
      throw new Error("Background or worktree agents are unsupported for reloadable Qoder SDK Queries");
    const hooks = Object.fromEntries(Object.entries(options.hooks).map(([event, groups]) =>
      [event, groups.map((group) => ({ ...group, hooks: group.hooks.map((callback) =>
        (...args) => this.track(() => callback(...args))) }))]));
    // Timers bypass input admission; background work can outlive its turn.
    hooks.PreToolUse.unshift({ matcher: "*", hooks: [async (input) =>
      ["CreateGoal", "ScheduleWakeup", "CronCreate"].includes(input.tool_name) ||
      input.tool_input?.run_in_background === true ||
      (input.tool_name === "Agent" && input.tool_input?.isolation === "worktree")
        ? { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny",
          permissionDecisionReason: "Goal activation, scheduled turns, background tools and worktree agents are unsupported for reloadable Qoder SDK Queries" } }
        : {}] });
    const canUseTool = options.canUseTool && ((...args) => this.track(() => options.canUseTool(...args)));
    return { ...options, hooks, ...(canUseTool ? { canUseTool } : {}) };
  }

  async *input(source) {
    await this.initialized.promise;
    if (this.closing) return;
    const stream = typeof source === "string"
      ? [{ type: "user", message: { role: "user", content: source }, parent_tool_use_id: null }]
      : source;
    for await (const message of stream) {
      if (this.closing) return;
      const uuid = message.uuid ?? randomUUID();
      if (typeof uuid !== "string" || !uuid.trim() || uuid.length > 128 || this.requests.has(uuid))
        throw new Error("Reloadable Qoder SDK requires unique bounded message UUIDs");
      const key = `${this.id}:message:${uuid}`;
      const admitted = lifecycle(this.config, "request_start", key);
      this.admissions.add(admitted);
      try {
        await admitted;
        this.requests.set(uuid, key);
      } finally { this.admissions.delete(admitted); }
      if (this.closing) return;
      yield { ...message, uuid };
    }
    // SDK endInput can close the transport on the first result. Keep it open
    // until our queued control barrier has confirmed native turn cleanup.
    while (!this.closing && this.requests.size) await this.changed.promise;
  }

  wake() {
    this.changed.resolve();
    this.changed = deferred();
  }

  async attach(query, initialization) {
    this.native = query;
    if (!initialization.capabilities?.includes("msg_lifecycle_v1"))
      throw new Error("Reloadable Qoder SDK requires msg_lifecycle_v1");
    if (!bridges.has(this.config.binding))
      bridges.set(this.config.binding, lifecycle(this.config, "ready"));
    await bridges.get(this.config.binding);
    this.admitted = true;
    this.initialized.resolve();
    this.reader = this.read();
    const methods = {
      next: () => this.next(),
      return: async () => { await this.close(); return { done: true, value: undefined }; },
      throw: async (error) => { await this.close(); throw error; },
      close: () => this.close(),
      streamInput: (stream) => query.streamInput(this.input(stream)),
      [Symbol.asyncIterator]: () => proxy,
      [Symbol.asyncDispose]: () => this.close(),
    };
    // These APIs can schedule work outside the input iterator's admission gate.
    for (const name of ["resumeGoal", "setGoal", "sendHostAction", "backgroundTasks"])
      methods[name] = () => { throw new Error(`${name} is unsupported for reloadable Qoder SDK Queries`); };
    const proxy = new Proxy(query, {
      get(target, name) {
        if (Object.hasOwn(methods, name)) return methods[name];
        const value = Reflect.get(target, name, target);
        return typeof value === "function" ? value.bind(target) : value;
      },
    });
    return proxy;
  }

  async read() {
    try {
      for (;;) {
        const item = await this.native.next();
        if (item.done) {
          await this.close(false);
          break;
        }
        const message = item.value;
        if (message.type === "system" && message.subtype === "background_tasks_changed") {
          this.background = new Set(message.tasks.map((task) => task.task_id));
          for (const id of this.background) this.notifications.add(id);
          if (this.background.size) this.backgroundResult = true;
        }
        if (message.type === "system" && message.subtype === "task_notification")
          this.notifications.delete(message.task_id);
        // Native timeout backgrounding returns before the child exits. Completion
        // then starts a notification turn without an input message UUID.
        if (message.type === "result" && this.backgroundResult &&
            !this.background.size && !this.notifications.size && !this.closing) {
          await this.native.getContextUsage();
          await Promise.allSettled([...this.callbacks]);
          this.backgroundResult = false;
          if (!this.closing)
            for (const uuid of this.completed) await this.release(uuid);
        }
        if (message.type === "command_lifecycle" &&
            ["completed", "cancelled", "discarded"].includes(message.state) &&
            this.requests.has(message.command_uuid) && !this.closing) {
          // Worker 1.1.78 can emit result/command completion before generator
          // finally blocks finish. getContextUsage is a read-only control request
          // queued behind the active turn in this SDK's native command loop.
          await this.native.getContextUsage();
          await Promise.allSettled([...this.callbacks]);
          this.completed.add(message.command_uuid);
          if (!this.closing && !this.backgroundResult) await this.release(message.command_uuid);
        }
        if (!this.closing) {
          this.output.push(item);
          this.available.resolve();
        }
      }
    } catch (error) {
      this.error = error;
      try { await this.close(false); } catch (cleanup) { this.error = new AggregateError([error, cleanup]); }
    } finally {
      this.finished = true;
      this.available.resolve();
    }
  }

  async release(uuid) {
    const key = this.requests.get(uuid);
    if (key === undefined) return;
    await lifecycle(this.config, "request_end", key);
    this.requests.delete(uuid);
    this.completed.delete(uuid);
    this.wake();
  }

  next() {
    // Only one consumer may replace the shared output notification at a time.
    const result = this.nextRead.then(() => this.readNext());
    this.nextRead = result.then(() => {}, () => {});
    return result;
  }

  async readNext() {
    while (!this.output.length && !this.finished && !this.closing) {
      await this.available.promise;
      this.available = deferred();
    }
    if (this.output.length) return this.output.shift();
    if (this.closing) await this.closePromise;
    if (this.error) throw this.error;
    return { done: true, value: undefined };
  }

  close(discard = true) {
    if (discard) this.output.length = 0;
    if (!this.closePromise) {
      this.closing = true;
      this.initialized.resolve();
      this.wake();
      this.closePromise = this.finish();
      // SDK may observe a concurrent aborted read; callers still receive failure.
      this.closePromise.catch(() => {});
    }
    return this.closePromise;
  }

  async finish() {
    await Promise.allSettled([...this.admissions]);
    // Keep an idle session busy during its native close handshake as well.
    const closing = `${this.id}:close`;
    if (this.native) {
      await lifecycle(this.config, "request_start", closing);
      // Native close detaches background Bash tasks; explicitly stop and await
      // them while the Query transport and its request claims are still alive.
      await Promise.all([...this.background].map((id) => this.native.stopTask(id)));
      await this.native.close();
      await Promise.allSettled([...this.callbacks]);
      for (const uuid of [...this.requests.keys()]) await this.release(uuid);
      await lifecycle(this.config, "request_end", closing);
    }
    this.available.resolve();
  }
}
