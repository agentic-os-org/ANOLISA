/** Native Cordis bindings shared by managed launches and cooperative Bridges. */
import { randomUUID } from "node:crypto";

export function identity(value, label) {
  if (
    typeof value !== "string" ||
    !value.trim() ||
    Buffer.byteLength(value) > 512
  )
    throw Error(`DeepSeek requires bounded ${label}`);
  return value;
}

export function validatePolicy(config) {
  if (
    !Array.isArray(config.hooks) ||
    config.hooks.some(
      (hook) =>
        !["session.start", "input.submit", "tool.before", "tool.after", "security.violation", "turn.stop"].includes(hook.event) ||
        !hook.step ||
        !(["block", "report"].includes(hook.on_error) ||
          (hook.event === "tool.after" && hook.on_error === "withhold_result")) ||
        !Number.isInteger(hook.budget_ms) ||
        hook.budget_ms < 1 ||
        hook.budget_ms > 55000,
    )
  )
    throw Error("Invalid AW DeepSeek routes");
  const after = config.hooks.filter((hook) => hook.event === "tool.after");
  if (
    config.replacement_step != null &&
    after.at(-1)?.step !== config.replacement_step
  )
    throw Error("AW replacement must be the final after step");
}

export async function evaluate(binding, event, payload, signal) {
  let withhold = false;
  for (const hook of binding.hooks.filter(hook => hook.event === event)) {
    try {
      const directive = await binding.invoke(hook, { ...payload, event }, signal);
      if (directive.aw_withheld) withhold = true;
      if (directive.block || directive.aw_replacement)
        return withhold ? { aw_withheld: true } : directive;
    } catch (error) {
      process.stderr.write(`AW ${event}/${hook.step}: ${error.message}; on_error=${hook.on_error}\n`);
      if (hook.on_error === "withhold_result") withhold = true;
      if (event !== "tool.after" && hook.on_error === "block")
        return { block: true, reason: "AW hook failed; configured failure action blocked the event" };
    }
  }
  return withhold ? { aw_withheld: true } : {};
}

function withheld() {
  const message = "AW withheld the tool result";
  return { isError: true, content: [{ type: "text", text: message }], error: { message } };
}

export function installTools(ctx, binding, { inputEnabled, resultGate, completed, cooperative = false }) {
  const slot = ctx.tools?.[Symbol.for("aw.deepseek.resultGate")];
  if (resultGate && slot?.current !== null)
    throw Error("AW requires one preloaded DeepSeek result gate per ToolRuntime");
  const active = new Map();
  const disposers = [];
  if (inputEnabled)
    disposers.push(
      ctx.on(
        "agent/pre-step",
        async ({ agent, messages, turn, signal }, next) => {
          for (const message of messages) {
            if (
              !Array.isArray(message.content) ||
              message.content.some(
                (part) => part?.type !== "text" || typeof part.text !== "string",
              )
            ) {
              process.stderr.write("AW input.submit: unsupported content\n");
              return { kind: "reject" };
            }
            const directive = await evaluate(
              binding(agent.id), "input.submit",
              {
                event: "input.submit",
                session_id: identity(agent.id, "session id"),
                aw_turn_id: identity(
                  `${turn}:${identity(message.id, "message id")}`,
                  "turn id",
                ),
                input_text: message.content.map((part) => part.text).join("\n"),
              },
              signal,
            );
            if (directive.block) {
              process.stderr.write(
                `AW input.submit: ${directive.reason ?? "Blocked by AW policy"}\n`,
              );
              return { kind: "reject" };
            }
          }
          return next();
        },
        { prepend: true },
      ),
    );
  disposers.push(
    ctx.on(
      "tools/pre-execute",
      async (exec, next) => {
        if (active.size >= 512 || active.has(exec.token))
          throw Error("AW execution capacity exceeded");
        const record = {
          binding: binding(exec.agent?.id),
          payload: {
            invocation_id: randomUUID(),
            session_id: identity(exec.agent?.id, "session id"),
            nested: exec.parent !== undefined,
            tool_call: {
              id: identity(exec.callId, "call id"),
              name: identity(exec.name, "tool name"),
              input: exec.arguments,
            },
          },
          denied: "AW preparation did not complete",
        };
        active.set(exec.token, record);
        const directive = await evaluate(
          record.binding, "tool.before",
          { ...record.payload, event: "tool.before" },
          exec.signal,
        );
        record.denied = directive.block
          ? (directive.reason ?? "Blocked by AW policy")
          : undefined;
        if (record.denied !== undefined)
          return { kind: "deny", reason: record.denied };
        return await next();
      },
      { prepend: true },
    ),
  );
  // Guards are synchronous and monotonic. They consume the async decision and
  // also deny an outer plugin's allow that bypassed AW's pre-execute listener.
  disposers.push(
    ctx.tools.guard(
      (exec) =>
        active.get(exec.token)?.denied ??
        (active.has(exec.token) ? undefined : "AW preparation was bypassed"),
    ),
  );
  if (!cooperative) disposers.push(
    ctx.on(
      "tools/post-execute",
      async (exec, result, next) => {
        const decision = await next();
        const record = active.get(exec.token);
        if (!record) return decision;
        // Preserve the ordinary single-slot Hook. Strict routes and structured
        // bash need the actual result after Cordis renders the final decision.
        if (resultGate && (exec.name === "bash" || record.binding.hooks.some(h => h.on_error === "withhold_result")))
          return decision;
        record.checkedInPost = true;
        // A value replacement requires native schema/render processing before its
        // text is known. Never compress that unmaterialized value or nested PTC data.
        if (Object.hasOwn(decision, "value")) return decision;
        const effective =
          decision.kind === "block"
            ? { content: decision.feedback, isError: true }
            : {
                ...result,
                ...(decision.content === undefined
                  ? {}
                  : { content: decision.content }),
              };
        const directive = await evaluate(
          record.binding, "tool.after",
          { ...record.payload, event: "tool.after", tool_response: effective },
          exec.signal,
        );
        if (directive.aw_replacement && decision.kind === "accept")
          return { ...decision, content: directive.aw_replacement.content };
        return decision;
      },
      { prepend: true },
    ),
  );
  disposers.push(ctx.on("tools/result", (exec, result) => {
    const record = active.get(exec.token);
    active.delete(exec.token);
    if (record) completed?.set(result, record);
  }));
  if (resultGate) {
    // Native denial and cancellation can reach publication before pre-execute ran.
    const skipped = (exec, result) => {
      try {
        const policy = active.get(exec.token)?.binding ?? binding(exec.agent?.id, { forPublication: true });
        return policy.hooks.some(hook => hook.on_error === "withhold_result") ? withheld() : result;
      } catch {
        // An unbound Bridge call has no admitted policy to authorize publication.
        return withheld();
      }
    };
    slot.current = {
      async after(exec, result) {
        const record = active.get(exec.token);
        if (!record) return skipped(exec, result);
        if (record.checkedInPost) return result;
        const directive = await evaluate(record.binding, "tool.after",
          { ...record.payload, aw_result_gate: true, tool_response: result }, exec.signal);
        if (directive.aw_withheld) return withheld();
        return directive.aw_replacement ?? result;
      },
      skipped,
    };
    disposers.push(() => { slot.current = null; });
  }
  return () => {
    for (const dispose of disposers.reverse()) dispose();
    active.clear();
  };
}
