import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import Module, { createRequire } from "node:module";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";
import { promisify } from "node:util";
import { ProfileSource, functionCodeSha256 } from "../src/profile-source.mjs";
import {
  appFixture,
  installProfile,
  policyFixture,
  profile,
  target,
} from "./profile-fixture.mjs";

const execute = promisify(execFile);

test("probes package installation and verifies fingerprints without loading CJS", async (t) => {
  const sourceText = "function f(x) {\r\n  return x + 1;\r\n}";
  const { root, entry } = appFixture(t, {
    "main.cjs": `exports.f = ${sourceText};`,
  });
  const require = createRequire(entry);
  const expected = createHash("sha256")
    .update(sourceText.replaceAll("\r\n", "\n"))
    .digest("hex");
  const document = profile([target("f", { code_sha256: expected })], {
    origin: "custom",
  });
  const source = await ProfileSource.probe(document, { resolveFrom: root });
  assert.equal(source.packageVersion, "1.5.2");
  assert.equal(require.cache[require.resolve("profile-fixture")], undefined);
  const { f } = require("profile-fixture");
  assert.equal(functionCodeSha256(f), expected);
  assert.throws(() => functionCodeSha256(Math.abs), {
    code: "source_probe_fingerprint_mismatch",
  });
  await assert.rejects(
    ProfileSource.probe(
      profile([target("f", { code_sha256: "0".repeat(64) })]),
      { resolveFrom: entry },
    ),
    { code: "source_probe_fingerprint_mismatch" },
  );
  await assert.rejects(
    ProfileSource.probe(
      profile([target("f")], {
        package: { name: "profile-fixture", versions: "==1.5.3" },
      }),
      { resolveFrom: entry },
    ),
    { code: "source_probe_package_mismatch" },
  );
  await assert.rejects(
    ProfileSource.probe(
      profile([target("f", { module: "profile-missing" })], {
        package: { name: "profile-missing", versions: ">=1" },
      }),
      { resolveFrom: entry },
    ),
    { code: "source_probe_package_missing" },
  );
});

test("wraps cached CJS methods, lazy exports, and restores descriptors and the loader", async (t) => {
  const { entry } = appFixture(
    t,
    {
      "main.cjs":
        "exports.Agent = class { method(a, b) { return this.value + a + b } static run(x) { return x } };",
      "more.cjs": "module.exports = function echo(value) { return value; };",
    },
    { ".": "./main.cjs", "./more": "./more.cjs" },
  );
  const require = createRequire(entry);
  const { Agent } = require("profile-fixture");
  const original = Object.getOwnPropertyDescriptor(Agent.prototype, "method");
  const loader = Module._load;
  const source = await installProfile(
    t,
    profile([
      target("Agent.prototype.method"),
      target("Agent.run"),
      target("default", { module: "profile-fixture/more" }),
    ]),
    { resolveFrom: entry },
  );
  const cached = Agent.prototype.method;
  const staticMethod = Agent.run;
  const lazy = require("profile-fixture/more");
  assert.notEqual(cached, original.value);
  assert.equal(functionCodeSha256(cached), functionCodeSha256(original.value));
  assert.equal(cached.length, 2);
  assert.equal(cached.name, "method");
  assert.equal(cached.call({ value: 1 }, 2, 3), 6);
  assert.equal(staticMethod(7), 7);
  assert.equal(lazy(7), 7);
  await source.teardown();
  assert.equal(Module._load, loader);
  assert.deepEqual(
    Object.getOwnPropertyDescriptor(Agent.prototype, "method"),
    original,
  );
  for (const wrapped of [cached, staticMethod, lazy])
    assert.throws(() => wrapped(), { code: "source_unavailable" });
});

for (const availability of ["required", "native_fallback"])
  test(`a lazy fingerprint mismatch poisons ${availability} cached wrappers`, async (t) => {
    const { entry } = appFixture(
      t,
      {
        "main.cjs": "exports.f = function f() { return 1 };",
        "more.cjs": "exports.g = function g() { return 2 };",
      },
      { ".": "./main.cjs", "./more": "./more.cjs" },
    );
    const require = createRequire(entry);
    const source = await installProfile(
      t,
      profile([
        target("f"),
        target("g", {
          module: "profile-fixture/more",
          code_sha256: "0".repeat(64),
        }),
      ]),
      { resolveFrom: entry, availability },
    );
    const { f } = require("profile-fixture");
    assert.equal(f(), 1);
    assert.throws(
      () => require("profile-fixture/more"),
      (error) =>
        error.code === "source_unavailable" &&
        error.cause?.code === "source_probe_fingerprint_mismatch",
    );
    assert.throws(f, { code: "source_unavailable" });
    assert.equal(source.state, "poisoned");
    await source.teardown();
  });

test("verify refuses a previously imported ESM named binding and restores partial patches", async (t) => {
  const { packageRoot, entry } = appFixture(
    t,
    {
      "main.mjs":
        "export class A { f(x) { return x } }; export const g = () => 2;",
    },
    { ".": { import: "./main.mjs" } },
  );
  const { A, g } = await import(pathToFileURL(join(packageRoot, "main.mjs")));
  const original = A.prototype.f;
  await assert.rejects(
    ProfileSource.probe(
      profile([
        target("g", {
          code_sha256: "0".repeat(64),
        }),
      ]),
      { resolveFrom: entry },
    ),
    { code: "source_probe_fingerprint_mismatch" },
  );
  const source = await ProfileSource.probe(
    profile([
      target("A.prototype.f"),
      target("g", { code_sha256: functionCodeSha256(g) }),
    ]),
    { resolveFrom: entry },
  );
  const loader = Module._load;
  source.install();
  await assert.rejects(source.verify(), {
    code: "source_verify_unwritable_export",
  });
  assert.equal(A.prototype.f, original);
  assert.equal(Module._load, loader);
  assert.equal(source.state, "closed");
});

test("NODE_OPTIONS preloads ESM export binding and class methods, restores live imports, and writes no stdout", async (t) => {
  const { root, entry } = appFixture(
    t,
    {
      "main.mjs": `export function run(x) { return x + 1; }
      export class Agent { go(x) { return this.value + x; } static stat(x) { return x; } }`,
    },
    "./main.mjs",
  );
  const bootstrap = join(root, "boot.json");
  const sdk = fileURLToPath(new URL("../", import.meta.url));
  const pkgDir = join(root, "node_modules", "@anolisa", "aw-sdk");
  // Exercise the public exports before the package build creates dist/.
  mkdirSync(pkgDir, { recursive: true });
  copyFileSync(join(sdk, "package.json"), join(pkgDir, "package.json"));
  symlinkSync(join(sdk, "src"), join(pkgDir, "dist"), "dir");
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "observe", budget_ms: 1000 },
  ]);
  writeFileSync(
    bootstrap,
    JSON.stringify({
      profile: profile([
        target("run"),
        target("Agent.prototype.go"),
        target("Agent.stat"),
      ]),
      target: "fixture",
      endpoint: service.endpoint,
    }),
    { mode: 0o600 },
  );
  writeFileSync(
    entry,
    `
    import assert from "node:assert/strict";
    import { execFileSync } from "node:child_process";
    import * as named from "profile-fixture";
    import { run, Agent } from "profile-fixture";
    assert.equal(Object.hasOwn(named, "default"), false);
    import { preloadedProfileSource as source } from "@anolisa/aw-sdk/profile-preload";
    assert.equal(source.activation_epoch, 1);
    const child = execFileSync(process.execPath, ["-e", "process.stdout.write(JSON.stringify([process.env.AW_PROFILE_BOOTSTRAP, process.env.NODE_OPTIONS, globalThis.fromApp]))"], { cwd: "/" });
    assert.deepEqual(JSON.parse(child), [null, process.env.NODE_OPTIONS, "loaded"]);
    assert.equal(process.env.AW_PROFILE_BOOTSTRAP, undefined);
    assert.equal(process.env.NODE_OPTIONS, '--require "' + process.cwd() + '/app preload.cjs" --no-warnings');
    assert(!process.execArgv.some((arg) => arg.includes("@anolisa")));
    const cached = run;
    const method = Agent.prototype.go;
    assert.equal(run(2), 3);
    assert.equal(Agent.stat(3), 3);
    assert.equal(method.call({ value: 4 }, 5), 9);
    await source.teardown();
    assert.notEqual(run, cached);
    assert.notEqual(Agent.prototype.go, method);
    assert.equal(run(2), 3);
    assert.throws(() => cached(2), { code: "source_unavailable" });
    assert.throws(() => method(), { code: "source_unavailable" });
  `,
  );
  const appPreload = join(root, "app preload.cjs");
  writeFileSync(appPreload, 'globalThis.fromApp = "loaded";');
  const env = {
    ...process.env,
    NODE_OPTIONS: `--require "${appPreload}" --import "@anolisa/aw-sdk/profile-preload" --no-warnings`,
    AW_PROFILE_BOOTSTRAP: bootstrap,
  };
  const run = () =>
    execute(
      process.execPath,
      ["--import=@anolisa/aw-sdk/profile-preload", entry],
      { cwd: root, env, timeout: 10000 },
    );
  const result = await run();
  assert.equal(result.stdout, "");
  assert.equal(result.stderr, "");
  assert.deepEqual(
    service.events().map(({ event }) => event),
    ["session.start", "session.start", "session.start"],
  );
  chmodSync(bootstrap, 0o644);
  await assert.rejects(
    run(),
    (error) =>
      error.stdout === "" &&
      /source_profile_bootstrap_invalid/.test(error.stderr),
  );
  chmodSync(bootstrap, 0o400);
  const alias = join(root, "alias");
  symlinkSync(bootstrap, alias);
  env.AW_PROFILE_BOOTSTRAP = alias;
  await assert.rejects(
    run(),
    (error) =>
      error.stdout === "" &&
      /source_profile_bootstrap_invalid/.test(error.stderr),
  );
  const directory = join(root, "parent-link");
  symlinkSync(root, directory);
  env.AW_PROFILE_BOOTSTRAP = join(directory, "boot.json");
  await assert.rejects(
    run(),
    (error) =>
      error.stdout === "" &&
      /source_profile_bootstrap_invalid/.test(error.stderr),
  );
  delete env.AW_PROFILE_BOOTSTRAP;
  writeFileSync(
    entry,
    `import { preloadedProfileSource } from "@anolisa/aw-sdk/profile-preload";
    if (preloadedProfileSource !== undefined) process.exitCode = 2;`,
  );
  const inert = await run();
  assert.equal(inert.stdout, "");
  assert.equal(inert.stderr, "");
});

test("ESM aliases share one module and guarded named exports follow live changes", async (t) => {
  const { packageRoot, entry } = appFixture(
    t,
    {
      "main.mjs": `let n = 0;
      export let increment = () => ++n;
      export function change() { increment = () => ++n * 10; label = "changed"; }
      export function value() { return n; }
      let label = "kept"; export { label as default };`,
    },
    { ".": "./main.mjs", "./alias": "./main.mjs" },
  );
  const source = await installProfile(
    t,
    profile([
      target("increment"),
      target("value", { module: "profile-fixture/alias" }),
    ]),
    { resolveFrom: entry },
  );
  const sdk = await import(pathToFileURL(join(packageRoot, "main.mjs")));
  assert.equal(sdk.default, "kept");
  assert.equal(sdk.increment(), 1);
  sdk.change();
  assert.equal(sdk.default, "changed");
  assert.equal(sdk.increment(), 20);
  const { increment, value } = sdk;
  assert.equal(value(), 2);
  await source.teardown();
  assert.equal(sdk.increment(), 30);
  assert.equal(sdk.value(), 3);
  assert.throws(increment, { code: "source_unavailable" });
  assert.throws(value, { code: "source_unavailable" });
});

test("restored named and default functions call the latest native binding", async (t) => {
  const { entry, packageRoot } = appFixture(
    t,
    {
      "main.mjs": `export let named = function () { return this.value + 1; };
      let Default = () => 1;
      export { Default as default };
      export function update() {
        named = function () { return this.value + 2; };
        Default = class Two { value = 2; };
      }`,
    },
    "./main.mjs",
  );
  const source = await installProfile(
    t,
    profile([target("named"), target("default")]),
    { resolveFrom: entry },
  );
  const url = pathToFileURL(join(packageRoot, "main.mjs"));
  const facade = await import(url);
  const cached = { named: facade.named, Default: facade.default };
  assert.equal(cached.named.call({ value: 10 }), 11);
  assert.equal(cached.Default(), 1);
  await source.teardown();
  facade.update();
  const native = await import(url);
  assert.equal(
    facade.named.call({ value: 10 }),
    native.named.call({ value: 10 }),
  );
  assert.equal(new facade.default().constructor, native.default);
  assert.equal(facade.default.prototype, native.default.prototype);
  assert.throws(() => cached.named(), { code: "source_unavailable" });
  assert.throws(() => cached.Default(), { code: "source_unavailable" });
});

test("active and restored class exports keep native static fields and method receivers", async (t) => {
  const { root, entry, packageRoot } = appFixture(
    t,
    {
      "main.mjs": `export default class Client {
      constructor() { this.direct = new.target === Client; }
      static region = "old";
      static #revision = 2;
      static revision() { return this.#revision; }
      static receiver() { return this; }
    }`,
    },
    "./main.mjs",
  );
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "record", budget_ms: 1000 },
  ]);
  const source = await installProfile(t, profile([target("default")]), {
    resolveFrom: entry,
  });
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  const url = pathToFileURL(join(packageRoot, "main.mjs"));
  const facade = await import(url);
  assert.equal(facade.default.revision(), 2);
  assert.equal(new facade.default().direct, true);
  class ActiveChild extends facade.default {}
  assert.equal(new ActiveChild().direct, false);
  assert.equal(service.events().length, 2);
  await source.teardown();
  const Client = facade.default;
  const { default: Native } = await import(url);

  Client.region = "new";
  assert.equal(Client.region, "new");
  assert.equal(Native.region, "new");
  assert.equal(Client.revision(), 2);
  const receiver = Client.receiver;
  assert.equal(Client.receiver, receiver);
  assert.equal(Client.receiver(), Native);
  assert.equal(receiver(), undefined);
  const other = {};
  assert.equal(Client.receiver.call(other), other);
  assert.throws(() => Client.revision.call(other), TypeError);
  class Child extends Client {}
  assert.equal(Child.receiver(), Child);
  Child.region = "child";
  assert.equal(Child.region, "child");
  assert.equal(Native.region, "new");
});

test("forwarding an unrelated ESM default respects its JavaScript export name", async (t) => {
  for (const [alias, hasDefault] of [
    ["default", true],
    [String.raw`def\u0061ult`, true],
    [String.raw`def\u{61}ult`, true],
    [String.raw`"def\u0061ult"`, true],
    [String.raw`"def\\u0061ult"`, false],
  ]) {
    const { entry, packageRoot } = appFixture(
      t,
      {
        "main.mjs": `let value = 1; export { value as ${alias} };
        export function update() { value++; }`,
      },
      "./main.mjs",
    );
    const source = await installProfile(t, profile([target("update")]), {
      resolveFrom: entry,
    });
    const ns = await import(pathToFileURL(join(packageRoot, "main.mjs")));
    assert.equal(Object.hasOwn(ns, "default"), hasDefault, alias);
    const key = hasDefault ? "default" : String.raw`def\u0061ult`;
    assert.equal(ns[key], 1, alias);
    ns.update();
    assert.equal(ns[key], 2, alias);
    await source.teardown();
  }
});

test("CJS assignments keep named, static and prototype methods guarded", async (t) => {
  const { root, entry } = appFixture(t, {
    "main.cjs": `exports.f = () => 1;
      exports.Agent = class { static f() { return 1; } f() { return 1; } };`,
  });
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "record", budget_ms: 1000 },
  ]);
  const source = await installProfile(
    t,
    profile(["f", "Agent.f", "Agent.prototype.f"].map((name) => target(name))),
    { resolveFrom: entry },
  );
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  const require = createRequire(entry);
  const sdk = require("profile-fixture");
  const replacement = () => 2;
  const nativeInstance = new sdk.Agent();
  const owners = [sdk, sdk.Agent, sdk.Agent.prototype, nativeInstance];
  try {
    for (const owner of owners) {
      owner.f = replacement;
      assert.notEqual(owner.f, replacement);
      assert.equal(owner.f(), 2);
    }
    assert.equal(require("profile-fixture"), sdk);
    assert.equal(service.events().length, owners.length);
  } finally {
    await source.teardown();
  }
  for (const owner of owners) assert.equal(owner.f, replacement);
});

test("package-internal ESM cycles retain hoisted bindings", async (t) => {
  const { packageRoot, entry } = appFixture(
    t,
    {
      "main.mjs": `export { initial } from "./dep.mjs"; export function f() { return 5; }`,
      "dep.mjs": `import { f } from "./main.mjs"; export const initial = f();`,
    },
    "./main.mjs",
  );
  const source = await installProfile(t, profile([target("f")]), {
    resolveFrom: entry,
  });
  const { f, initial } = await import(
    pathToFileURL(join(packageRoot, "main.mjs"))
  );
  assert.equal(initial, 5);
  assert.equal(f(), 5);
  await source.teardown();
  assert.throws(f, { code: "source_unavailable" });
});

test("assigning a bound prototype guard dispatches once and restores its bound native method", async (t) => {
  const { root, entry } = appFixture(t, {
    "main.cjs": `exports.Agent = class {
      #tag = "agent";
      constructor() { this.read = this.read.bind(this, "first"); }
      read(...args) { return [this.#tag, ...args]; }
    };`,
  });
  const { Agent } = createRequire(entry)("profile-fixture");
  const document = profile([target("Agent.prototype.read")]);
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "record", budget_ms: 1000 },
  ]);
  const source = await installProfile(t, document, { resolveFrom: entry });
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  const instance = new Agent();
  const first = instance.read;
  instance.read = first.bind({}, 42);
  instance.read = instance.read.bind(null, "third");
  const cached = instance.read;
  const expected = ["agent", "first", 42, "third", "last"];
  assert.deepEqual(cached("last"), expected);
  assert.equal(service.events().length, 1);
  await source.teardown();
  assert.deepEqual(instance.read.call({}, "last"), expected);
  assert.equal(
    Object.getOwnPropertyDescriptor(instance, "read").value,
    instance.read,
  );
  assert.throws(() => first("last"), { code: "source_unavailable" });
  assert.throws(() => cached("last"), { code: "source_unavailable" });
});

test("an active live class binding forwards the replacement prototype and new.target", async (t) => {
  const { root, entry, packageRoot } = appFixture(
    t,
    {
      "main.mjs": `export let Client = class Original { static version = 3; static old = true; };
      export const current = () => Client;
      export function replace() {
        Client = class Current { static version = 4; static next = true; constructor() { this.target = new.target; } };
        return Client;
      }`,
    },
    "./main.mjs",
  );
  const service = await policyFixture(t, root, [
    { event: "session.start", step: "record", budget_ms: 1000 },
  ]);
  const source = await installProfile(t, profile([target("Client")]), {
    resolveFrom: entry,
  });
  await source.activate({ endpoint: service.endpoint, target: "fixture" });
  const ns = await import(pathToFileURL(join(packageRoot, "main.mjs")));
  const cached = ns.Client;
  function checkFields(Native) {
    assert.equal(cached.version, Native.version);
    assert.deepEqual(Reflect.ownKeys(cached), Reflect.ownKeys(Native));
    assert.deepEqual(Object.keys(cached), Object.keys(Native));
    for (const key of ["version", "old", "next"]) {
      assert.equal(key in cached, key in Native);
      assert.equal(Object.hasOwn(cached, key), Object.hasOwn(Native, key));
      assert.deepEqual(
        Object.getOwnPropertyDescriptor(cached, key),
        Object.getOwnPropertyDescriptor(Native, key),
      );
    }
  }
  checkFields(ns.current());
  const Current = ns.replace();
  checkFields(Current);
  assert.equal(cached.prototype, Current.prototype);
  const direct = new cached();
  assert.equal(direct.target, Current);
  assert.equal(Object.getPrototypeOf(direct), Current.prototype);
  class Child extends cached {}
  const child = new Child();
  assert.equal(child.target, Child);
  assert.equal(Object.getPrototypeOf(child), Child.prototype);
  assert.equal(Object.getPrototypeOf(Child.prototype), Current.prototype);
  assert.equal(service.events().length, 2);
  await source.teardown();
  assert.equal(new ns.Client().target, Current);
  assert.throws(() => new cached(), { code: "source_unavailable" });
});

test("instance patches allow discarded owners to be collected before teardown", async (t) => {
  const { entry } = appFixture(t, {
    "main.cjs": `exports.Agent = class {
      constructor() { this.read = this.read.bind(this); }
      read() { return this; }
    };`,
  });
  const script = `
    import assert from "node:assert/strict";
    import { createRequire } from "node:module";
    import { setImmediate } from "node:timers/promises";
    import { ProfileSource } from ${JSON.stringify(new URL("../src/profile-source.mjs", import.meta.url).href)};
    const entry = process.argv[1];
    const source = await ProfileSource.probe(JSON.parse(process.argv[2]), { resolveFrom: entry });
    source.install();
    await source.verify();
    const { Agent } = createRequire(entry)("profile-fixture");
    const live = new Agent();
    const refs = Array.from({ length: 16 }, () => new WeakRef(new Agent()));
    for (let i = 0; i < 30; i++) {
      await setImmediate();
      globalThis.gc();
      if (refs.every(ref => !ref.deref())) break;
    }
    assert.ok(refs.every(ref => !ref.deref()));
    await source.teardown();
    assert.equal(live.read.call(null), live);
  `;
  await execute(
    process.execPath,
    [
      "--expose-gc",
      "--input-type=module",
      "-e",
      script,
      entry,
      JSON.stringify(profile([target("Agent.prototype.read")])),
    ],
    { timeout: 15000 },
  );
});
