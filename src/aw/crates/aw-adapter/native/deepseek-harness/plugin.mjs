/** Native tool policy; framework permissions and structured values stay native. */
import { evaluate, identity, installTools, validatePolicy } from "./hooks.mjs";
import { readFileSync, writeFileSync, renameSync, unlinkSync } from "node:fs";
import { transport } from "./callback.mjs";

export const name = "aw-deepseek";
export const inject = ["tools"];

export function apply(ctx) {
  const config = JSON.parse(readFileSync(process.env.AW_NATIVE_CONFIG, "utf8"));
  for (const key of ["executable", "binding", "ready_path", "ready_token"])
    identity(config[key], key);
  validatePolicy(config);
  const callback = transport(config);
  const binding = { ...config, invoke: callback.invoke };
  const final = config.hooks.filter(hook => hook.event === "security.violation");
  const finalGate = Symbol.for("aw.deepseek.finalGate");
  if (final.length && globalThis[Symbol.for("aw.deepseek.finalPatched")] !== true)
    throw Error("AW final result gate was not installed before DSH Agent startup");
  const completed = new WeakMap();
  const disposers = [installTools(ctx, () => binding, {
    inputEnabled: config.hooks.some(hook => hook.event === "input.submit"),
    resultGate: config.result_gate,
    completed: final.length ? completed : undefined,
  })];
  const lifecycle = (event) => config.hooks.filter((hook) => hook.event === event);
  const sessionStart = lifecycle("session.start");
  const turnStop = lifecycle("turn.stop");
  const observationTasks = new Set();
  const enqueue = (routes, payload) => {
    if (observationTasks.size >= 128) {
      process.stderr.write("AW DeepSeek lifecycle observation capacity exceeded\n");
      return;
    }
    const done = evaluate(binding, routes[0].event, payload).finally(() => observationTasks.delete(done));
    observationTasks.add(done);
  };
  if (sessionStart.length)
    disposers.push(ctx.on("agent/created", async ({ agent, source, signal }) => {
      if (source === "compact") return;
      await evaluate(binding, "session.start", {
        event: "session.start", seam: "agent/created", session_id: identity(agent.session.id, "session id"),
        source,
      }, signal);
    }));
  if (turnStop.length)
    disposers.push(ctx.on("session/event", (session, event) => {
      if (event.type !== "turn/end") return;
      const { turn, reason } = event.data;
      enqueue(turnStop, {
        event: "turn.stop", seam: "turn/end", session_id: identity(session.id, "session id"),
        turn_id: String(turn), reason: identity(reason.kind, "turn end reason"),
        ...(typeof reason.reason === "string" ? { cancellation_reason: reason.reason } : {}),
      });
    }));
  if (final.length) {
    globalThis[finalGate] = async (exec, result) => {
      const record = completed.get(result);
      completed.delete(result);
      const denied = (reason) => ({
        isError: true,
        content: [{ type: "text", text: "AW withheld the final tool result" }],
        error: { message: reason },
      });
      if (!record) return denied("AW final result identity was not verified");
      try {
        const directive = await evaluate(binding, "security.violation", {
          ...record.payload,
          event: "security.violation",
          tool_response: result,
        }, exec.signal);
        return directive.block ? denied("AW final result security check blocked the result") : result;
      } catch (error) {
        process.stderr.write(`AW final result gate: ${error.message}\n`);
        return denied("AW final result security check failed");
      }
    };
  }
  ctx.effect(() => async () => {
    for (const dispose of disposers.reverse()) dispose();
    if (final.length) globalThis[finalGate] = async () => {
      throw Error("AW final result gate was disposed");
    };
    await Promise.allSettled(observationTasks);
    callback.dispose();
  });
  const temporary = `${config.ready_path}.${process.pid}.tmp`;
  try {
    writeFileSync(
      temporary,
      JSON.stringify({
        version: 1,
        adapter: "deepseek-harness",
        pid: process.pid,
        token: config.ready_token,
        hooks: config.hooks.length,
      }),
      { mode: 0o600, flag: "wx" },
    );
    renameSync(temporary, config.ready_path);
  } finally {
    try {
      unlinkSync(temporary);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
  }
  // The Agent loop waits for this service before accepting tool work.
  ctx.provide("awDeepseek", {});
}
