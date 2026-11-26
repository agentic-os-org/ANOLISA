import assert from "node:assert/strict";
import test from "node:test";
import { evaluate, installTools } from "../../../../crates/aw-adapter/native/deepseek-harness/hooks.mjs";
import { parseDirective } from "../../../../crates/aw-adapter/native/deepseek-harness/callback.mjs";

test("only an admitted replacement can change both matching bash texts", () => {
  const hook = { event: "tool.after", step: "mask", on_error: "withhold_result" };
  const original = { isError: false, content: [{ type: "text", text: "original" }],
    value: { kind: "foreground", stdout: { text: "original" }, cwd: "/work" } };
  const payload = { tool_call: { name: "bash" }, tool_response: original, aw_result_gate: true, nested: false };
  const parse = (reply, step = "mask") => parseDirective(Buffer.from(JSON.stringify(reply)), hook, payload, step);
  const replaced = structuredClone(original);
  replaced.content[0].text = "masked";
  assert.throws(() => parse({ aw_replacement: replaced }), /metadata/);
  replaced.value.stdout.text = "masked";
  assert.deepEqual(parse({ aw_replacement: replaced }).aw_replacement, replaced);
  assert.throws(() => parse({ aw_replacement: replaced }, "other"), /unauthorized/);
  replaced.value.cwd = "/";
  assert.throws(() => parse({ aw_replacement: replaced }), /metadata/);
  original.value.stdout.text = "different";
  assert.throws(() => parse({ aw_replacement: replaced }), /projection/);
  assert.throws(() => parse({ aw_withheld: true, value: "original" }), /invalid/);
});

test("strict faults drain the remaining steps and take precedence over their replacements", async () => {
  const seen = [];
  const hooks = ["first", "last"].map(step => ({ event: "tool.after", step, on_error: "withhold_result" }));
  const outcome = await evaluate({ hooks, invoke(hook) {
    seen.push(hook.step);
    if (hook.step === "first") throw Error("transport unavailable");
    return { aw_replacement: { content: [] } };
  } }, "tool.after", {});
  assert.deepEqual(seen, ["first", "last"]);
  assert.deepEqual(outcome, { aw_withheld: true });
});

test("a Bridge cannot confirm readiness without a verified result registry", () => {
  assert.throws(() => installTools({}, () => {}, { resultGate: true }), /preloaded/);
});
