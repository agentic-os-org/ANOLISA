/** Contract tests for SDK option composition, vetoes and bounded callback cleanup. */
import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createHash } from "node:crypto";
import { pathToFileURL } from "node:url";
import * as bridge from "../../../../aw-adapter/native/qoder-sdk/bridge.mjs";
import "./resident.test.mjs";
import "./resident-policy.test.mjs";
import "./daemon.test.mjs";
import "./private.test.mjs";
import "./cleanup-fence.test.mjs";
import "./native-options.test.mjs";
const { createQuery } = bridge;

function fixture(t, mode = "allow") {
  const root = fs.mkdtempSync(path.join(process.cwd(), "sdk-bridge-"));
  t.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const sdk = path.join(root, "sdk.mjs");
  fs.writeFileSync(sdk, `import fs from 'node:fs';
  export async function startDaemon(options) {
    fs.writeFileSync(${JSON.stringify(path.join(root, "started"))}, JSON.stringify(options));
    if(options.failStart) throw new Error('daemon startup failed');
    return { options, async stop(){fs.writeFileSync(${JSON.stringify(path.join(root, "stopped"))},'stopped');} };
  }
  export function query({options}) {
    fs.writeFileSync(${JSON.stringify(path.join(root, "queried"))},'queried'); return {
    options, async initializationResult(){if(options.failInit) throw new Error('init failed');},
    async request(){return {sources:{flag:options.settings},effective:options.settings};},
    async return(){fs.writeFileSync(${JSON.stringify(path.join(root, "closed"))},'closed');}
  };}`);
  const executable = path.join(root, "callback");
  fs.writeFileSync(executable, `#!${process.execPath}
import fs from 'node:fs';
let input='';for await(const chunk of process.stdin)input+=chunk;
fs.writeFileSync(${JSON.stringify(path.join(root, "input"))},input);
fs.writeFileSync(${JSON.stringify(path.join(root, "pid"))},String(process.pid));
const mode=${JSON.stringify(mode)};
if(mode==='wait')await new Promise(r=>setTimeout(r,30000));
if(mode==='deny'){process.stdout.write(JSON.stringify('policy denied'));process.exitCode=2;}
else if(mode==='invalid')process.stdout.write('{"approval":true}');
else process.stdout.write('{}');
`, { mode: 0o700 });
  fs.writeFileSync(path.join(root, "package.json"), '{"type":"module"}');
  const config = { sdk, executable, binding: path.join(root, "binding"), cwd: process.cwd(),
    ready_path: path.join(root, "ready"), ready_token: "fixture", hooks: [
      { event: "tool.before", step: "before", on_error: "block", budget_ms: 500, replace_result: false },
      { event: "tool.after", step: "after", on_error: "report", budget_ms: 500, replace_result: false },
    ] };
  const file = path.join(root, "config.json");
  fs.writeFileSync(file, JSON.stringify(config));
  process.env.AW_QODER_SDK_CONFIG = file;
  return { root, config, file, options: { settingSources: [], cwd: process.cwd() } };
}
const payload = (event = "PreToolUse") => ({ hook_event_name: event, session_id: "session",
  tool_use_id: "call", tool_name: "Bash", tool_input: { command: "original" }, cwd: process.cwd() });
const invoke = (query, event = "PreToolUse", signal = new AbortController().signal) =>
  query.options.hooks[event].at(-1).hooks[0](payload(event), "call", { signal });

test("registers before initialization and preserves the caller options", async (t) => {
  const f = fixture(t);
  const q = await createQuery({ prompt: "first", options: f.options });
  assert.equal(fs.existsSync(path.join(f.root, "ready")), true);
  assert.equal(f.options.hooks, undefined);
  for (const event of ["PreToolUse", "PostToolUse", "PostToolUseFailure"])
    assert.deepEqual(await invoke(q, event), {});
  await q.return();
  assert(fs.existsSync(path.join(f.root, "closed")));
});
test("denial and invalid callbacks fail closed without approval", async (t) => {
  for (const mode of ["deny", "invalid"]) {
    const f = fixture(t, mode);
    const q = await createQuery({ prompt: "first", options: f.options });
    assert.equal((await invoke(q)).hookSpecificOutput.permissionDecision, "deny");
    assert.deepEqual(await invoke(q, "PostToolUse"), {});
  }
});
test("query routes remain private after another query is configured", async (t) => {
  const a = fixture(t, "deny");
  const qa = await createQuery({ prompt: "a", options: a.options });
  const b = fixture(t);
  const qb = await createQuery({ prompt: "b", options: b.options });
  assert.equal((await invoke(qa)).hookSpecificOutput.permissionDecision, "deny");
  assert.deepEqual(await invoke(qb), {});
  assert(fs.existsSync(path.join(a.root, "input")));
  assert(fs.existsSync(path.join(b.root, "input")));
});
test("native callbacks keep vetoes and cannot mutate checked arguments", async (t) => {
  const f = fixture(t);
  const options = { ...f.options, hooks: { PreToolUse: [{ hooks: [async (input) => {
    input.tool_input.command = "changed";
    return { hookSpecificOutput: { updatedInput: { command: "changed" } } };
  }] }] }, canUseTool: async () => ({ behavior: "allow", updatedInput: { command: "changed" } }) };
  const q = await createQuery({ prompt: "first", options });
  const original = payload();
  const result = await q.options.hooks.PreToolUse[0].hooks[0](original, "call", {});
  assert.equal(result.hookSpecificOutput.permissionDecision, "deny");
  assert.equal(original.tool_input.command, "original");
  assert.equal((await q.options.canUseTool("Bash", original.tool_input, {})).behavior, "deny");
  const denied = await createQuery({ prompt: "first", options: { ...f.options,
    canUseTool: async () => ({ behavior: "deny", message: "native veto" }) } });
  assert.equal((await denied.options.canUseTool("Bash", {}, {})).message, "native veto");
});
test("unsupported routing and cwd overrides fail before readiness", async (t) => {
  const f = fixture(t);
  for (const extra of [{ settingSources: undefined }, { settings: {} }, { transport: {} },
    { pathToQoderCLIExecutable: "/other" }, { cwd: "/other" }, { plugins: [{}] },
    { hooks: { PermissionRequest: [{}] } }, { extraArgs: { settings: "{}" } },
    { env: { QODER_HEADLESS_FAST_HOOKS: "1" } }]) {
    await assert.rejects(createQuery({ prompt: "first", options: { ...f.options, ...extra } }));
    assert(!fs.existsSync(path.join(f.root, "ready")));
  }
});
test("failed initialization closes the Query without reporting ready", async (t) => {
  const f = fixture(t);
  await assert.rejects(createQuery({ prompt: "first", options: { ...f.options, failInit: true } }));
  assert(!fs.existsSync(path.join(f.root, "ready")));
  assert(fs.existsSync(path.join(f.root, "closed")));
});
test("process runtime selectors fail before SDK query creation", async (t) => {
  const f = fixture(t);
  const inherited = process.env.QODERCLI_PATH;
  const restore = () => {
    if (inherited === undefined) delete process.env.QODERCLI_PATH;
    else process.env.QODERCLI_PATH = inherited;
  };
  t.after(restore);
  delete process.env.QODERCLI_PATH;
  for (const extra of [{ executable: "node" }, { executable: "" }, { executable: null },
    { executableArgs: ["--inspect"] }, { executableArgs: [] }, { executableArgs: null },
    { spawnQoderCLIProcess: () => { throw new Error("must not spawn"); } },
    { spawnQoderCLIProcess: null }]) {
    await assert.rejects(createQuery({ prompt: "first", options: { ...f.options, ...extra } }),
      /runtime.*overrides/);
    assert(!fs.existsSync(path.join(f.root, "queried")));
    assert(!fs.existsSync(path.join(f.root, "ready")));
  }
  process.env.QODERCLI_PATH = "/unverified/qodercli";
  // Query-local env does not mask the inherited selector used by SDK 1.0.56.
  for (const env of [undefined, {}, { QODERCLI_PATH: "" }]) {
    await assert.rejects(createQuery({ prompt: "first", options: { ...f.options, env } }),
      /QODERCLI_PATH/);
    assert(!fs.existsSync(path.join(f.root, "queried")));
    assert(!fs.existsSync(path.join(f.root, "ready")));
  }
  delete process.env.QODERCLI_PATH;
  const q = await createQuery({ prompt: "first", options: { ...f.options,
    executable: undefined, executableArgs: undefined, spawnQoderCLIProcess: undefined } });
  assert(fs.existsSync(path.join(f.root, "queried")));
  assert(fs.existsSync(path.join(f.root, "ready")));
  await q.return();
});
test("cancellation waits for owned callback process exit", async (t) => {
  const f = fixture(t, "wait");
  const q = await createQuery({ prompt: "first", options: f.options });
  const controller = new AbortController();
  const pending = invoke(q, "PreToolUse", controller.signal);
  const deadline = Date.now() + 3000;
  while (!fs.existsSync(path.join(f.root, "pid")) && Date.now() < deadline)
    await new Promise((resolve) => setTimeout(resolve, 10));
  assert(fs.existsSync(path.join(f.root, "pid")));
  controller.abort();
  assert.equal((await pending).hookSpecificOutput.permissionDecision, "deny");
  const pid = fs.readFileSync(path.join(f.root, "pid"), "utf8");
  assert(!fs.existsSync(`/proc/${pid}`));
});
test("identity mismatches block before invoking policy", async (t) => {
  const f = fixture(t);
  const q = await createQuery({ prompt: "first", options: f.options });
  const result = await q.options.hooks.PreToolUse[0].hooks[0](payload(), "other", {});
  assert.equal(result.hookSpecificOutput.permissionDecision, "deny");
  assert(!fs.existsSync(path.join(f.root, "input")));
});

test("daemon launch admission rejects overrides and unregistered daemons", async (t) => {
  const f = fixture(t);
  // A daemon created directly by the SDK has no AW-validated launch context.
  await assert.rejects(createQuery({ prompt: "first", options: { ...f.options, daemon: {} } }),
    /startDaemon/);
  assert(!fs.existsSync(path.join(f.root, "queried")));
  assert(!fs.existsSync(f.config.ready_path));
  for (const extra of [{ pathToQoderCLIExecutable: "/other" },
    { executable: "node" }, { executableArgs: [] }, { spawnQoderCLIProcess: null },
    { env: { QODER_HEADLESS_FAST_HOOKS: "1" } },
    { env: { QODER_HEADLESS_FAST_HOOKS: " true " } }]) {
    await assert.rejects(bridge.startDaemon(extra), /runtime.*overrides|disables native hooks/);
    assert(!fs.existsSync(path.join(f.root, "started")));
  }
  const selector = process.env.QODERCLI_PATH;
  t.after(() => {
    if (selector === undefined) delete process.env.QODERCLI_PATH;
    else process.env.QODERCLI_PATH = selector;
  });
  process.env.QODERCLI_PATH = "/unverified/qodercli";
  await assert.rejects(bridge.startDaemon({}), /QODERCLI_PATH/);
  assert(!fs.existsSync(path.join(f.root, "started")));
  delete process.env.QODERCLI_PATH;
  const launch = { auth: { type: "fixture" }, workerIdleTimeoutMs: 42,
    env: { QODER_HEADLESS_FAST_HOOKS: "false", APPLICATION_SETTING: "kept" } };
  const starting = bridge.startDaemon(launch);
  launch.env.QODER_HEADLESS_FAST_HOOKS = "1";
  launch.auth.type = "mutated";
  const daemon = await starting;
  assert.equal(daemon.options.env.QODER_HEADLESS_FAST_HOOKS, "false");
  assert.equal(daemon.options.auth.type, "fixture");
  launch.env.QODER_HEADLESS_FAST_HOOKS = "false";
  launch.auth.type = "fixture";
  assert.deepEqual(daemon.options.auth, launch.auth);
  assert.equal(daemon.options.workerIdleTimeoutMs, launch.workerIdleTimeoutMs);
  for (const [key, value] of Object.entries(launch.env)) assert.equal(daemon.options.env[key], value);
  assert(!fs.existsSync(f.config.ready_path), "daemon startup alone is not Query readiness");
  const q = await createQuery({ prompt: "first", options: { ...f.options, daemon } });
  assert.equal(q.options.daemon, daemon);
  assert(fs.existsSync(f.config.ready_path));
  await q.return();
  assert(!fs.existsSync(path.join(f.root, "stopped")), "Query closure must not stop the shared daemon");
  await daemon.stop();
  assert(fs.existsSync(path.join(f.root, "stopped")));
  await assert.rejects(bridge.startDaemon({ failStart: true }), /daemon startup failed/);
});

test("managed prewarm confirms worker readiness without creating a Query", async t => {
  const f = fixture(t);
  f.config.resident = true;
  f.config.resident_policy_reload = true;
  fs.writeFileSync(f.file, JSON.stringify(f.config));
  const daemon = await bridge.startDaemon({});
  try {
    assert(!fs.existsSync(path.join(f.root, "queried")));
    assert.equal(JSON.parse(fs.readFileSync(path.join(f.root, "input"))).pid, process.pid);
    assert.equal(JSON.parse(fs.readFileSync(f.config.ready_path)).hooks, f.config.hooks.length);
  } finally { await daemon.stop(); }
});

test("failed prewarm readiness stops its daemon and publishes no receipt", async t => {
  const f = fixture(t, "deny");
  f.config.resident = true;
  f.config.resident_policy_reload = true;
  fs.writeFileSync(f.file, JSON.stringify(f.config));
  await assert.rejects(bridge.startDaemon({}), /readiness failed/);
  assert(fs.existsSync(path.join(f.root, "stopped")));
  assert(!fs.existsSync(path.join(f.root, "queried")));
  assert(!fs.existsSync(f.config.ready_path));
});

test("Hook-disable flags use the pinned Worker's whitespace normalization", async (t) => {
  const f = fixture(t);
  const inherited = process.env.QODER_HEADLESS_FAST_HOOKS;
  t.after(() => {
    if (inherited === undefined) delete process.env.QODER_HEADLESS_FAST_HOOKS;
    else process.env.QODER_HEADLESS_FAST_HOOKS = inherited;
  });
  for (const value of ["1", " true ", "\tYES\n", " On "]) {
    delete process.env.QODER_HEADLESS_FAST_HOOKS;
    await assert.rejects(createQuery({ prompt: "first", options: { ...f.options,
      env: { QODER_HEADLESS_FAST_HOOKS: value } } }), /disables native hooks/);
    process.env.QODER_HEADLESS_FAST_HOOKS = value;
    await assert.rejects(createQuery({ prompt: "first", options: f.options }), /disables native hooks/);
    assert(!fs.existsSync(path.join(f.root, "queried")));
    assert(!fs.existsSync(f.config.ready_path));
  }
  process.env.QODER_HEADLESS_FAST_HOOKS = " false ";
  const q = await createQuery({ prompt: "first", options: f.options });
  assert(fs.existsSync(f.config.ready_path));
  await q.return();
});


test("reloadable Queries reject background and isolated agent definitions before SDK creation", async (t) => {
  const f = fixture(t);
  const daemon = await bridge.startDaemon({});
  fs.writeFileSync(f.file, JSON.stringify({ ...f.config, reloadable: true }));
  for (const definition of [{ background: true }, { isolation: "worktree" }]) {
    await assert.rejects(createQuery({ prompt: "first", options: { ...f.options, daemon,
      agents: { foreground: { description: "foreground", prompt: "inspect" },
        unsafe: { description: "unsafe", prompt: "inspect", ...definition } } } }),
    /Background or worktree agents/);
    assert(!fs.existsSync(path.join(f.root, "queried")));
    assert(!fs.existsSync(f.config.ready_path));
  }
  // Ordinary Query registration does not impose reload lifecycle restrictions.
  fs.writeFileSync(f.file, JSON.stringify(f.config));
  const agents = { native: { description: "native", prompt: "inspect",
    background: true, isolation: "worktree" } };
  const q = await createQuery({ prompt: "first", options: { ...f.options, daemon, agents } });
  assert.deepEqual(q.options.agents, agents);
  await q.return();
  await daemon.stop();
});


test("cold Query selects the explicit SDK site and strips its bridge selector", async t => {
  const f = fixture(t);
  const cn = path.join(f.root, "cn.mjs");
  fs.writeFileSync(cn, fs.readFileSync(f.config.sdk, "utf8").replaceAll(path.join(f.root, "queried"), path.join(f.root, "cn-queried")));
  f.config.sdks = { global: f.config.sdk, cn };
  f.config.default_site = "global";
  fs.writeFileSync(f.file, JSON.stringify(f.config));
  const q = await createQuery({ prompt: "cold", options: { ...f.options, awSite: "cn" } });
  assert(fs.existsSync(path.join(f.root, "cn-queried")));
  assert(!fs.existsSync(path.join(f.root, "queried")));
  assert.equal(q.options.awSite, undefined);
  await q.return();
});

test("Query inherits its daemon site and rejects a different explicit site before SDK dispatch", async t => {
  const f = fixture(t);
  const cn = path.join(f.root, "cn.mjs");
  fs.writeFileSync(cn, fs.readFileSync(f.config.sdk, "utf8")
    .replaceAll(path.join(f.root, "queried"), path.join(f.root, "cn-queried")));
  f.config.sdks = { global: f.config.sdk, cn };
  f.config.default_site = "global";
  fs.writeFileSync(f.file, JSON.stringify(f.config));
  const daemon = await bridge.startDaemon({ awSite: "cn" });
  let prompts = 0;
  const prompt = { async *[Symbol.asyncIterator]() { prompts++; yield "must not dispatch"; } };
  try {
    await assert.rejects(createQuery({ prompt, options: { ...f.options, daemon, awSite: "global" } }),
      /site differs/);
    assert.equal(prompts, 0);
    assert(!fs.existsSync(path.join(f.root, "queried")));
    assert(!fs.existsSync(path.join(f.root, "cn-queried")));
    assert(!fs.existsSync(f.config.ready_path));
    const q = await createQuery({ prompt: "inherited", options: { ...f.options, daemon } });
    assert(fs.existsSync(path.join(f.root, "cn-queried")));
    assert(!fs.existsSync(path.join(f.root, "queried")));
    assert.equal(q.options.daemon, daemon);
    assert.equal(q.options.awSite, undefined);
    await q.return();
  } finally { await daemon.stop(); }
});

test("nonresident native execution rejects before SDK, prompt or factory execution", async t => {
  const f = fixture(t);
  let prompts = 0, factories = 0;
  const prompt = { async *[Symbol.asyncIterator]() { prompts++; yield "must not dispatch"; } };
  await assert.rejects(createQuery({ prompt, options: { ...f.options,
    awNativeExecutionFactory: async () => { factories++; throw new Error("must not acquire"); } } }),
  /Native command execution requires an AW resident Query/);
  assert.equal(prompts, 0);
  assert.equal(factories, 0);
  assert(!fs.existsSync(path.join(f.root, "queried")));
  assert(!fs.existsSync(f.config.ready_path));
});

test("native command execution rejects absent approval and bypass modes before SDK creation", async t => {
  const f = fixture(t);
  fs.writeFileSync(f.file, JSON.stringify({ ...f.config, resident: true,
    native_command_wrappers: [path.join(f.root, "wrapper")] }));
  let prompts = 0, factories = 0;
  const prompt = { async *[Symbol.asyncIterator]() { prompts++; yield "must not dispatch"; } };
  for (const permissionMode of [undefined, "bypassPermissions", "yolo", "auto", "dontAsk", "plan"]) {
    await assert.rejects(createQuery({ prompt, options: { ...f.options, permissionMode,
      ...(permissionMode ? { canUseTool: async () => ({ behavior: "allow" }) } : {}),
      awNativeExecutionFactory: async () => { factories++; throw new Error("must not acquire"); } } }),
    permissionMode ? /requires default or acceptEdits/ : /requires an explicit canUseTool approver/);
    assert.equal(prompts, 0);
    assert.equal(factories, 0);
    assert(!fs.existsSync(path.join(f.root, "queried")));
    assert(!fs.existsSync(f.config.ready_path));
  }
});

test("preloads reach only the admitted official daemon launch", async t => {
  const f = fixture(t);
  const worker = path.join(f.root, "_worker/qoder-worker-runtime.obf.mjs");
  fs.mkdirSync(path.dirname(worker)); fs.writeFileSync(worker, "// worker");
  const preload = path.join(f.root, "preload.mjs"); fs.writeFileSync(preload, "// import");
  f.config.native_policy = { runtime_preloads: [{ path: preload,
    sha256: createHash("sha256").update(fs.readFileSync(preload)).digest("hex") }] };
  fs.writeFileSync(f.file, JSON.stringify(f.config));
  const launch = { pathToQoderCLIExecutable: worker, executableArgs: ["--import", preload] };
  await assert.rejects(createQuery({ prompt: "cold", options: { ...f.options, ...launch } }));
  assert(!fs.existsSync(path.join(f.root, "queried")));
  const daemon = await bridge.startDaemon(launch);
  assert.deepEqual(daemon.options.executableArgs, launch.executableArgs);
  await daemon.stop();
});


test("declarative profile owns the fixed native skill execution setting", async t => {
  const f = fixture(t);
  f.config.native_policy = { declarative_plugins: true };
  fs.writeFileSync(f.file, JSON.stringify(f.config));
  await assert.rejects(createQuery({ prompt: "direct", options: f.options }), /managed daemon/);
  assert(!fs.existsSync(path.join(f.root, "queried")));
  const daemon = await bridge.startDaemon({});
  const q = await createQuery({ prompt: "metadata", options: { ...f.options, daemon } });
  assert.deepEqual(q.options.settings, { skills: { disableShellExecution: true } });
  assert.equal(f.options.settings, undefined);
  await q.return();
  await assert.rejects(createQuery({ prompt: "override", options: { ...f.options,
    daemon, settings: { skills: { disableShellExecution: false } } } }));
  await daemon.stop();
});
