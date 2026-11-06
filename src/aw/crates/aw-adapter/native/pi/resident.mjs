/** Resident hosts create/dispose task sessions; tenant context never lives in a global current session. */
import { realpathSync } from "node:fs";
import { configuration, installSessionBridge, publishReady } from "./bridge.mjs";
import { Scope } from "./scope.mjs";
import { randomUUID } from "node:crypto";
import { inputBoundary } from "./submission.mjs";
import { expandsSkill, prepareSkill } from "./skills.mjs";

async function bounded(promise, milliseconds) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("SDK teardown deadline exceeded")), milliseconds);
    })]);
  } finally { clearTimeout(timer); }
}

export async function runWorker(app, sdk, core, args) {
  if (configuration().observation_enabled)
    throw new Error("AW resident Pi does not support AgentSight evidence");
  const active = new Map();
  const closed = new WeakSet();
  let poisoned = false;

  const closeSession = async (session) => {
    const record = active.get(session);
    if (!record) {
      if (closed.has(session)) return;
      throw new Error("Session does not belong to this resident host");
    }
    if (record.closing) return await record.closing;
    // Fence callbacks and model transport before abort can yield to the SDK.
    record.scope.failed = true;
    record.inputAbort?.abort();
    record.closing = (async () => {
      try {
        try {
          await bounded((async () => {
            await record.abort();
            await record.pending?.catch(() => {});
            record.bridge.dispose();
            await record.dispose();
          })(), 12000);
        } finally {
          await record.scope.close();
        }
        active.delete(session);
        closed.add(session);
      } catch (error) {
        poisoned = true;
        throw error;
      }
    })();
    return await record.closing;
  };

  const createSession = async (options = {}) => {
    if (poisoned) throw new Error("AW worker requires restart after failed task cleanup");
    const scope = new Scope();
    const cwd = realpathSync(options.cwd ?? process.cwd());
    let session;
    try {
      await scope.open(cwd, { ...process.env, ...(options.environment ?? {}) });
      const result = await app.createSession({ ...options, cwd, sdk, args });
      session = result?.session;
      if (!(session instanceof sdk.AgentSession) || !(session.agent instanceof core.Agent)
          || realpathSync(session.sessionManager.getCwd()) !== cwd || active.has(session))
        throw new Error("Pi factory must return a fresh SDK session with the admitted cwd");
      if (poisoned) throw new Error("AW worker requires restart after failed task cleanup");
      const nativeStream = session.agent.streamFunction;
      if (typeof nativeStream !== "function") throw new Error("Pi requires its public model stream function");
      const bridge = installSessionBridge(session, scope);
      scope.onFailure = () => session.agent.abort();
      const stream = bridge.model(nativeStream);
      // The SDK may schedule a final turn after abort; gate before any model transport.
      Object.defineProperty(session.agent, "streamFunction", {
        configurable: false, enumerable: true,
        value: (...arguments_) => {
          scope.checkExecution();
          if (boundary) arguments_[1] = boundary.model(arguments_[1], arguments_[2]);
          return stream(...arguments_);
        }, writable: false,
      });
      const inputHooks = configuration().hooks.filter(hook => hook.event === "input.submit");
      const checked = inputHooks.length > 0;
      const skillChecks = inputHooks.some(hook => hook.skill_check === true);
      const boundary = checked ? inputBoundary(session) : null;
      const record = { scope, bridge, closing: null, pending: null, inputAbort: null,
        abort: session.abort.bind(session), dispose: session.dispose.bind(session) };
      if (checked) {
        const agentAbort = session.agent.abort.bind(session.agent);
        Object.defineProperty(session.agent, "abort", { value: () => {
          scope.failed = true;
          record.inputAbort?.abort();
          return agentAbort();
        }, configurable: false, writable: false });
      }
      Object.defineProperty(session, "abort", { value: () => {
        scope.failed = true;
        record.inputAbort?.abort();
        // Cancellation closes the task, including an input event whose reply was lost.
        return checked ? closeSession(session) : record.abort();
      }, configurable: false, writable: false });
      if (checked) {
        Object.defineProperty(session, "dispose", { value: () => closeSession(session),
          configurable: false, writable: false });
      }
      const prompt = session.prompt.bind(session);
      Object.defineProperty(session, "prompt", { value: (...arguments_) => {
        const execute = async () => {
          scope.check();
          if (record.pending) throw new Error("One AW Pi session permits one active prompt");
          const skill = skillChecks && expandsSkill(...arguments_);
          boundary?.validate(arguments_[0], arguments_[1], skill);
          scope.begin();
          record.inputAbort = new AbortController();
          try {
            if (checked) {
              try {
                const selected = skillChecks ? await prepareSkill(session, sdk, ...arguments_) : null;
                scope.checkExecution();
                await bridge.submit(selected?.text ?? arguments_[0], randomUUID(), record.inputAbort.signal, selected?.metadata);
                scope.checkExecution();
                if (selected) {
                  await selected.verify();
                  scope.checkExecution();
                  arguments_ = [selected.text, selected.options];
                }
                boundary.validate(...arguments_);
              } catch (error) {
                arguments_[1]?.preflightResult?.(false);
                throw error;
              }
            }
            scope.checkExecution();
            const value = boundary
              ? await boundary.run(arguments_[0], () => prompt(...arguments_))
              : await prompt(...arguments_);
            scope.checkExecution();
            return value;
          } finally {
            try { await bridge.drain(); }
            catch (error) { scope.failed = true; session.agent.abort(); throw error; }
            finally { record.inputAbort = null; }
          }
        };
        // Keep rejected concurrent calls from clearing the owner's pending promise.
        if (record.pending) return Promise.reject(new Error("One AW Pi session permits one active prompt"));
        const pending = execute();
        record.pending = pending;
        return pending.finally(() => { if (record.pending === pending) record.pending = null; });
      }, configurable: false, writable: false });
      active.set(session, record);
      return { ...result, close: () => closeSession(session) };
    } catch (error) {
      scope.failed = true;
      try {
        try {
          if (session instanceof sdk.AgentSession && !active.has(session)) {
            await bounded(session.abort(), 12000);
            session.dispose();
          }
        } finally { await scope.close(); }
      } catch (cleanupError) {
        poisoned = true;
        throw new AggregateError([error, cleanupError], "AW task creation cleanup failed");
      }
      throw error;
    }
  };

  const withSession = async (options, body) => {
    const result = await createSession(options);
    try { return await body(result.session); }
    finally { await result.close(); }
  };

  publishReady();
  try {
    await app.runWorker({ args, createSession, closeSession, withSession });
  } finally {
    const results = await Promise.allSettled([...active.keys()].map(closeSession));
    const failures = results.filter((result) => result.status === "rejected").map((result) => result.reason);
    if (failures.length) throw new AggregateError(failures, "AW resident task cleanup failed");
  }
}
