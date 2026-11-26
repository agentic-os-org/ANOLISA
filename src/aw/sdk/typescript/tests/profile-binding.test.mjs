import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { join } from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";
import {
  appFixture,
  field,
  hook,
  installProfile,
  policyFixture,
  profile,
  target,
} from "./profile-fixture.mjs";

test("CJS owner reassignment guards new methods before publishing the owner", async (t) => {
  const { root, entry } = appFixture(t, {
    "main.cjs": `exports.Agent = class { f() { return 1; } g() { return 2; } };
      exports.update = () => exports.Agent = class { f() { return 3; } g() { return 4; } };`,
  });
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "record", budget_ms: 1000 },
  ]);
  const source = await installProfile(
    t,
    profile(
      ["Agent.prototype.f", "Agent.prototype.g"].map((name) => target(name)),
    ),
    { resolveFrom: entry },
  );
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  const sdk = createRequire(entry)("profile-fixture");
  const old = new sdk.Agent();
  sdk.update();
  const active = new sdk.Agent();
  assert.deepEqual([old.f(), active.f(), active.g()], [1, 3, 4]);
  assert.equal(service.events().length, 3);
  const cached = active.f;
  await source.teardown();
  assert.equal(active.f(), 3);
  assert.equal(new sdk.Agent().g(), 4);
  assert.throws(cached, { code: "source_unavailable" });
});

test("a rejected owner replacement restores the candidate's original descriptors immediately", async (t) => {
  const { entry } = appFixture(t, {
    "main.cjs": "exports.Agent = class { f() {} g() {} };",
  });
  const source = await installProfile(
    t,
    profile(
      ["Agent.prototype.f", "Agent.prototype.g"].map((name) => target(name)),
    ),
    { resolveFrom: entry },
  );
  const sdk = createRequire(entry)("profile-fixture");
  const original = sdk.Agent;
  class Candidate {
    f() {
      return 42;
    }
  }
  const descriptors = Object.getOwnPropertyDescriptors(Candidate.prototype);
  assert.throws(
    () => {
      sdk.Agent = Candidate;
    },
    { code: "source_unavailable" },
  );
  assert.equal(sdk.Agent, original);
  assert.deepEqual(
    Object.getOwnPropertyDescriptors(Candidate.prototype),
    descriptors,
  );
  assert.equal(new Candidate().f(), 42);
  await source.teardown();
  assert.deepEqual(
    Object.getOwnPropertyDescriptors(Candidate.prototype),
    descriptors,
  );
});

test("borrowed intrinsic bind is rejected before publishing an opaque replacement", async (t) => {
  const { entry } = appFixture(t, { "main.cjs": "exports.f = () => 1;" });
  const source = await installProfile(t, profile([target("f")]), {
    resolveFrom: entry,
  });
  const sdk = createRequire(entry)("profile-fixture");
  const previous = sdk.f;
  assert.throws(
    () => {
      sdk.f = Function.prototype.bind.call(previous, null);
    },
    (error) => error.cause?.code === "source_verify_opaque_replacement",
  );
  assert.equal(sdk.f, previous);
  await source.teardown();
  assert.equal(sdk.f(), 1);
});

test("a bound ESM replacement dispatches only the destination CJS target", async (t) => {
  const { root, entry, packageRoot } = appFixture(
    t,
    {
      "main.cjs": "exports.f = () => 1;",
      "api.mjs":
        "export let g = () => 2; export function change() { g = function(input, label, caller) { return [this.session, input.command, label, caller.command]; } }",
    },
    { ".": "./main.cjs", "./api": "./api.mjs" },
  );
  const service = await policyFixture(t, root, [
    {
      event: "tool.before",
      step: "before",
      replace_input: true,
      budget_ms: 1000,
    },
  ]);
  const options = {
    hooks: { "tool.before": hook(["replace_input"]) },
    fields: {
      tool_name: { const: "bound-tool" },
      session_id: field("receiver.session"),
      input: field("arg.0", "json"),
    },
  };
  const source = await installProfile(
    t,
    profile([
      target("f", options),
      target("g", { ...options, module: "profile-fixture/api" }),
    ]),
    { resolveFrom: entry },
  );
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  const sdk = createRequire(entry)("profile-fixture");
  const api = await import(pathToFileURL(join(packageRoot, "api.mjs")));
  api.change();
  const first = { command: "raw:bound" };
  const caller = { command: "raw:caller" };
  const candidate = api.g
    .bind({ session: "bound-session" }, first)
    .bind({ session: "unused" }, "second");
  sdk.f = candidate;
  assert.deepEqual(sdk.f(caller), [
    "bound-session",
    "safe:bound",
    "second",
    "raw:caller",
  ]);
  const events = service.events();
  assert.equal(events.length, 1);
  assert.deepEqual(events[0].input, { command: "raw:bound" });
  assert.equal(events[0].session_id, "bound-session");
  assert.equal(first.command, "raw:bound");
  const cached = sdk.f;
  await source.teardown();
  assert.deepEqual(sdk.f(caller), [
    "bound-session",
    "raw:bound",
    "second",
    "raw:caller",
  ]);
  assert.throws(cached, { code: "source_unavailable" });
  assert.throws(candidate, { code: "source_unavailable" });
});

test("an inherited guard respects existing receiver data descriptors", async (t) => {
  const { entry } = appFixture(t, {
    "main.cjs": "exports.A = class { f() {} };",
  });
  const source = await installProfile(t, profile([target("A.prototype.f")]), {
    resolveFrom: entry,
  });
  const { A } = createRequire(entry)("profile-fixture");
  const original = () => 1;
  const replacement = () => 2;
  for (const writable of [false, true]) {
    const instance = new A();
    Object.defineProperty(instance, "f", {
      value: original,
      configurable: true,
      writable,
    });
    Reflect.set(A.prototype, "f", replacement, instance);
    assert.equal(instance.f(), writable ? 2 : 1);
    assert.equal(
      Object.getOwnPropertyDescriptor(instance, "f").enumerable,
      false,
    );
    if (!writable)
      assert.equal(
        Object.getOwnPropertyDescriptor(instance, "f").value,
        original,
      );
  }
  await source.teardown();
});

test("frozen CJS classes keep private static receivers and ESM arrows keep their call shape", async (t) => {
  const { root, entry, packageRoot } = appFixture(
    t,
    {
      "main.cjs":
        "module.exports = Object.freeze(class C { static #v = 3; static value() { return this.#v; } });",
      "api.mjs": "export const f = () => 17;",
    },
    { ".": "./main.cjs", "./api": "./api.mjs" },
  );
  const source = await installProfile(
    t,
    profile([
      target("default"),
      target("f", { module: "profile-fixture/api" }),
    ]),
    { resolveFrom: entry },
  );
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "record", budget_ms: 1000 },
  ]);
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  const C = createRequire(entry)("profile-fixture");
  assert.equal(C.value(), 3);
  assert.throws(() => C.value.call({}), TypeError);
  const { f } = await import(pathToFileURL(join(packageRoot, "api.mjs")));
  assert.equal(f(), 17);
  assert.throws(() => Reflect.construct(Object, [], f), TypeError);
  assert.throws(() => new f(), TypeError);
  await source.teardown();
});

test("a locked static method retains its native receiver on an extensible CJS class", async (t) => {
  const { entry } = appFixture(t, {
    "main.cjs": `class Client {
    static #version = 3;
    static [Symbol.for("version")]() { return this.#version; }
  }
  Object.defineProperty(Client, Symbol.for("version"), {configurable:false,writable:false});
  module.exports = Client;`,
  });
  const source = await installProfile(t, profile([target("default")]), {
    resolveFrom: entry,
  });
  const Client = createRequire(entry)("profile-fixture");
  const version = Symbol.for("version");
  assert.equal(Object.isExtensible(Client), true);
  assert.equal(Client[version](), 3);
  assert.throws(() => Client[version].call({}), TypeError);
  await source.teardown();
  assert.equal(createRequire(entry)("profile-fixture")[version](), 3);
});
