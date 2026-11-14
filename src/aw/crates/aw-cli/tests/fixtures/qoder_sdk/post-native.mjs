/** Real Worker result withholding through an admitted ordinary AW resident Query. */
import assert from "node:assert/strict";
import fs from "node:fs";
import { failedDelivery, modelText } from "./post-evidence.mjs";
import path from "node:path";
import { pathToFileURL } from "node:url";
const { createQuery, startDaemon } = await import(pathToFileURL(process.env.AW_QODER_SDK_BRIDGE));
const root = process.env.QODER_TEST_ROOT;
const mode = process.env.QODER_POST_CASE;
const final = mode.startsWith("gate-");
const late = mode === "late-mutation" || mode === "gate-late";
const finalBlocked = final && !["gate-safe", "gate-native-error"].includes(mode);
const withheld = ["failure", "timeout", "failure-error"].includes(mode);
const toolFailure = ["native-error", "failure-error", "gate-native-error"].includes(mode);
const marker = path.join(root, "second-tool");
const sentinel = "AW_NATIVE_POST_RESULT";
const lateMarker = "AW_QODER_LATE_POST_SECRET";
const command = toolFailure ? `printf ${sentinel} >&2; exit 7` : `printf ${sentinel}`;
const observed = [], models = [], messages = [];
let daemon, query, failure, finalized = 0;
const children = () => fs.readFileSync(`/proc/${process.pid}/task/${process.pid}/children`, "utf8")
  .trim().split(/\s+/).filter(Boolean).map(Number);
const timer = setTimeout(() => query?.close().catch(() => {}), 90000);
try {
  daemon = await startDaemon({ auth: { type: "accessToken",
    accessToken: { envVar: "QODER_PERSONAL_ACCESS_TOKEN" } },
  debugFile: path.join(root, "daemon.log"), stderr: data => fs.appendFileSync(path.join(root, "native-stderr.log"), data) });
  fs.writeFileSync(path.join(root, "daemon-process.json"), JSON.stringify({
    pids: children(), cwd: process.cwd(), ports: [], expected_lifetime_seconds: 100,
    log: path.join(root, "daemon.log"), stop: "await query.close(); await daemon.stop()" }));
  const observer = async input => {
    observed.push(structuredClone(input));
    try { return late ? { hookSpecificOutput: {
      hookEventName: "PostToolUse", updatedToolOutput: lateMarker } } : {}; }
    finally { await new Promise(resolve => setTimeout(resolve, 100)); finalized++; }
  };
  query = await createQuery({ prompt:
    `Call Bash exactly once with ${JSON.stringify({ command })}. After receiving its result, ` +
    `call Bash again with ${JSON.stringify({ command: `touch ${marker}` })}. Then stop. ` +
    "Do not combine commands or execute the second call before the first result arrives." +
    (final || late ? " In the final answer, quote the first Bash output exactly." : ""),
  options: { daemon, cwd: process.cwd(), settingSources: [], persistSession: false,
    permissionMode: "bypassPermissions", tools: ["Bash"], maxTurns: 4,
    ...(final ? { model: "auto" } : { resolveModel: async input => { models.push(input.purpose); return { model: "auto" }; } }),
    hooks: { PostToolUse: [{ hooks: [observer] }], PostToolUseFailure: [{ hooks: [observer] }] } } });
  try { for await (const message of query) { messages.push(message); if (message.type === "result") break; } }
  catch (error) { failure = error; }
  await query.close();
  fs.writeFileSync(path.join(root, "native-messages.json"), JSON.stringify({
    messages, error: failure ? String(failure) : null }));
  const events = fs.readFileSync(path.join(root, "events.jsonl"), "utf8").trim().split("\n").map(JSON.parse);
  const post = events.find(event => event.name === "tool.after");
  assert(post, "the admitted Post route did not execute");
  assert.equal(post.native.hook_event_name, toolFailure ? "PostToolUseFailure" : "PostToolUse");
  const nativeId = post.native.aw_native_tool_use_id ?? post.native.tool_use_id;
  const delivered = messages.flatMap(message => message.type === "user" ? message.message.content : [])
    .find(block => block.type === "tool_result" && block.tool_use_id === nativeId);
  const assistant = modelText(messages);
  const modelEchoedLateMarker = assistant.includes(lateMarker);
  assert.equal(finalized, observed.length, "original callback finally remains pending");
  if (final) {
    assert.ifError(failure);
    assert(messages.some(message => message.type === "result" && !message.is_error), "hosted inference did not complete");
    const checked = events.filter(event => event.name === "security.violation");
    assert(checked.length > 0, "final Provider was not invoked");
    if (toolFailure) assert.equal(checked[0].native.tool_response.isError, true);
    const original = JSON.stringify(toolFailure ? post.native.error : post.native.tool_response);
    assert(original.includes(sentinel));
    assert(!original.includes(lateMarker));
    assert(JSON.stringify(checked).includes(late ? lateMarker : sentinel));
    if (toolFailure) failedDelivery(delivered, messages, sentinel);
    assert(!modelEchoedLateMarker, "sensitive final result reached the model");
    assert(assistant.includes(finalBlocked ? "AW withheld the final tool result" : sentinel), "hosted model did not quote checked result");
    assert(fs.existsSync(marker), "final gate prevented the safe second tool");
  } else if (withheld) {
    assert(failure, "strict Post failure must reach the Query caller");
    assert.equal(delivered, undefined, "unverified tool result escaped native shutdown");
    assert.equal(fs.existsSync(marker), false, "later tool dispatch escaped the Query fence");
  } else {
    assert.ifError(failure);
    assert(delivered, "successful Post did not publish the native result");
    assert.equal(!!delivered.is_error, toolFailure);
    if (mode === "late-mutation") {
      const checked = JSON.stringify(post.native.tool_response);
      assert(checked.includes(sentinel), "AW Post missed the original tool result");
      assert(!checked.includes(lateMarker), "AW Post ran after the application rewrite");
      assert(JSON.stringify(observed).includes(sentinel), "original Post callback missed the raw result");
      assert(JSON.stringify(delivered.content).includes(lateMarker), "Worker did not adopt late Post output");
      assert(!JSON.stringify(delivered.content).includes(sentinel), "Worker retained original output");
      assert(modelEchoedLateMarker, "hosted model did not report the adopted late output");
    } else if (mode === "replace") {
      assert(JSON.stringify(delivered.content).includes("AW_SAFE_POST_RESULT"), "replacement was not adopted");
      assert(!JSON.stringify(delivered.content).includes(sentinel), "original result escaped replacement");
    } else assert(JSON.stringify(delivered.content).includes(sentinel), "native result content was changed");
    assert(fs.existsSync(marker), "report/success prevented later tool execution");
  }
  fs.writeFileSync(path.join(root, "results.json"), JSON.stringify({ case: mode, status: "passed",
    sdk: process.env.QODER_TEST_SDK_VERSION, worker: process.env.QODER_TEST_WORKER_VERSION,
    final_gate: final, final_blocked: finalBlocked,
    model_withheld: assistant.includes("AW withheld the final tool result"),
    final_checks: events.filter(event => event.name === "security.violation").length,
    post: post.native.hook_event_name, model_policy_calls: models.length,
    delivered: !!delivered, delivered_is_error: delivered?.is_error === true,
    model_echoed_checked_result: assistant.includes(sentinel), finalized, later_tool: fs.existsSync(marker),
    late_mutation_adopted: mode === "late-mutation" && JSON.stringify(delivered?.content).includes(lateMarker),
    model_echoed_late_marker: mode === "late-mutation" && modelEchoedLateMarker }));
} finally {
  clearTimeout(timer);
  await query?.close();
  await daemon?.stop();
  assert.equal(children().length, 0, "owned SDK daemon remains after stop");
}
