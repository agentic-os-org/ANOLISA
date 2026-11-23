/** Hold native command credentials until the verified Worker terminal barrier. */
import { isAbsolute } from "node:path";
import { ExecutionOwner, PRIVATE_EXECUTION_VERSION } from "./private.mjs";

export const NATIVE_COMMAND_WRAPPER_VERSION = "aw.native-command-wrapper/v1";

const deferred = () => {
  let resolve, reject;
  const promise = new Promise((done, fail) => { resolve = done; reject = fail; });
  return { promise, resolve, reject };
};
const failure = () => new Error("AW native command execution failed; worker restart required");
export const supportsWrapperMode = mode => mode === undefined || ["default", "acceptEdits"].includes(mode);
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;

/** An explicit wrapper replaces the native command without changing its logical input. */
export class NativeCommands {
  constructor(factory, resident) {
    if (typeof factory !== "function" || !Array.isArray(resident.config.native_command_wrappers) ||
        !resident.config.native_command_wrappers.length ||
        resident.config.native_command_wrappers.some(path => typeof path !== "string" || !isAbsolute(path)))
      throw new Error("AW native command execution requires admitted command wrappers");
    this.resident = resident;
    this.permissionChanges = Promise.resolve();
    this.records = new Map();
    this.backgroundConsumers = new Map();
    this.owner = new ExecutionOwner(async (context, options) => {
      const lease = await factory(context, options);
      // Validation belongs inside ExecutionOwner.run so even an invalid wrapper
      // returns its original lease to the owner's unconditional release path.
      const release = lease?.release;
      const settle = lease?.settle;
      const record = this.records.get(context.invocationId);
      record.lease = {
        version: lease?.version, keys: lease && Object.keys(lease),
        commandWrapper: lease?.commandWrapper && { ...lease.commandWrapper },
        settle: typeof settle === "function" ? options => settle.call(lease, options) : undefined,
        releaseSupported: typeof release === "function",
      };
      // Native wrappers own their ticket transport and output redaction. This
      // adapter reuses the lifecycle owner without claiming private env delivery.
      return { version: PRIVATE_EXECUTION_VERSION, environment: {}, hookEnvironment: {},
        secrets: {}, redact: value => value, release: async () => {
          // An unknown consumer outcome retains the ticket and trips the shared
          // cleanup fence. Revocation must never stand in for a process join.
          if (record.published && !record.unused && !record.consumerSettled) throw failure();
          if (typeof release !== "function") throw failure();
          await release.call(lease);
        } };
    }, { onFailure: error => resident.fail(error) });
  }

  async prepare(entry, signal, permissionMode) {
    this.resident.identities.check(entry, true);
    this.owner.check();
    if (entry.phase === "denied") return { hookSpecificOutput: {
      hookEventName: "PreToolUse", permissionDecision: "deny" } };
    if (entry.name !== "Bash") return {};
    // Child agents and native settings can select a different mode from the Query.
    // The pinned Worker skips Hook asks in bypass/auto, so it must not execute here.
    if (!["default", "acceptEdits"].includes(permissionMode)) {
      entry.phase = "denied";
      return { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny",
        permissionDecisionReason: "AW command wrapper requires default or acceptEdits native permission mode" } };
    }
    if (typeof entry.input.command !== "string" || entry.input.run_in_background === true ||
        this.records.has(entry.id)) throw failure();
    const record = { entry, prepared: deferred(), terminal: deferred(),
      consumer: new AbortController(), execution: new AbortController() };
    record.detach = () => signal?.removeEventListener("abort", abort);
    const abort = () => { record.execution.abort(); record.consumer.abort(); };
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    this.records.set(entry.id, record);
    const context = { invocationId: entry.id, nativeToolUseId: entry.native,
      sessionId: this.resident.identities.session, policyRevision: this.resident.config.policy_revision,
      toolName: entry.name, input: entry.input };
    record.pending = this.owner.run(context, async approved => {
      const wrapper = record.lease?.commandWrapper;
      if (record.lease.version !== NATIVE_COMMAND_WRAPPER_VERSION ||
          record.lease.keys.some(key => !["version", "commandWrapper", "settle", "release"].includes(key)) ||
          typeof record.lease.settle !== "function" ||
          !record.lease.releaseSupported ||
          !wrapper || Object.getPrototypeOf(wrapper) !== Object.prototype ||
          Object.keys(wrapper).some(key => !["executable", "ticket"].includes(key)) ||
          !this.resident.config.native_command_wrappers.includes(wrapper.executable) ||
          typeof wrapper.ticket !== "string" || !/^[A-Za-z0-9_-]{16,256}$/.test(wrapper.ticket))
        throw failure();
      this.resident.identities.check(entry, true);
      await this.awaitPermissions(signal);
      this.resident.identities.check(entry, true);
      if (entry.phase === "denied" || record.unused) {
        record.prepared.resolve({ hookSpecificOutput: {
          hookEventName: "PreToolUse", permissionDecision: "deny" } });
        return {};
      }
      const native = { ...structuredClone(approved.input), command: [wrapper.executable,
        wrapper.ticket, Buffer.from(approved.input.command, "utf8").toString("base64url")]
        .map(quote).join(" ") };
      record.nativeInput = native;
      // Keep the logical command through the Worker's native rule evaluation.
      // Only its subsequent permission callback may publish executable arguments.
      record.prepared.resolve({ hookSpecificOutput: { hookEventName: "PreToolUse",
        permissionDecision: "ask" } });
      await record.terminal.promise;
      if (record.published && !record.unused) {
        // Native Bash may return while its process continues in the background,
        // including timeout promotion with delayed or silent task metadata.
        await this.settleConsumer(record);
      }
      return {};
    }, { signal: record.execution.signal });
    record.pending.finally(record.detach).catch(() => {});
    record.pending.catch(error => record.prepared.reject(error));
    return await record.prepared.promise;
  }

  /** Publish a wrapper only after native rules and the explicit approver allow it. */
  async approved(entry, signal) {
    await this.awaitPermissions(signal);
    this.resident.identities.check(entry, true);
    this.owner.check();
    const record = this.records.get(entry.id);
    if (!record?.nativeInput || record.published || record.unused || entry.phase === "denied")
      throw failure();
    entry.nativeInput = structuredClone(record.nativeInput);
    record.published = true;
    return structuredClone(entry.nativeInput);
  }

  async awaitPermissions(signal) {
    if (signal?.aborted) throw failure();
    let timer;
    const abort = () => rejectWait(failure());
    let rejectWait;
    const cancelled = new Promise((_, reject) => {
      rejectWait = reject;
      timer = setTimeout(abort, 12000);
    });
    signal?.addEventListener("abort", abort, { once: true });
    if (signal?.aborted) abort();
    const deadline = Date.now() + 12000;
    try {
      if (signal?.aborted) throw failure();
      while (Date.now() < deadline) {
        const pending = this.permissionChanges;
        await Promise.race([pending, cancelled]);
        if (signal?.aborted) throw failure();
        // A downgrade can be queued while the previous ACK is still pending.
        if (pending === this.permissionChanges) return;
      }
      throw failure();
    } finally {
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
    }
  }

  /** Adopt permission transitions only after the native control plane acknowledges them. */
  setPermissionMode(mode, change) {
    if (!supportsWrapperMode(mode))
      return Promise.reject(new Error("AW native command execution requires default or acceptEdits permission mode"));
    const changing = this.permissionChanges.then(async () => {
      this.resident.scope.checkExecution();
      try { await change(mode); }
      catch {
        const error = failure();
        this.resident.fail(error);
        throw error;
      }
    });
    this.permissionChanges = changing;
    changing.catch(() => {});
    return changing;
  }

  /** Permission denial proves the opaque ticket was never dispatched. */
  async denied(entry) {
    entry.phase = "denied";
    const record = this.records.get(entry.id);
    if (record) record.unused = true;
    await this.complete(entry);
  }

  /** An unfinished producer requires cancellation rather than natural EOF settlement. */
  hasUnfinishedConsumer() {
    return [...this.records.values()].some(record => record.published &&
      !record.unused && !record.entry.terminal);
  }

  /** Correlate native task identity without inferring ownership from result text. */
  observeTask(message) {
    if (message.type !== "system") return;
    if (["task_started", "task_progress"].includes(message.subtype) &&
        message.session_id === this.resident.identities.session &&
        typeof message.task_id === "string" && message.task_id &&
        typeof message.tool_use_id === "string") {
      const entry = this.resident.identities.active.get(message.tool_use_id);
      const record = entry && this.records.get(entry.id);
      if (record?.published && !record.unused) {
        if (record.taskId && record.taskId !== message.task_id) throw failure();
        const previous = this.backgroundConsumers.get(message.task_id) ??
          [...this.records.values()].find(other => other.taskId === message.task_id);
        if (previous && previous !== record) throw failure();
        if (!this.backgroundConsumers.has(message.task_id) &&
            this.backgroundConsumers.size >= this.resident.scope.invocationLimit) throw failure();
        record.taskId = message.task_id;
        // A typed announcement can precede its first live snapshot and outlast
        // consumer retirement. Retain that unresolved native task, not history.
        this.backgroundConsumers.set(message.task_id, record);
      }
    }
    const announced = this.backgroundConsumers.get(message.task_id);
    if (message.subtype === "task_notification" && announced &&
        message.session_id === this.resident.identities.session &&
        (message.tool_use_id === undefined || message.tool_use_id === announced.entry.native) &&
        ["completed", "failed", "stopped"].includes(message.status) &&
        !this.resident.background.has(message.task_id))
      this.backgroundConsumers.delete(message.task_id);
    if (message.subtype === "background_tasks_changed") {
      const current = new Map();
      for (const id of this.resident.background) {
        const record = this.backgroundConsumers.get(id) ??
          [...this.records.values()].find(record => record.taskId === id);
        if (record) current.set(id, record);
      }
      // Full snapshots replace both announced and previously live task state.
      this.backgroundConsumers = current;
    }
  }

  /** Consumer join supersedes only its own previously correlated native task. */
  acknowledgedTask(id) {
    return this.backgroundConsumers.get(id)?.consumerSettled === true;
  }

  settleConsumer(record) {
    if (!record.settling) {
      record.settling = (async () => {
        await record.lease.settle({ signal: record.consumer.signal });
        record.consumerSettled = true;
        // Native close may abort its old Pre waiter after the consumer has joined.
        // That signal no longer owns an execution or credential consumer.
        record.detach();
      })();
      record.settling.catch(() => {});
    }
    return record.settling;
  }

  /** Join actual consumers before the native transport can close their watchdog. */
  async quiesce() {
    const records = [...this.records.values()].filter(record => record.published && !record.unused);
    if (records.some(record => !record.entry.terminal && !record.consumer.signal.aborted))
      throw failure();
    const settled = await Promise.allSettled(records.map(record => this.settleConsumer(record)));
    if (settled.some(result => result.status === "rejected")) throw failure();
  }

  /** Request stop through the consumer contract; credentials remain held until join. */
  stopConsumers() {
    for (const record of this.records.values()) record.consumer.abort();
  }

  /** Called only after native terminal publication and original callback drain. */
  async complete(entry) {
    const record = this.records.get(entry.id);
    if (!record) return;
    record.terminal.resolve();
    await record.pending;
    this.records.delete(entry.id);
  }

  /** Confirm native shutdown before retiring leases for unfinished commands. */
  async close(nativeStopped) {
    if (nativeStopped)
      for (const record of this.records.values()) {
        record.terminal.resolve();
      }
    if (nativeStopped) {
      let timer;
      try {
        // Completed consumers and original callbacks can finish normally. Calling
        // owner.close first would abort an otherwise successful delivery waiter.
        await Promise.race([
          Promise.allSettled([...this.records.values()].map(record => record.pending)),
          new Promise(resolve => { timer = setTimeout(resolve, this.resident.cleanupBudget()); }),
        ]);
      } finally { clearTimeout(timer); }
    }
    await this.owner.close(Math.min(60000, this.resident.cleanupBudget()));
    this.records.clear();
    this.backgroundConsumers.clear();
  }
}
