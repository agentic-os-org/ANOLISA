import assert from "node:assert/strict";
import test from "node:test";
import { forwardCodexResponses, gatewayArgs, reviewedHookTrust } from "../src/codex-gateway.mjs";

test("each gateway dispatch scrubs the complete AW result and its replay", () => {
  const item = { type: "function_call_output", call_id: "a", output: "left credential [MASK] right" };
  const request = { input: [{ type: "message", content: [] }, item, { ...item, type: "custom_tool_call_output" }] };
  for (let attempt = 0; attempt < 2; attempt++) {
    const sent = forwardCodexResponses(request, value => value.replace("credential", "[redacted]"),
      (payload, counts) => {
        assert.deepEqual(counts, { scanned: 2, changed: 2 });
        return payload;
      });
    assert.equal(sent.input[1].output, "left [redacted] [MASK] right");
    assert.equal(sent.input[2].output, sent.input[1].output);
    assert.equal(item.output, "left credential [MASK] right");
  }
});

test("malformed, opaque or unavailable result scrubbing stops the entire send", () => {
  const good = { type: "function_call_output", output: "value" };
  let sent = 0;
  for (const [request, scrub] of [
    [{ input: [good], previous_response_id: "old" }, String],
    [{ input: [good], conversation: {} }, String],
    [{ input: [good, { ...good, output: [] }] }, String],
    [{ input: [good, { ...good, duplicate: "value" }] }, String],
    [{ input: [good, { type: "future_result" }] }, String],
    [{ input: [good] }, () => { throw new Error("sensitive error"); }],
    [{ input: [good] }, () => null],
    [{ input: [good] }, undefined],
  ]) assert.throws(() => forwardCodexResponses(request, scrub, () => sent++), /Codex/);
  assert.equal(sent, 0);
});

test("new home uses approved hashes and a literal local gateway", () => {
  const pins = Object.fromEntries(["user_prompt_submit", "pre_tool_use", "post_tool_use"].map(
    key => [key, "sha256:" + "a".repeat(64)]));
  const settings = reviewedHookTrust('/new/"home', pins);
  assert.equal(settings.split("trusted_hash").length, 4);
  assert.ok(settings.includes(JSON.stringify('/new/"home/hooks.json:post_tool_use:0:0')));
  assert.throws(() => reviewedHookTrust("/new", {}));
  assert.throws(() => reviewedHookTrust("relative", pins));
  const options = { provider: "gateway", name: "Local Gateway", envKey: "LOCAL_CODEX_GATEWAY_KEY" };
  assert.ok(gatewayArgs(9090, options).includes('model_providers.gateway.base_url="http://127.0.0.1:9090/v1"'));
  for (const port of [0, 65536, "8080", 1.5]) assert.throws(() => gatewayArgs(port, options));
});

test("gateway settings bind one provider and admit only credential environment names", () => {
  const options = { provider: "local-v2_test", name: "Local \\ 网关", envKey: "ROUTE_2_KEY" };
  const prefix = "model_providers.local-v2_test";
  assert.deepEqual(gatewayArgs(1234, options), [
    "-c", `${prefix}.name=${JSON.stringify(options.name)}`,
    "-c", `${prefix}.base_url="http://127.0.0.1:1234/v1"`,
    "-c", `${prefix}.env_key="ROUTE_2_KEY"`,
    "-c", `${prefix}.wire_api="responses"`,
    "-c", `${prefix}.supports_websockets=false`,
  ]);
  assert.doesNotThrow(() => gatewayArgs(1, { ...options, provider: "a".repeat(64), envKey: "A".repeat(64) }));
  const invalid = {
    provider: ["", "A", "0gateway", "a.b", "a/b", "a".repeat(65), "a\n"],
    name: ["", '"quoted"', "one'two", "one\ntwo", "\u0085", "\ud800"],
    envKey: ["", "lower", "A-B", "A".repeat(65), "_KEY", "A\n", "HOME", "PATH",
      "LD_PRELOAD", "CODEX_HOME", "AW_SOCKET", "PYTHONPATH", "NODE_OPTIONS", "HTTPS_PROXY"],
  };
  for (const [field, values] of Object.entries(invalid)) {
    for (const value of values)
      assert.throws(() => gatewayArgs(1234, { ...options, [field]: value }), /Invalid Codex gateway/);
  }
});
