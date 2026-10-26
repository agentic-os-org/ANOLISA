/** Resident hosts create/dispose task sessions; tenant context never lives in a global current session. */
import { realpathSync } from "node:fs";
import { configuration, installSessionBridge, publishReady } from "./bridge.mjs";
import { Scope } from "./scope.mjs";

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
    record.closing = (async () => {
      try {
        try {
          await bounded(session.abort(), 12000);
          record.bridge.dispose();
          session.dispose();
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
      const stream = bridge.model(nativeStream);
      // The SDK may schedule a final turn after abort; gate before any model transport.
      Object.defineProperty(session.agent, "streamFunction", {
        configurable: false, enumerable: true,
        value: (...arguments_) => { scope.checkExecution(); return stream(...arguments_); }, writable: false,
      });
      const abort = session.abort.bind(session);
      Object.defineProperty(session, "abort", { value: (...arguments_) => {
        // Public abort has the same final-turn hazard as managed close.
        scope.failed = true;
        return abort(...arguments_);
      }, configurable: false, writable: false });
      const prompt = session.prompt.bind(session);
      let prompting = false;
      Object.defineProperty(session, "prompt", { value: async (...arguments_) => {
        scope.check();
        if (prompting) throw new Error("One AW Pi session permits one active prompt");
        scope.begin();
        prompting = true;
        try { const value = await prompt(...arguments_); scope.checkExecution(); return value; }
        finally {
          try { await bridge.drain(); }
          catch (error) { scope.failed = true; session.agent.abort(); throw error; }
          finally { prompting = false; }
        }
      }, configurable: false, writable: false });
      active.set(session, { scope, bridge, closing: null });
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
