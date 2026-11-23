import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import test from "node:test";
import { delay, fixture } from "./qoder-bridge-fixture.mjs";

for (const required of [true, false]) {
  test(`unconfigured SessionStart keeps late tool attachment (required=${required})`, { timeout: 15000 }, async (t) => {
    const f = await fixture(required);
    t.after(() => f.close());
    f.configuration.spec.events["session.start"] = { enabled: false, required: false, steps: [] };
    await writeFile(f.configFile, JSON.stringify(f.configuration));
    const descriptorFile = await f.descriptor();
    const descriptor = JSON.parse(await readFile(descriptorFile));
    assert.equal(descriptor.observe_session_start, undefined);
    const wiring = required ? {} : { descriptorFile, executable: "/missing/aw" };
    const bridge = await f.bridge(wiring);
    let nativeStarts = 0;
    const options = { hooks: { SessionStart: [{ hooks: [async () => { nativeStarts++; }] }] } };
    const iterator = bridge.query({ prompt: "tool", options })[Symbol.asyncIterator]();
    assert.equal(f.options[0].hooks.SessionStart.length, 1);
    assert.equal((await iterator.next()).value.type, "model");
    assert.equal(nativeStarts, 1);
    assert.equal(bridge.state, "detached");
    await writeFile(f.root + "/deny", "");
    await f.attach();
    assert((await iterator.next()).done);
    assert.deepEqual(f.calls, ["model", "denied"]);
  });
}

for (const useDescriptor of [false, true]) {
  test(`configured SessionStart reaches AW before the model (descriptor=${useDescriptor})`, { timeout: 15000 }, async (t) => {
    const f = await fixture();
    t.after(() => f.close());
    const marker = f.root + "/started";
    f.configuration.spec.providers.policy.config.start_file = marker;
    f.configuration.spec.events["session.start"] = {
      enabled: true,
      required: true,
      steps: [{ id: "start", provider: "policy", operation: "start", effects: ["observe"], on_error: "report" }],
    };
    await writeFile(f.configFile, JSON.stringify(f.configuration));
    const descriptorFile = await f.descriptor();
    assert.equal(JSON.parse(await readFile(descriptorFile)).observe_session_start, true);
    const wiring = useDescriptor ? { descriptorFile, executable: "/missing/aw" } : {};
    const bridge = await f.bridge({ ...wiring, attachTimeoutMs: 6000 });
    const iterator = bridge.query({ prompt: "text" })[Symbol.asyncIterator]();
    assert.equal(f.options[0].hooks.SessionStart.length, 1);
    const first = iterator.next();
    first.catch(() => {});
    await delay(30);
    assert.deepEqual(f.calls, []);
    await f.attach();
    assert.equal((await first).value.type, "model");
    assert.equal(await readFile(marker, "utf8"), "session");
    assert((await iterator.next()).done);
  });
}
