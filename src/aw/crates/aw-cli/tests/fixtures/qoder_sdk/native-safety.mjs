/** Native permission composition and scheduling boundaries against a live Worker. */
import { timeoutSafety } from "./native-timeout.mjs";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";

export async function safety({ session, turn, prompt, results, root, reloadable, update, state }) {
  const evidence = [];
  const save = () => fs.writeFileSync(path.join(root, "safety.json"), JSON.stringify(evidence, null, 2));
  const pass = (name, detail) => {
    results.push({ case: name, status: "passed" });
    evidence.push({ case: name, ...detail }); save();
  };
  const events = () => fs.readFileSync(path.join(root, "events.jsonl"), "utf8").trim().split("\n").map(JSON.parse);
  let allows = 0;
  const application = await session(true, { hooks: { PreToolUse: [{ hooks: [async () => {
    allows++;
    return { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "allow" } };
  }] }] } });
  const messages = await turn(application, "printf AW_DENY > app-allow.txt", "app-allow.txt", false);
  assert(allows > 0);
  assert(events().some((e) => JSON.stringify(e.tool.input).includes("app-allow.txt")));
  assert(JSON.stringify(messages).includes("AW_NATIVE_RETRY: use an approved command"));
  pass("application-allow-aw-deny", { allows, messages });
  pass("denial-reason-preserved", {});

  let permissions = 0;
  const permission = await session(true, { permissionMode: "default", canUseTool: async () => {
    permissions++;
    return { behavior: "allow" };
  } });
  await turn(permission, "printf ALLOW > permission-allow.txt", "permission-allow.txt", true);
  assert(permissions > 0, "native allow callback was not exercised by the control");
  const before = permissions;
  const denied = await turn(permission, "printf AW_DENY > permission-deny.txt", "permission-deny.txt", false);
  assert(events().some((e) => JSON.stringify(e.tool.input).includes("permission-deny.txt")));
  pass("permission-allow-aw-deny", { before, after: permissions, messages: denied });

  if (reloadable) {
    for (const definition of [{ background: true }, { isolation: "worktree" }])
      await assert.rejects(session(true, { agents: { unsafe: { description: "unsafe",
        prompt: "inspect", ...definition } } }), /Background or worktree agents/);
    pass("agent-definition-admission", {});
    for (const [name, tool, input] of [
      ["background-tool-denied", "Bash", { command: "printf BACKGROUND > background.txt", run_in_background: true }],
      ["goal-activation-denied", "CreateGoal", { objective: "Reply GOAL_DONE", max_turns: 1 }],
      ["worktree-agent-denied", "Agent", { description: "inspect", prompt: "Run pwd once using Bash and stop.", subagent_type: "general-purpose", isolation: "worktree" }],
    ]) {
      const observed = [];
      const q = await session(true, { tools: ["Bash", "Agent", "CreateGoal"], hooks: { PreToolUse: [{ hooks: [async (input) => {
        observed.push(input);
        return { hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "allow" } };
      }] }] } });
      const output = await prompt(q, `Call ${tool} exactly once with this JSON: ${JSON.stringify(input)}. Use only the JSON properties shown; do not include schema metadata such as additionalProperties. If denied, report the denial and stop. Do not retry or substitute arguments.`);
      assert(observed.some((e) => e.tool_name === tool && (tool === "CreateGoal" || (input.isolation
        ? e.tool_input.isolation === "worktree" : e.tool_input.run_in_background === true))), "model did not exercise the requested guard");
      assert(!fs.existsSync("background.txt"));
      assert(JSON.stringify(output).includes("unsupported for reloadable Qoder SDK Queries"));
      if (tool === "CreateGoal") assert.equal(await q.q.getGoal(), null);
      assert.equal(state().requests, 0);
      update("candidate.json");
      pass(name, { observed, messages: output });
      await q.q.return();
    }
  }

  if (reloadable) await timeoutSafety({ session, results, root, update, state });

  // Probe the unwrapped native semantics; these Queries do not claim AW protection.
  if (!reloadable) {
    const observed = [];
    const goal = await session(false, { tools: ["UpdateGoal", "CreateGoal"], hooks: { PreToolUse: [{ hooks: [async (input) => {
      observed.push(input); return {};
    }] }] } });
    const create = await prompt(goal, 'Call CreateGoal exactly once with {"objective":"Reply GOAL_DONE and finish immediately","max_turns":1}. Then call UpdateGoal with {"status":"complete"} and stop.');
    assert(observed.some((e) => e.tool_name === "CreateGoal"));
    assert(observed.some((e) => e.tool_name === "UpdateGoal" && e.tool_input.status === "complete"));
    const toolResults = create.flatMap((m) => m.type === "user" ? m.message.content : []).filter((m) => m.type === "tool_result");
    assert(toolResults.some((r) => String(r.content).includes('"status":"active"')));
    assert(toolResults.some((r) => String(r.content).includes('"status":"complete"')));
    pass("native-goal-create-and-complete", { observed: [...observed], messages: create });
    await goal.q.clearGoal();
    await goal.q.return();

    for (const args of [["init", "-q"], ["-c", "user.name=AW Test", "-c", "user.email=aw-test@example.invalid", "commit", "--allow-empty", "-qm", "fixture"]]) {
      const result = spawnSync("git", args, { encoding: "utf8", timeout: 10000 });
      assert.equal(result.status, 0, result.stderr);
    }
    const worktree = [];
    const isolated = await session(true, { tools: ["Agent", "Bash"], hooks: { PreToolUse: [{ hooks: [async (input) => {
      worktree.push(input); return {};
    }] }] } });
    const output = await prompt(isolated, 'Call Agent exactly once with {"description":"cwd probe","prompt":"Use Bash exactly once to run pwd, return its output and stop.","subagent_type":"general-purpose","isolation":"worktree"}. Return the result and stop.');
    const child = worktree.find((e) => e.tool_name === "Bash" && e.agent_id && e.tool_input.command.trim() === "pwd");
    assert(child, "worktree child did not execute the cwd probe");
    assert.equal(child.cwd, process.cwd(), "pinned Worker changed its child callback cwd");
    const result = output.flatMap((m) => m.type === "user" ? m.message.content : []).find((m) =>
      m.type === "tool_result" && m.tool_use_id === child.tool_use_id);
    assert(result && !result.is_error);
    assert(String(result.content).trim().startsWith(path.join(process.cwd(), ".qoder/worktrees/")));
    const callbacks = events().filter((e) => (e.native.aw_native_tool_use_id ?? e.tool.call_id) === child.tool_use_id);
    assert(callbacks.some((e) => e.name === "tool.before" && e.native.agent_id === child.agent_id));
    assert(callbacks.some((e) => e.name === "tool.after"));
    pass("worktree-callback-cwd", { cwd: process.cwd(), observed: worktree, messages: output });
    await isolated.q.return();
  }
}
