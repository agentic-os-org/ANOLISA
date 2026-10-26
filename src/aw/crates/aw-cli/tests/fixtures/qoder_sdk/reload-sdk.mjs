/** Synthetic native command queue with independently held execution and cleanup. */
import { spawn } from "node:child_process";
import fs from "node:fs";
import { randomUUID } from "node:crypto";
export async function startDaemon() { return { async stop() {} }; }
export const gates = new Map();
export const backgrounds = new Map();
export function gate(name) {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  const value = { promise, resolve };
  gates.set(name, value);
  return value;
}
export function query({ prompt, options }) {
  const session = randomUUID();
  let closed = false, wake, currentCommand, turn = Promise.resolve();
  const messages = [];
  function emit(message) { messages.push({ ...message, session_id: session }); wake?.(); }
  const q = {
    options,
    emit,
    async initializationResult() { return { capabilities: ["msg_lifecycle_v1"] }; },
    async next() {
      while (!messages.length && !closed) await new Promise((done) => { wake = done; });
      return messages.length ? { value: messages.shift(), done: false } : { done: true };
    },
    async streamInput(source) {
      for await (const message of source) {
        if (closed) break;
        turn = (async () => {
          const command = message.message.content;
          currentCommand = command;
          const id = randomUUID();
          const payload = { session_id: session, tool_use_id: id, cwd: process.cwd(),
            tool_name: "Bash", tool_input: { command, ...(command === "background" ? { run_in_background: true } : {}) }, scenario: "block" };
          if (command.startsWith("agent-")) {
            payload.tool_name = "Agent";
            payload.tool_input = { subagent_type: "foreground", prompt: "inspect",
              ...(command === "agent-background" ? { run_in_background: true } : {}),
              ...(command === "agent-worktree" ? { isolation: "worktree" } : {}),
              ...(command === "agent-default" ? { isolation: "default" } : {}) };
          } else if (command.startsWith("goal-")) {
            payload.tool_name = command === "goal-create" ? "CreateGoal" : "UpdateGoal";
            payload.tool_input = command === "goal-create" ? { objective: "inspect" } : { status: "complete" };
          } else if (command === "wakeup" || command.startsWith("cron-")) {
            payload.tool_name = { wakeup: "ScheduleWakeup", "cron-create": "CronCreate",
              "cron-list": "CronList", "cron-delete": "CronDelete" }[command];
            payload.tool_input = command === "wakeup"
              ? { delaySeconds: 60, reason: "review", prompt: "Run queued work" }
              : { cron: "* * * * *", prompt: "Run queued work" };
          } else if (command === "custom-worktree") {
            payload.tool_name = "custom";
            payload.tool_input = { isolation: "worktree" };
          }
          let reason;
          const callback = async (name, extra = {}) => {
            for (const group of options.hooks[name] ?? [])
              for (const hook of group.hooks) {
                const result = await hook({ ...payload, ...extra, hook_event_name: name }, id, {});
                if (result.hookSpecificOutput?.permissionDecision === "deny") {
                  reason = result.hookSpecificOutput.permissionDecisionReason;
                  return true;
                }
              }
            return false;
          };
          gates.get(`${command}:submitted`)?.resolve();
          await gates.get(`${command}:model`)?.promise;
          const denied = await callback("PreToolUse");
          gates.get(`${command}:started`)?.resolve();
          await gates.get(`${command}:tool`)?.promise;
          if (!denied && ["wakeup", "cron-create"].includes(command))
            fs.writeFileSync(`${command}.scheduled`, "native scheduling reached");
          if (!denied && command.startsWith("timeout-")) {
            const child = spawn(process.execPath, ["-e", 'process.stdin.resume();process.stdin.on("end",()=>process.exit(0));']);
            const record = { pid: child.pid, command: child.spawnargs, cwd: process.cwd(),
              ports: [], deadline_seconds: 60, stop: `kill -TERM -- ${child.pid}` };
            fs.writeFileSync(`${command}.process.json`, JSON.stringify(record));
            const exited = new Promise((resolve) => child.once("exit", resolve));
            backgrounds.set(command, { id, child, exited });
            await new Promise((resolve) => setTimeout(resolve, 5));
            emit({ type: "system", subtype: "background_tasks_changed", tasks: [{ task_id: id }] });
            exited.then(async () => {
              const cleanup = gates.get(`${command}:background-cleanup`);
              turn = cleanup?.promise ?? Promise.resolve();
              emit({ type: "system", subtype: "background_tasks_changed", tasks: [] });
              emit({ type: "system", subtype: "task_notification", task_id: id, status: "completed" });
              emit({ type: "result", autonomous: true });
              await turn;
            });
          }
          if (!denied) await callback("PostToolUse", { tool_response: "native result" });
          // Real Worker can emit these before turn-finally cleanup is complete.
          emit({ type: "command_lifecycle", command_uuid: message.uuid, state: "completed" });
          emit({ type: "result", denied, reason });
          await gates.get(`${command}:cleanup`)?.promise;
        })();
        await turn;
      }
    },
    async getContextUsage() {
      if (options.failBarrier) throw new Error("native barrier failed");
      const pending = turn;
      gates.get(`${currentCommand}:barrier`)?.resolve();
      await pending;
      return {};
    },
    async stopTask(id) {
      const task = [...backgrounds.values()].find((item) => item.id === id);
      gates.get("stop-entered")?.resolve();
      await gates.get("stop-release")?.promise;
      task.child.stdin.end();
      await task.exited;
    },
    async close() {
      if (options.failClose) throw new Error("native cleanup failed");
      gates.get("close-entered")?.resolve();
      await gates.get("close-release")?.promise;
      await turn;
      closed = true;
      wake?.();
    },
    async return() { await this.close(); return { done: true }; },
  };
  q.streamInput(prompt).then(() => q.close()).catch((error) => { emit({ type: "error", message: error.message }); });
  return q;
}
