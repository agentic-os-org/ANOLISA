/** Per-Query ownership fences dispatch and joins original callbacks before scope retirement. */
import { realpathSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { Scope as SharedScope } from "./scope.mjs";
import { Identities } from "./identity.mjs";
import { daemonOwner, credentialOptions } from "./daemon.mjs";
import { admitMcp, delegatesMcp, dispatchMcp } from "./mcp.mjs";
import { randomUUID } from "node:crypto";
import { isDeepStrictEqual } from "node:util";
import { ExecutionOwner, checkExecutionCleanup } from "./private.mjs";
import { postTools } from "./post.mjs";
import { NativeCommands, supportsWrapperMode } from "./command-wrapper.mjs";
import { prepareNativeOptions, sdkEntry, verifyNativeSettings } from "./native-options.mjs";

const deny = (reason) => ({ hookSpecificOutput: { hookEventName: "PreToolUse",
  permissionDecision: "deny", permissionDecisionReason: reason } });
const stopped = () => ({ continue: false, stopReason: "AW required Hook failed" });

/** Launch and task snapshots share the same native Hook contract. */
export function validateHooks(config) {
  if (!Array.isArray(config.hooks)) throw new Error("Missing AW hooks");
  for (const hook of config.hooks)
    if (!hook || typeof hook !== "object" || !["tool.before", "tool.after"].includes(hook.event) ||
        typeof hook.step !== "string" || !hook.step ||
        !(["block", "report"].includes(hook.on_error) ||
          (config.resident && hook.event === "tool.after" && hook.on_error === "withhold_result")) ||
        hook.replace_result !== false ||
        !Number.isInteger(hook.budget_ms) || hook.budget_ms < 1 || hook.budget_ms > 55000)
      throw new Error("Invalid AW Qoder SDK hook route");
}

class Scope extends SharedScope {
  checkExecution(execution = this.execution) {
    checkExecutionCleanup();
    super.checkExecution(execution);
  }
}

export async function bounded(promise, milliseconds = 12000) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("AW original callback cleanup deadline exceeded")), milliseconds);
    })]);
  } finally { clearTimeout(timer); }
}

export class Resident {
  constructor(config) {
    this.config = config;
    this.id = randomUUID();
    this.scope = new Scope(config, "qoder-sdk");
    this.identities = new Identities(this.scope, error => this.fail(error));
    this.identities.beforeRetire = entry => this.nativeCommands?.complete(entry);
    this.callbacks = new Set();
    this.work = new Set();
    this.closing = false;
    this.failure = null;
    this.background = new Set();
    this.cleanupAbort = new AbortController();
    this.strictPost = config.hooks.some(hook => hook.event === "tool.after" &&
      hook.on_error === "withhold_result");
  }

  track(callback, entry) {
    const original = Promise.resolve().then(callback);
    this.callbacks.add(original);
    entry?.pending.add(original);
    const result = original.finally(() => {
      this.callbacks.delete(original);
      entry?.pending.delete(original);
    });
    result.catch(() => {});
    return result;
  }

  fail(error) {
    this.failure ??= error;
    this.scope.failed = true;
    this.identities.cancel(this.failure);
    // Interrupt uses the native control plane; do not await it from its own Hook.
    if (this.query && !this.closing && !this.interruption) {
      this.interruption = this.query.interrupt();
      this.interruption.catch(() => {});
    }
  }

  async withhold(error) {
    this.fail(error);
    // Closing only the native transport avoids joining this callback from itself.
    // The full resident close subsequently joins original callbacks and finalizers.
    try { await this.stopNative(); }
    catch (cleanup) { throw new AggregateError([error, cleanup], "AW Post withholding cleanup failed"); }
    throw error;
  }

  abortCleanup() {
    this.cleanupDeadline ??= Date.now() + 60000;
    this.cleanupAbort.abort();
    this.nativeCommands?.stopConsumers();
  }

  async quiesceConsumers() {
    const settling = this.nativeCommands?.quiesce();
    let abort;
    const cancelled = new Promise(resolve => {
      abort = resolve;
      if (this.cleanupAbort.signal.aborted) resolve();
      else this.cleanupAbort.signal.addEventListener("abort", abort, { once: true });
    });
    try {
      await Promise.race([settling, cancelled]);
      // Normal completion owns no task-length timeout; explicit cancellation
      // upgrades even an existing graceful close to the bounded cleanup budget.
      if (this.cleanupAbort.signal.aborted)
        await bounded(settling, this.cleanupBudget());
    } finally { this.cleanupAbort.signal.removeEventListener("abort", abort); }
  }

  stopNative({ graceful = false } = {}) {
    if (!graceful || this.failure || this.nativeCommands?.hasUnfinishedConsumer())
      this.abortCleanup();
    if (!this.nativeStopped) {
      this.nativeStopped = (async () => {
        try { await this.quiesceConsumers(); }
        catch (error) { this.abortCleanup(); throw error; }
        this.cleanupDeadline ??= Date.now() + 60000;
        const failures = [];
        if (this.query && !this.nativeClose)
          throw new Error("Pinned Qoder Query lacks native close; shutdown cannot be confirmed");
        if (this.nativeClose) {
          const tasks = [...this.background].filter(id => !this.nativeCommands?.acknowledgedTask(id));
          try { await bounded(Promise.all(tasks.map(id => this.query.stopTask(id))),
            Math.min(12000, this.cleanupBudget())); }
          catch (error) { failures.push(error); }
          this.nativeShutdownReady = true;
          if (this.nativeCancellation) this.nativeAbort.abort();
          // The official Worker can extend its own close grace for background work.
          // Consumer join has completed; native shutdown uses the shared cleanup budget.
          try { await bounded(this.nativeClose(), this.cleanupBudget()); }
          catch (error) { failures.push(error); }
        }
        if (failures.length) throw new AggregateError(failures, "AW native shutdown incomplete");
      })();
      this.nativeStopped.catch(() => {});
    }
    return this.nativeStopped;
  }

  cleanupBudget() {
    return Math.max(1, this.cleanupDeadline - Date.now());
  }

  decorate(options) {
    if (options.experimentalCloudAgent)
      throw new Error("AW resident Queries require the pinned local Worker transport");
    const hooks = {};
    for (const [event, groups] of Object.entries(options.hooks ?? {})) {
      hooks[event] = groups.map((group, groupIndex) => ({ ...group,
        hooks: group.hooks.map((callback, callbackIndex) => {
          const key = `${event}:${groupIndex}:${callbackIndex}`;
          return (input, id, context) => this.track(async () => {
            const tool = ["PreToolUse", "PostToolUse", "PostToolUseFailure"].includes(event);
            const entry = tool ? await this.identities.claim(input, id, key, context.signal) : undefined;
            // The native envelope was verified above. Application observers retain
            // native IDs while the Post tool_input retains the logical command.
            const payload = entry?.nativeInput && event !== "PreToolUse" ?
              { ...structuredClone(input), tool_input: structuredClone(entry.input) } : input;
            const result = await callback(payload, id, context);
            checkExecutionCleanup();
            if (entry && event === "PreToolUse") {
              this.identities.check(entry, true);
              if (result?.hookSpecificOutput?.permissionDecision === "deny") {
                entry.phase = "denied";
                await this.nativeCommands?.denied(entry);
              }
              else if (entry.phase !== "denied") entry.phase = "allowed";
            }
            return result;
          }, this.identities.dispatch.getStore()?.entry).catch(error => {
            if (this.strictPost && ["PostToolUse", "PostToolUseFailure"].includes(event))
              return this.withhold(error);
            this.fail(error);
            return event === "PreToolUse" ? deny("AW native identity or callback failed") : stopped();
          });
        }) }));
    }
    const canUseTool = options.canUseTool && ((...args) => this.track(async () => {
      const ticket = this.identities.dispatch.getStore();
      if (ticket?.kind !== "can_use_tool" || !ticket.entry || args[0] !== ticket.entry.name ||
          args[2].toolUseID !== ticket.entry.native)
        return { behavior: "deny", message: "AW rejected native permission identity" };
      if (ticket.entry.callbacks.has("can_use_tool"))
        throw new Error("AW rejected repeated native permission callback");
      ticket.entry.callbacks.add("can_use_tool");
      await this.identities.ready(ticket.entry, args[2].signal, true);
      const nativeInput = ticket.entry.nativeInput ?? ticket.entry.input;
      if (ticket.entry.phase === "denied" || !isDeepStrictEqual(args[1], nativeInput)) {
        ticket.entry.phase = "denied";
        return { behavior: "deny", message: "AW rejected native permission identity" };
      }
      // The Worker authorizes the logical input before this callback. Publishing
      // a wrapper in PreTool would hide command-specific native deny rules.
      const result = await options.canUseTool(args[0], structuredClone(ticket.entry.input), args[2]);
      checkExecutionCleanup();
      this.identities.check(ticket.entry, true);
      if (result.behavior === "deny") {
        ticket.entry.phase = "denied";
        await this.nativeCommands?.denied(ticket.entry);
        return result;
      }
      if (this.nativeCommands && ticket.entry.name === "Bash" && result.behavior === "allow")
        return { ...result, updatedInput: await this.nativeCommands.approved(ticket.entry, args[2].signal) };
      return ticket.entry.nativeInput && result.behavior === "allow" ?
        { ...result, updatedInput: structuredClone(nativeInput) } : result;
    }, this.identities.dispatch.getStore()?.entry).catch(error => {
      this.fail(error);
      return { behavior: "deny", message: "AW native permission identity failed" };
    }));
    const decorated = { ...options, hooks, ...(canUseTool ? { canUseTool } : {}) };
    for (const path of ["resolveModel", "onAuthExpired", "resolveSessionArtifacts", "onElicitation",
      "auth.fetchJobToken", "auth.fetchServiceAccountToken",
      "memory.generation.turnComplete.shouldGenerate", "memory.generation.onResult",
      "memory.consumption.onResult", "evolution.skill.generation.turnComplete.shouldReview",
      "evolution.skill.generation.onResult"]) {
      const fields = path.split(".");
      const name = fields.pop();
      let source = options;
      for (const field of fields) {
        source = source?.[field];
        if (Array.isArray(source)) { source = undefined; break; }
      }
      const callback = source?.[name];
      if (typeof callback !== "function") continue;
      // Leave invalid native option shapes intact for the SDK's own validation.
      let target = decorated;
      for (const field of fields) {
        target[field] = { ...target[field] };
        target = target[field];
      }
      target[name] = (...args) => this.track(() => callback.apply(source, args));
    }
    return decorated;
  }

  install(query) {
    // Admission can fail after the SDK has started its transport. Retain close
    // before checking any other seam so rejection still joins native shutdown.
    this.query = query;
    if (typeof query.close === "function") this.nativeClose = query.close.bind(query);
    for (const method of ["close", "recordInboundSessionMessage", "handleControlRequest", "interrupt", "getContextUsage"])
      if (typeof query[method] !== "function")
        throw new Error(`Pinned Qoder Query is missing native seam ${method}`);
    if (typeof query.transport?.write !== "function")
      throw new Error("Pinned Qoder Query lacks native control response transport");
    if (this.nativeCommands) {
      if (typeof query.transport.endInput !== "function")
        throw new Error("Pinned Qoder Query lacks native input EOF seam");
      const endInput = query.transport.endInput.bind(query.transport);
      query.transport.endInput = () => {
        if (this.nativeShutdownReady) return endInput();
        if (!this.inputEnding) {
          // Finite prompts end native stdin after the SDK's result barrier. Hold
          // that EOF too: timeout-backgrounded consumers still need their watch.
          const pending = Promise.resolve().then(async () => {
            await this.quiesceConsumers();
            if (!this.closing) endInput();
          });
          this.inputEnding = pending;
          this.work.add(pending);
          pending.catch(error => this.fail(error)).finally(() => this.work.delete(pending));
        }
      };
    }
    const record = query.recordInboundSessionMessage.bind(query);
    query.recordInboundSessionMessage = message => {
      try {
        this.identities.produce(message);
        if (message.type === "user")
          for (const block of message.message?.content ?? [])
            if (block.type === "tool_result") this.identities.terminal(block.tool_use_id);
        if (message.type === "system" && message.subtype === "background_tasks_changed") {
          if (this.nativeCommands && this.identities.session &&
              message.session_id !== this.identities.session)
            throw new Error("AW native background task session identity changed");
          this.background = new Set(message.tasks.map(task => task.task_id));
        }
        this.nativeCommands?.observeTask(message);
      } catch (error) {
        this.fail(error);
        throw this.failure;
      }
      if (message.type === "user") {
        for (const block of message.message?.content ?? []) {
          if (block.type !== "tool_result") continue;
          const pending = (async () => {
            // Result publication precedes the native generator's finally. This
            // queued control request acknowledges that the turn has unwound.
            try { await query.getContextUsage(); }
            catch (error) { if (!this.closing) throw error; }
            // Concurrent close owns the remaining ledger after native shutdown.
            if (!this.closing) await this.identities.retire(block.tool_use_id);
          })();
          this.work.add(pending);
          pending.catch(error => this.fail(error)).finally(() => this.work.delete(pending));
        }
      }
      return record(message);
    };
    const dispatch = query.handleControlRequest.bind(query);
    query.handleControlRequest = frame => {
      let ticket;
      try { ticket = this.identities.ticket(frame); }
      catch (error) {
        this.fail(error);
        const request = frame?.request;
        const kind = request?.subtype ?? request?.type;
        if (this.strictPost && kind === "hook_callback" &&
            ["PostToolUse", "PostToolUseFailure"].includes(request.input?.hook_event_name))
          return this.track(() => this.withhold(error)).catch(() => {});
        const response = kind === "hook_callback" ?
          (request.input?.hook_event_name === "PreToolUse" ? deny("AW native identity rejected") : stopped()) :
          kind === "can_use_tool" ? { behavior: "deny", message: "AW native identity rejected" } : undefined;
        const result = response ? { subtype: "success", request_id: frame?.request_id, response } :
          { subtype: "error", request_id: frame?.request_id, error: "AW native identity rejected" };
        query.transport.write(JSON.stringify({ type: "control_response", response: result }) + "\n");
        return Promise.resolve();
      }
      return this.identities.dispatch.run(ticket, () => this.track(() => dispatch(frame), ticket.entry))
        .finally(() => this.identities.pending.delete(ticket.id));
    };
    if (this.mcp.size) {
      if (typeof query.processControlRequest !== "function")
        throw new Error("Pinned Qoder Query lacks native MCP dispatch seam");
      const process = query.processControlRequest.bind(query);
      query.processControlRequest = (frame, signal) =>
        this.track(() => dispatchMcp(this, frame, signal, process));
    }
    query.close = () => this.close();
    query.return = async () => { await this.close(); return { done: true }; };
    const next = query.next.bind(query);
    query.next = async () => {
      try {
        checkExecutionCleanup();
        if (this.failure) throw this.failure;
        let item = await next();
        while (!item.done && item.value.type === "stream_event" && !this.partialMessages)
          item = await next();
        checkExecutionCleanup();
        if (this.failure) throw this.failure;
        if (item.done) await this.close({ graceful: true });
        return item;
      } catch (error) {
        // A late frame can hit the closed scope after the original identity or
        // callback failure. Keep that first rejection actionable to the caller.
        const primary = this.failure ?? error;
        try { await this.close(); }
        catch (cleanup) { throw new AggregateError([primary, cleanup], "AW native read and cleanup failed"); }
        throw primary;
      }
    };
    const streamInput = query.streamInput.bind(query);
    query.streamInput = source => {
      this.scope.checkExecution();
      return streamInput(source);
    };
    if (this.nativeCommands && typeof query.setPermissionMode === "function") {
      const change = query.setPermissionMode.bind(query);
      query.setPermissionMode = mode => this.nativeCommands.setPermissionMode(mode, change);
    }
    if (typeof query.sendHostAction === "function") {
      const send = query.sendHostAction.bind(query);
      query.sendHostAction = async (...args) => {
        this.scope.checkExecution();
        // The SDK fallback owns an auth payload and helper process outside its
        // main transport; native close alone does not join that cleanup.
        return this.track(() => send(...args));
      };
    }
    for (const name of ["setMcpServers"])
      query[name] = () => { throw new Error(`${name} bypasses AW resident admission`); };
    if (query.readLoopPromise) {
      // Transport loss must reclaim the scope even when the application stops reading.
      query.readLoopPromise.then(() => this.close(), error => {
        this.failure ??= error;
        return this.close();
      }).catch(error => { this.failure ??= error; });
    }
  }

  facade() {
    const facade = Object.create(null);
    // Keep SDK parser, transport and callback maps inaccessible to application code.
    for (const name of ["next", "return", "close", "streamInput", "initializationResult",
      "interrupt", "cancelAsyncMessage", "stopTask", "backgroundTasks", "setGoal",
      "setGoalMaxTurns", "resumeGoal", "getGoal", "clearGoal", "setPlanMode", "getPlanMode",
      "setPermissionMode", "setModel", "setProxy", "generateSessionTitle", "askSideQuestion",
      "addDirectories", "supportedCommands", "getAvailableModels", "listByokProviders",
      "validateByokModel", "listByokConfigs", "createByokConfig", "updateByokConfig",
      "deleteByokConfig", "checkByokConfig", "supportedAgents", "mcpServerStatus",
      "getContextUsage", "getUsageInfo", "reloadSkills", "listPlugins", "accountInfo",
      "rewindFiles", "rewind", "seedReadState", "flushMemory", "flushSkillEvolution",
      "refreshMemory", "mcpAuthenticate", "mcpSubmitOAuthCallbackUrl", "toggleMcpServer",
      "reconnectMcpServer", "mcpClearAuth", "injectMcpToken", "sendHostAction"])
      if (typeof this.query[name] === "function") facade[name] = this.query[name].bind(this.query);
    facade.throw = async error => {
      try { await this.close(); }
      catch (cleanup) { throw new AggregateError([error, cleanup], "AW Query cleanup failed"); }
      throw error;
    };
    facade[Symbol.asyncIterator] = () => facade;
    facade[Symbol.asyncDispose] = () => this.close();
    return Object.freeze(facade);
  }

  async preparePolicy(cwd, environment) {
    if (!this.config.resident_policy_reload) return;
    // The root claim also covers native import and finalizers after child close.
    await this.scope.reserve();
    await this.scope.open(cwd, environment, { deferAcknowledgement: true });
    const policy = await this.scope.policy();
    if (!policy || typeof policy !== "object" || Array.isArray(policy) ||
        typeof policy.policy_revision !== "string" || !/^[a-f0-9]{64}$/.test(policy.policy_revision))
      throw new Error("AW resident policy handoff is malformed");
    validateHooks({ ...this.config, hooks: policy.hooks });
    const hooks = structuredClone(policy.hooks);
    for (const hook of hooks) Object.freeze(hook);
    // Task snapshots cannot replace launch identity or executable fields.
    this.config = { ...this.config, policy_revision: policy.policy_revision,
      hooks: Object.freeze(hooks) };
    this.strictPost = this.config.hooks.some(hook => hook.event === "tool.after" &&
      hook.on_error === "withhold_result");
  }

  async create(prompt, options, compose, ready) {
    checkExecutionCleanup();
    const startupConfig = this.config;
    const { awExecutionFactory, awNativeExecutionFactory, awSite, ...nativeOptions } = options;
    options = nativeOptions;
    if (awNativeExecutionFactory !== undefined) {
      if (typeof options.canUseTool !== "function")
        throw new Error("AW native command execution requires an explicit canUseTool approver");
      if (!supportsWrapperMode(options.permissionMode))
        throw new Error("AW native command execution requires default or acceptEdits permission mode");
      this.nativeCommands = new NativeCommands(awNativeExecutionFactory, this);
    }
    if (awExecutionFactory !== undefined) {
      this.executionOwner = new ExecutionOwner(awExecutionFactory, {
        onFailure: error => this.fail(error),
      });
      this.executionOwner.check();
    }
    this.partialMessages = options.includePartialMessages === true;
    this.owner = daemonOwner(options.daemon, this.config);
    const site = awSite ?? this.owner?.site ?? this.config.default_site ?? "global";
    const sdk = sdkEntry(this.config, site);
    if (this.owner && this.owner.sdk !== sdk)
      throw new Error("AW Query site differs from its managed daemon");
    if (sdk !== this.config.sdk) this.config = { ...this.config, sdk, active_site: site };
    if (this.owner && (options.env !== undefined || options.auth !== undefined))
      throw new Error("Qoder daemon owns its credential environment; Query overrides are unsupported");
    const environment = { ...(this.owner?.environment ?? process.env), ...(options.env ?? {}) };
    // Partial output supplies root producers and child identity announcements;
    // child callbacks await the complete assistant on this private channel.
    options = { ...options, includePartialMessages: true,
      ...(this.owner ? {} : { env: { ...environment } }),
      ...(options.auth ? { auth: credentialOptions(options.auth) } : {}) };
    const cwd = realpathSync(options.cwd ?? process.cwd());
    this.cwd = cwd;
    this.mcp = admitMcp(options);
    this.ownerClose = () => this.close();
    try {
      await this.preparePolicy(cwd, environment);
      const scoped = { ...this.config, cwd, scope: this.scope };
      this.nativeAssets = await prepareNativeOptions(scoped, { ...options, cwd });
      const composed = compose(scoped, this.nativeAssets.options);
      for (const event of ["PreToolUse", "PostToolUse", "PostToolUseFailure"])
        if (!composed.hooks[event]?.length)
          composed.hooks[event] = [{ matcher: "*", hooks: [async () => ({})] }];
      // Each policy callback receives the AW UUID while application Hooks retain native IDs.
      for (const [event, groups] of Object.entries(composed.hooks)) {
        if (!["PreToolUse", "PostToolUse", "PostToolUseFailure"].includes(event)) continue;
        const originalCount = options.hooks?.[event]?.length ?? 0;
        for (const group of groups.slice(originalCount)) {
          group.hooks = group.hooks.map(callback => async (input, id, context) => {
            const entry = this.identities.dispatch.getStore()?.entry;
            if (!entry) throw new Error("AW policy callback lacks native dispatch identity");
            if (delegatesMcp(this, this.identities.dispatch.getStore().request.input)) return {};
            if (event !== "PreToolUse") {
              await postTools(this, this.identities.payload(entry, input), context.signal);
              return {};
            }
            return await callback(this.identities.payload(entry, input), entry.id, context);
          });
        }
      }
      if (this.nativeCommands) {
        const groups = composed.hooks.PreToolUse;
        const applicationCount = options.hooks?.PreToolUse?.length ?? 0;
        const policy = groups.slice(applicationCount);
        if (policy.some(group => group.matcher != null && group.matcher !== "*"))
          throw new Error("AW command execution requires unfiltered policy dispatch");
        // Worker hook groups execute concurrently. Keep policy and wrapper
        // acquisition in one AW callback, preserving the native application groups.
        composed.hooks.PreToolUse = [...groups.slice(0, applicationCount), {
          matcher: "*", timeout: policy.reduce((total, group) => total + (group.timeout ?? 0), 12),
          hooks: [async (input, id, context) => {
            const entry = this.identities.dispatch.getStore()?.entry;
            if (!entry) throw new Error("AW command wrapper lacks native dispatch identity");
            for (const group of policy)
              for (const callback of group.hooks) {
                const result = await callback(input, id, context);
                if (result?.hookSpecificOutput?.permissionDecision === "deny") return result;
              }
            return await this.nativeCommands.prepare(entry, context.signal, input.permission_mode);
          }] }];
      }
      const decorated = this.decorate(composed);
      if (this.config.resident_policy_reload) await this.scope.acknowledge();
      const sdk = await import(pathToFileURL(this.config.sdk).href);
      if (!this.config.resident_policy_reload) await this.scope.open(cwd, environment);
      // No await may separate this fence from native creation and owner registration.
      checkExecutionCleanup();
      if (this.owner?.closing) throw new Error("AW daemon stopped during Query admission");
      this.scope.begin();
      const initialized = new Promise(resolve => { this.releaseInput = resolve; });
      const source = typeof prompt === "string" ? [{ type: "user",
        message: { role: "user", content: prompt }, parent_tool_use_id: null }] : prompt;
      const resident = this;
      const input = async function* () {
        await initialized;
        if (!resident.closing) yield* source;
      };
      if (this.nativeCommands && options.abortController) {
        // The native Worker also listens to this signal and closes immediately.
        // Keep its transport alive until consumer stop/join and native task ACKs.
        const signal = options.abortController.signal;
        if (signal.aborted) throw new Error("AW native Query cancelled before creation");
        this.nativeAbort = new AbortController();
        decorated.abortController = this.nativeAbort;
        const abort = () => {
          this.nativeCancellation = true;
          this.close().catch(error => { this.failure ??= error; });
        };
        signal.addEventListener("abort", abort, { once: true });
        this.detachAbort = () => signal.removeEventListener("abort", abort);
      }
      const query = sdk.query({ prompt: input(), options: decorated });
      this.install(query);
      this.owner?.queries.add(this.ownerClose);
      const initialization = await query.initializationResult();
      await verifyNativeSettings(this.config, query, bounded);
      checkExecutionCleanup();
      if (this.failure) throw this.failure;
      if (this.closing || this.owner?.closing)
        throw new Error("AW resident Query closed during initialization");
      if (!initialization?.capabilities?.includes("msg_lifecycle_v1"))
        throw new Error("AW resident Query requires msg_lifecycle_v1");
      // Launcher readiness attests the launch; task routes may already be newer.
      ready(startupConfig);
      this.releaseInput();
      return this.facade();
    } catch (error) {
      const primary = this.failure ??= error;
      try { await this.close(); }
      catch (cleanup) { throw new AggregateError([primary, cleanup], "AW native initialization and cleanup failed"); }
      throw primary;
    }
  }

  close({ graceful = false } = {}) {
    if (!graceful || this.failure) this.abortCleanup();
    if (!this.closed) {
      this.closing = true;
      this.releaseInput?.();
      this.scope.failed = true;
      this.identities.cancel(this.failure ?? new Error("AW native Query closed before child producer"));
      this.closed = this.finish(graceful);
      this.closed.catch(() => {});
    }
    return this.closed;
  }

  async finish(graceful) {
    // Join credential consumers before native close aborts its original waiters.
    const failures = [];
    let nativeStopped = false;
    try { await this.stopNative({ graceful }); nativeStopped = true; }
    catch (error) { failures.push(error); }
    this.detachAbort?.();
    try { await this.executionOwner?.close(this.cleanupBudget()); }
    catch (error) { failures.push(error); }
    let callbacksDrained = false;
    try {
      await bounded(Promise.allSettled([...this.callbacks, ...this.work]),
        Math.min(12000, this.cleanupBudget()));
      callbacksDrained = true;
    }
    catch (error) { failures.push(error); }
    try { await this.nativeCommands?.close(nativeStopped && callbacksDrained); }
    catch (error) { failures.push(error); }
    // Unconfirmed consumers or callbacks may still read the assets. Keep both
    // snapshots and scope ownership until the worker is stopped.
    if (failures.length)
      throw new AggregateError(failures, "AW native cleanup incomplete; scope retained until worker exit");
    await this.nativeAssets?.close();
    await this.scope.drain();
    this.identities.active.clear();
    await this.scope.close();
    if (this.config.resident_policy_reload) await this.scope.release();
    this.owner?.queries.delete(this.ownerClose);
  }
}
