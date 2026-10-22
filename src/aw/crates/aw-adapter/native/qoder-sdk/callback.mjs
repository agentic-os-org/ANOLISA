/** Bounded callback subprocess; only neutral or deny directives are admitted. */
import { spawn } from "node:child_process";
export async function invoke(config, hook, payload, signal) {
  const input = Buffer.from(JSON.stringify(payload));
  if (input.length > 1024 * 1024) throw new Error("native_input_limit");
  if (signal?.aborted) throw new Error("callback_aborted");
  return await new Promise((resolve, reject) => {
    const child = spawn(
      config.executable,
      [
        "hook",
        "--adapter",
        "qoder-sdk",
        "--binding",
        config.binding,
        "--event",
        hook.event,
        "--step",
        hook.step,
        "--on-error",
        hook.on_error,
      ],
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
      hook.budget_ms + 3000,
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
      if (code === 2 && hook.event === "tool.before") {
        try {
          const reason = stdout.length ? JSON.parse(stdout.toString("utf8")) : "Tool blocked by AW policy";
          if (typeof reason !== "string") throw new Error("invalid_native_response");
          return resolve({ hookSpecificOutput: { hookEventName: "PreToolUse",
            permissionDecision: "deny", permissionDecisionReason: reason } });
        } catch (error) { return reject(error); }
      }
      if (code !== 0) return reject(new Error(`callback_exit_${code}`));
      try {
        const value = stdout.length ? JSON.parse(stdout.toString("utf8")) : {};
        if (!value || Array.isArray(value) || typeof value !== "object" || Object.keys(value).length)
          throw new Error("unsupported_qoder_sdk_directive");
        resolve(value);
      } catch (error) { reject(error); }
    });
    child.stdin.end(input);
    if (signal?.aborted) abort();
  });
}
