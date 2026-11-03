/** One bounded CLI child; completion joins owned stdio and cancellation kills its group. */
import { spawn } from "node:child_process";

export async function run(executable, args, input, timeout, signal, environment) {
  if (input.length > 1024 * 1024) throw new Error("native_input_limit");
  if (signal?.aborted) throw new Error("callback_aborted");
  return await new Promise((resolve, reject) => {
    const child = spawn(executable, args, {
      stdio: ["pipe", "pipe", "pipe"], detached: true, env: environment,
    });
    let failure;
    let stdout = Buffer.alloc(0);
    let stderr = Buffer.alloc(0);
    const stop = (error) => {
      failure ??= error;
      if (child.pid) {
        try { process.kill(-child.pid, "SIGKILL"); }
        catch (error) { if (error.code !== "ESRCH") failure = error; }
      }
    };
    const abort = () => stop(new Error("callback_aborted"));
    const timer = setTimeout(() => stop(new Error("callback_timeout")), timeout);
    signal?.addEventListener("abort", abort, { once: true });
    child.on("error", stop);
    child.stdin.on("error", stop);
    child.stdout.on("data", (data) => {
      if (stdout.length + data.length > 1024 * 1024) stop(new Error("callback_stdout_limit"));
      else stdout = Buffer.concat([stdout, data]);
    });
    child.stderr.on("data", (data) => {
      if (stderr.length + data.length > 65536) stop(new Error("callback_stderr_limit"));
      else stderr = Buffer.concat([stderr, data]);
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", abort);
      if (stderr.length) process.stderr.write(stderr);
      if (failure) reject(failure);
      else resolve({ code, stdout });
    });
    child.stdin.end(input);
  });
}
