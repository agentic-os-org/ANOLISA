/** Application-owned Cordis profile with one immutable AW Task per Bridge request. */
import { Admission, AwError, Instance } from "../../../typescript/src/client.mjs";
import { loadConfiguration } from "../../../typescript/src/configuration.mjs";
import { identity, installTools, validatePolicy } from "../../../../crates/aw-adapter/native/deepseek-harness/hooks.mjs";
import { parseDirective } from "../../../../crates/aw-adapter/native/deepseek-harness/callback.mjs";

const profile = Object.freeze({
  adapter: "deepseek-harness", version: "cordis-hooks/v1", entrypoint: "cordis",
});

export async function createDeepSeekBridge({
  configFile, descriptorFile, target, executable, attachTimeoutMs = 5000,
}) {
  if (!Number.isInteger(attachTimeoutMs) || attachTimeoutMs < 100 || attachTimeoutMs > 60000)
    throw new AwError("invalid_attach_timeout");
  const configured = await loadConfiguration({
    configFile, descriptorFile, target, executable, profile, requiresService: true,
  });
  const events = { "tool.before": ["observe", "block"], "tool.after": ["observe", "replace_result"] };
  if (configured.inputEnabled) events["input.submit"] = ["observe", "block"];
  const sessions = new Map();
  let installed = false;
  let closed = false;
  const instance = new Instance({
    ...configured,
    reconnectMs: 100,
    timeoutMs: 6000,
    capabilities: { ...profile, events, failure_actions: { "tool.after": ["withhold_result"] } },
    install(routes) {
      validatePolicy({ hooks: routes.hooks, replacement_step: routes.events?.["tool.after"]?.replacement_step });
      return installed && !closed && routes.hooks.every(hook => Object.hasOwn(events, hook.event));
    },
  });
  let closing;
  const close = () => {
    closed = true;
    return closing ??= instance.close();
  };
  return {
    plugin: {
      name: "aw-deepseek-cordis",
      inject: ["tools"],
      apply(ctx) {
        if (installed || closed) throw new AwError("deepseek_bridge_unavailable");
        const dispose = installTools(ctx, (sessionId, { forPublication = false } = {}) => {
          const binding = sessions.get(sessionId);
          if (!binding || closed || (binding.signal?.aborted && !forPublication)) throw new AwError("deepseek_task_required");
          return binding;
        }, { inputEnabled: configured.inputEnabled, resultGate: true, cooperative: true });
        ctx.effect(() => async () => {
          installed = false;
          dispose();
          await close();
        });
        installed = true;
        instance.listen();
        ctx.provide("awDeepseek", {});
      },
    },
    async runTask(sessionId, work, { signal, cwd } = {}) {
      identity(sessionId, "session id");
      if (sessions.has(sessionId)) throw new AwError("deepseek_session_busy");
      if (!installed || closed) throw new AwError("deepseek_bridge_unavailable");
      const admission = new Admission(instance, {
        events: Object.keys(events), timeoutMs: attachTimeoutMs, cwd,
      });
      sessions.set(sessionId, null);
      try {
        await admission.prepare(signal);
        signal?.throwIfAborted();
        const { hooks, events: routes } = admission.routes;
        const binding = {
          hooks, signal,
          async invoke(hook, payload, executionSignal) {
            if (closed || sessions.get(sessionId) !== binding) throw new AwError("deepseek_task_closed");
            const signals = [signal, executionSignal].filter(Boolean);
            const { code, stdout, stderr } = await admission.invoke(hook, payload, AbortSignal.any(signals));
            if (stderr.length) process.stderr.write(stderr);
            if (code !== 0) throw new AwError("deepseek_callback_failed");
            return parseDirective(stdout, hook, payload, routes["tool.after"]?.replacement_step);
          },
        };
        sessions.set(sessionId, binding);
        return await work();
      } finally {
        sessions.set(sessionId, null);
        try { await admission.close(); }
        finally { sessions.delete(sessionId); }
      }
    },
    close,
  };
}
