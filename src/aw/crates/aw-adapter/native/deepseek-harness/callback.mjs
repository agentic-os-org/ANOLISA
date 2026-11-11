/** Bounded callback transport; only AW's authorized final step can replace text. */
import { spawn } from "node:child_process";
import { isDeepStrictEqual } from "node:util";

export function transport(config) {
  const children = new Set();
  const stop = (child) => {
    if (!child.pid) return;
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch (error) {
      if (error.code !== "ESRCH") throw error;
    }
  };
  const stopAll = () => {
    for (const child of children) stop(child);
  };
  process.on("exit", stopAll);
  process.on("SIGINT", stopAll);
  process.on("SIGTERM", stopAll);
  return {
    dispose() {
      stopAll();
      for (const event of ["exit", "SIGINT", "SIGTERM"])
        process.off(event, stopAll);
    },
    async invoke(hook, payload, signal) {
      const input = Buffer.from(JSON.stringify(payload));
      if (input.length > 1024 * 1024) throw Error("native_input_limit");
      if (signal.aborted) throw Error("callback_aborted");
      return await new Promise((resolve, reject) => {
        const child = spawn(
          config.executable,
          [
            "hook",
            "--adapter",
            "deepseek-harness",
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
        children.add(child);
        let failure;
        let stdout = Buffer.alloc(0);
        let stderr = Buffer.alloc(0);
        const fail = (error) => {
          failure ??= error;
          stop(child);
        };
        const abort = () => fail(Error("callback_aborted"));
        signal.addEventListener("abort", abort, { once: true });
        if (signal.aborted) abort();
        const timer = setTimeout(
          () => fail(Error("callback_timeout")),
          hook.budget_ms + 3000,
        );
        child.on("error", fail);
        child.stdin.on("error", fail);
        child.stdout.on("data", (data) => {
          if (stdout.length + data.length > 1024 * 1024)
            fail(Error("callback_stdout_limit"));
          else stdout = Buffer.concat([stdout, data]);
        });
        child.stderr.on("data", (data) => {
          if (stderr.length + data.length > 65536)
            fail(Error("callback_stderr_limit"));
          else stderr = Buffer.concat([stderr, data]);
        });
        child.on("close", (code) => {
          clearTimeout(timer);
          signal.removeEventListener("abort", abort);
          children.delete(child);
          // Reap descendants even when the direct callback exits successfully.
          stop(child);
          if (stderr.length) process.stderr.write(stderr);
          if (failure) return reject(failure);
          if (code !== 0) return reject(Error(`callback_exit_${code}`));
          try {
            const directive = stdout.length
              ? JSON.parse(stdout.toString("utf8"))
              : {};
            if (
              !directive ||
              Array.isArray(directive) ||
              typeof directive !== "object" ||
              Object.keys(directive).some(
                (key) => !["block", "reason", "aw_replacement"].includes(key),
              ) ||
              (directive.block !== undefined &&
                typeof directive.block !== "boolean") ||
              (directive.reason !== undefined &&
                typeof directive.reason !== "string") ||
              (hook.event === "tool.after" && directive.block === true)
            )
              throw Error("invalid_native_directive");
            if (Object.hasOwn(directive, "aw_replacement")) {
              if (
                hook.event !== "tool.after" ||
                hook.step !== config.replacement_step
              )
                throw Error("unauthorized_replacement");
              const original = payload.tool_response;
              const replacement = directive.aw_replacement;
              const text = replacement?.content?.[0]?.text;
              if (
                original.isError ||
                payload.nested ||
                original.content?.length !== 1 ||
                original.content[0].type !== "text" ||
                typeof text !== "string" ||
                !text ||
                Buffer.byteLength(text) > 1024 * 1024
              )
                throw Error("invalid_replacement_slot");
              const expected = structuredClone(original);
              expected.content[0].text = text;
              if (!isDeepStrictEqual(expected, replacement))
                throw Error("replacement_changed_metadata");
            }
            resolve(directive);
          } catch (error) {
            reject(error);
          }
        });
        child.stdin.end(input);
      });
    },
  };
}
