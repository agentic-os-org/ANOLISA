/** Bounded callback subprocess; only neutral or deny directives are admitted. */
import { spawn } from "node:child_process";
async function execute(config, args, payload, signal, timeout) {
  const input = Buffer.from(JSON.stringify(payload));
  if (input.length > 1024 * 1024) throw new Error("native_input_limit");
  if (signal?.aborted) throw new Error("callback_aborted");
  return await new Promise((resolve, reject) => {
    const child = spawn(
      config.executable,
      args,
      { stdio: ["pipe", "pipe", "pipe"], detached: true },
    );
    let failure;
    let stdout = Buffer.alloc(0);
    let stderr = Buffer.alloc(0);
    const stop = (error) => {
      failure ??= error;
      if (child.pid) {
        try {
          process.kill(-child.pid, "SIGKILL");
        } catch (error) {
          if (error.code !== "ESRCH") failure = error;
        }
      }
    };
    const abort = () => stop(new Error("callback_aborted"));
    const timer = setTimeout(
      () => stop(new Error("callback_timeout")),
      timeout,
    );
    signal?.addEventListener("abort", abort, { once: true });
    child.on("error", stop);
    child.stdin.on("error", stop);
    child.stdout.on("data", (data) => {
      if (stdout.length + data.length > 1024 * 1024)
        stop(new Error("callback_stdout_limit"));
      else stdout = Buffer.concat([stdout, data]);
    });
    child.stderr.on("data", (data) => {
      if (stderr.length + data.length > 65536)
        stop(new Error("callback_stderr_limit"));
      else stderr = Buffer.concat([stderr, data]);
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
      if (stderr.length) process.stderr.write(stderr);
      if (failure) return reject(failure);
      resolve({ code, stdout });
    });
    child.stdin.end(input);
    if (signal?.aborted) abort();
  });
}

export async function lifecycle(config, phase, key = "") {
  const { code } = await execute(config,
    ["bridge", "--adapter", "qoder-sdk", "--binding", config.binding],
    { phase, key, pid: process.pid }, undefined, 5000);
  if (code !== 0) throw new Error(`lifecycle_${phase}_exit_${code}`);
}

export async function invoke(config, hook, payload, signal) {
  const { code, stdout } = config.scope ? await config.scope.invoke(hook, payload, signal) : await execute(config,
    ["hook", "--adapter", "qoder-sdk", "--binding", config.binding,
      "--event", hook.event, "--step", hook.step, "--on-error", hook.on_error],
    payload, signal, hook.budget_ms + 3000);
  if (code === 2 && hook.event === "tool.before") {
    const reason = stdout.length ? JSON.parse(stdout.toString("utf8")) : "Tool blocked by AW policy";
    if (typeof reason !== "string") throw new Error("invalid_native_response");
    return { hookSpecificOutput: { hookEventName: "PreToolUse",
      permissionDecision: "deny", permissionDecisionReason: reason } };
  }
  if (code !== 0) throw new Error(`callback_exit_${code}`);
  const value = stdout.length ? JSON.parse(stdout.toString("utf8")) : {};
  if (!value || Array.isArray(value) || typeof value !== "object" || Object.keys(value).length)
    throw new Error("unsupported_qoder_sdk_directive");
  return value;
}
