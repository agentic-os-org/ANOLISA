import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFile } from "child_process";
import { BtrfsManager } from "../btrfs-manager.js";
import { DEFAULT_CONFIG } from "../config.js";
import { registerHooks } from "../hooks.js";
import { register } from "../index.js";
import { registerTools } from "../tool-registry.js";
import { pluginState, workspaceRegistrationPath } from "../state.js";
import { CrontabManager } from "../cron.js";
import { loadPersistedConfig, persistConfig } from "../persist.js";
import { WS_CKPT_TOOL_NAMES } from "../whitelist.js";
import type { AnyAgentTool, OpenClawPluginApi } from "../../types-shim.js";

vi.mock("child_process", () => {
  const fn = vi.fn();
  (fn as any)[Symbol.for("nodejs.util.promisify.custom")] = vi.fn();
  return { execFile: fn };
});
vi.mock("../persist.js", () => ({
  loadPersistedConfig: vi.fn(),
  persistConfig: vi.fn(() => ""),
}));
const run = (execFile as any)[Symbol.for("nodejs.util.promisify.custom")];

function deferred() {
  let release!: () => void;
  const promise = new Promise<void>((resolve) => { release = resolve; });
  return { promise, release };
}

describe("registered rollback and turn-end workspace scope", () => {
  let root: string, workspace: string, other: string;
  let failed: boolean;
  let setupFailed: boolean;
  let statusPayload: string | undefined;
  let statusFailed: boolean;
  let calls: string[][];
  let hooks: Map<string, (...args: unknown[]) => unknown>;
  let api: OpenClawPluginApi;
  type Owner = { sessionKey?: string; sessionId?: string };
  type ToolFactory = (context: Owner) => AnyAgentTool;
  const sessionA: Owner = { sessionKey: "agent:main:a", sessionId: "id-a" };
  const sessionB: Owner = { sessionKey: "agent:main:b", sessionId: "id-b" };
  let tools: Map<string, AnyAgentTool | ToolFactory>;
  let owners: Set<Owner>;
  const originalState = { ...pluginState };

  beforeEach(async () => {
    vi.mocked(persistConfig).mockClear();
    root = fs.mkdtempSync(path.join(os.tmpdir(), "rollback-scope-"));
    workspace = path.join(root, "default");
    other = path.join(root, "other");
    fs.mkdirSync(workspace);
    fs.mkdirSync(other);
    calls = [];
    failed = false;
    setupFailed = false;
    statusPayload = undefined;
    statusFailed = false;
    run.mockImplementation(async (_bin: string, args: string[]) => {
      calls.push(args);
      if (args[0] === "status" && args.includes("json")) {
        if (statusFailed) throw Object.assign(new Error("status unavailable"), { code: 1, stdout: "", stderr: "status unavailable" });
        const selector = args[args.indexOf("--workspace") + 1];
        const resolved = selector === "ws-other" ? other : workspace;
        return { stdout: statusPayload ?? JSON.stringify({ uptime_secs: 1, workspaces: [{ ws_id: selector, path: resolved, snapshot_count: 1 }], fs_total_bytes: 0, fs_used_bytes: 0 }), stderr: "" };
      }
      if (setupFailed && ["status", "init"].includes(args[0]) && args.includes(other)) {
        throw Object.assign(new Error("setup failed"), { code: 1, stdout: "", stderr: "setup failed" });
      }
      if (args[0] === "init") {
        const target = args[args.indexOf("--workspace") + 1];
        if (!fs.existsSync(target)) {
          const stderr = `path does not exist: ${target}`;
          throw Object.assign(new Error(stderr), { code: 1, stdout: "", stderr });
        }
      }
      if (failed && args[0] === "rollback") {
        throw Object.assign(new Error("failed"), { code: 1, stdout: "", stderr: "failed" });
      }
      return { stdout: args[0] === "list" ? "[]" : "done", stderr: "" };
    });
    vi.spyOn(CrontabManager, "migrate").mockResolvedValue([]);
    const config = { ...DEFAULT_CONFIG, workspace, autoCheckpoint: true, cronSchedules: [] };
    const manager = new BtrfsManager(config);
    await manager.ensureWorkspace(workspace);
    Object.assign(pluginState, { manager, environmentReady: true, resolvedConfig: config });
    hooks = new Map();
    tools = new Map();
    owners = new Set([sessionA, sessionB]);
    api = {
      config: { tools: { allow: WS_CKPT_TOOL_NAMES } },
      on: (name: string, handler: (...args: unknown[]) => unknown) => hooks.set(name, handler),
      registerTool: (tool: AnyAgentTool | ToolFactory, opts: { name: string }) => tools.set(opts.name, tool),
    } as unknown as OpenClawPluginApi;
    registerTools(api);
    registerHooks(api, config);
    calls.length = 0;
  });

  afterEach(async () => {
    // Consume any pending one-shot state without relying on its representation.
    pluginState.resolvedConfig!.autoCheckpoint = false;
    for (const owner of owners) await end(owner);
    Object.assign(pluginState, originalState);
    vi.restoreAllMocks();
    run.mockReset();
    fs.rmSync(root, { recursive: true, force: true });
  });

  function tool(name: string, owner: Owner): AnyAgentTool {
    const registration = tools.get(name)!;
    return typeof registration === "function" ? registration(owner) : registration;
  }
  async function rollback(explicit?: string, preview = false, owner = sessionA) {
    owners.add(owner);
    return tool("ws-ckpt-rollback", owner).execute("rollback", {
      target: "before-edit", workspace: explicit, preview,
    });
  }
  function updateWorkspace(next: string) {
    return tool("ws-ckpt-config", sessionA).execute("config", {
      action: "update", key: "workspace", value: next,
    });
  }
  async function configure(next: string) {
    const result = await updateWorkspace(next);
    expect(result.details?.status).not.toBe("failed");
    return result;
  }
  function checkpoint(explicit?: string) {
    return tool("ws-ckpt-checkpoint", sessionA).execute("checkpoint", { id: "manual", workspace: explicit });
  }
  async function start() { await hooks.get("session_start")!({}); }
  async function end(owner = sessionA) { await hooks.get("agent_end")!({}, owner); }
  function targets(command: string) {
    return calls.filter((args) => args[0] === command).map((args) => args[args.indexOf("--workspace") + 1]);
  }
  function checkpoints() { return targets("checkpoint"); }

  function pauseNextIdLookup(selector: string) {
    const entered = deferred();
    const blocked = deferred();
    const execute = run.getMockImplementation()!;
    let pending = true;
    run.mockImplementation(async (bin: string, args: string[]) => {
      if (pending && args[0] === "status" && args.includes("json") && args.includes(selector)) {
        pending = false;
        entered.release();
        await blocked.promise;
      }
      return execute(bin, args);
    });
    return { entered: entered.promise, release: blocked.release };
  }


  it("maps a bare workspace ID to its registered path and keeps the rollback selector", async () => {
    const result = await rollback("ws-default");
    expect(result.details?.status).not.toBe("failed");
    expect(calls.find((args) => args[0] === "rollback")?.slice(0, 3)).toEqual(["rollback", "--workspace", "ws-default"]);
    await end(sessionB);
    expect(checkpoints()).toEqual([workspace]);
    await end();
    expect(checkpoints()).toEqual([workspace]);
    await end();
    expect(checkpoints()).toEqual([workspace, workspace]);
  });

  it("does not transfer an ID-addressed rollback to a different workspace", async () => {
    await rollback("ws-other");
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("matches a restored ID after switching to that registered path", async () => {
    await rollback("ws-other");
    await configure(other);
    await end();
    expect(checkpoints()).toEqual([]);
  });

  it("uses daemon identity even when the same bare token is a local directory", async () => {
    fs.mkdirSync(path.join(root, "ws-default"));
    const cwd = vi.spyOn(process, "cwd").mockReturnValue(root);
    try {
      await rollback("ws-default");
      await end();
      expect(checkpoints()).toEqual([]);
    } finally { cwd.mockRestore(); }
  });

  it("resolves a bare relative name without requiring an ID prefix", async () => {
    await rollback("project");
    await end();
    expect(checkpoints()).toEqual([]);
  });

  it.each([true, false])("matches configured IDs and path-addressed rollback (%s)", async (explicitPath) => {
    await configure("ws-default");
    await rollback(explicitPath ? workspace : undefined);
    await end();
    expect(checkpoints()).toEqual([]);
    await end();
    expect(checkpoints()).toEqual(["ws-default"]);
  });

  it.each(["not json", "null", "{}", '{"workspaces":[]}', '{"workspaces":[{},{}]}', '{"workspaces":[{"path":"relative"}]}', '{"workspaces":[{"path":""}]}'])("rejects unresolved registration before rollback (%s)", async (payload) => {
    statusPayload = payload;
    const result = await rollback("ws-default");
    expect(result.details?.status).toBe("failed");
    expect(result.content[0].text).toContain("resolve workspace");
    expect(calls.some((args) => args[0] === "rollback")).toBe(false);
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("reports status failure without executing rollback", async () => {
    statusFailed = true;
    const result = await rollback("ws-default");
    expect(result.details?.status).toBe("failed");
    expect(result.content[0].text).toContain("status unavailable");
    expect(calls.some((args) => args[0] === "rollback")).toBe(false);
  });

  it("does not query registration or arm suppression for an ID preview", async () => {
    statusFailed = true;
    await rollback("ws-default", true);
    expect(calls.some((args) => args[0] === "status")).toBe(false);
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("does not arm an ID rollback that fails after status resolution", async () => {
    failed = true;
    const result = await rollback("ws-default");
    expect(result.details?.status).toBe("failed");
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("skips an unresolved ID checkpoint and consumes the pending rollback state", async () => {
    await configure("ws-default");
    await rollback(workspace);
    statusFailed = true;
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    await end();
    expect(warning).toHaveBeenCalledWith(expect.stringContaining("status unavailable"));
    expect(checkpoints()).toEqual([]);
    statusFailed = false;
    await end();
    expect(checkpoints()).toEqual(["ws-default"]);
  });

  it("does not add a daemon lookup to path-addressed rollbacks", async () => {
    await rollback(workspace);
    await end();
    expect(calls.some((args) => args[0] === "status")).toBe(false);
    expect(checkpoints()).toEqual([]);
  });

  it.each(["succeeds", "fails"])("refuses a pending default rollback if a workspace switch %s during ID lookup", async (outcome) => {
    await configure("ws-default");
    const execute = run.getMockImplementation()!;
    let release!: () => void;
    let started!: () => void;
    const begun = new Promise<void>((resolve) => { started = resolve; });
    const waiting = new Promise<void>((resolve) => { release = resolve; });
    run.mockImplementation(async (bin: string, args: string[]) => {
      if (args[0] === "status" && args.includes("json")) { started(); await waiting; }
      return execute(bin, args);
    });
    const pending = rollback();
    await begun;
    setupFailed = outcome === "fails";
    await configure(other);
    release();
    const result = await pending;
    expect(result.details?.status).toBe("failed");
    expect(result.content[0].text).toContain(setupFailed ? `Default workspace ${other} is not ready` : "changed");
    expect(calls.some((args) => args[0] === "rollback")).toBe(false);
    await end();
    expect(checkpoints()).toEqual(setupFailed ? [] : [other]);
  });

  it("does not skip a new manager target after awaiting the old ID registration", async () => {
    await configure("ws-default");
    await rollback(workspace);
    const execute = run.getMockImplementation()!;
    let release!: () => void;
    let started!: () => void;
    const begun = new Promise<void>((resolve) => { started = resolve; });
    const waiting = new Promise<void>((resolve) => { release = resolve; });
    run.mockImplementation(async (bin: string, args: string[]) => {
      if (args[0] === "status" && args.includes("json")) { started(); await waiting; }
      return execute(bin, args);
    });
    const pending = end();
    await begun;
    await configure(other);
    release();
    await pending;
    expect(checkpoints()).toEqual([other]);
    await configure(workspace);
    await end();
    expect(checkpoints()).toEqual([other, workspace]);
  });

  it.each(["disabled", "unavailable"])("rechecks checkpoint readiness after ID lookup (%s)", async (state) => {
    await configure("ws-default");
    await rollback(other);
    const execute = run.getMockImplementation()!;
    let release!: () => void;
    let started!: () => void;
    const begun = new Promise<void>((resolve) => { started = resolve; });
    const waiting = new Promise<void>((resolve) => { release = resolve; });
    run.mockImplementation(async (bin: string, args: string[]) => {
      if (args[0] === "status" && args.includes("json")) { started(); await waiting; }
      return execute(bin, args);
    });
    const pending = end();
    await begun;
    if (state === "disabled") pluginState.resolvedConfig!.autoCheckpoint = false;
    else pluginState.environmentReady = false;
    release();
    await pending;
    expect(checkpoints()).toEqual([]);
    pluginState.resolvedConfig!.autoCheckpoint = true;
    pluginState.environmentReady = true;
    await configure(other);
    await end();
    expect(checkpoints()).toEqual([other]);
  });

  it("does not let another session consume a rollback on the same workspace", async () => {
    await rollback();
    await end(sessionB);
    expect(checkpoints()).toEqual([workspace]);
    await end(sessionA);
    expect(checkpoints()).toEqual([workspace]);
    await end(sessionA);
    expect(checkpoints()).toEqual([workspace, workspace]);
  });

  it("retains both sessions' independently pending rollbacks", async () => {
    await rollback();
    await rollback(undefined, false, sessionB);
    await end(sessionA);
    await end(sessionB);
    expect(checkpoints()).toEqual([]);
    await end(sessionB);
    expect(checkpoints()).toEqual([workspace]);
  });

  it("keeps a new conversation from consuming the old session instance", async () => {
    const resetSession = { ...sessionA, sessionId: "after-reset" };
    await rollback();
    await end(resetSession);
    expect(checkpoints()).toEqual([workspace]);
    await end(sessionA);
    expect(checkpoints()).toEqual([workspace]);
  });

  it("matches the supported legacy factory without sessionId", async () => {
    const legacy = { sessionKey: sessionA.sessionKey };
    await rollback(undefined, false, legacy);
    await end(sessionB);
    expect(checkpoints()).toEqual([workspace]);
    await end(sessionA);
    expect(checkpoints()).toEqual([workspace]);
    await end(sessionA);
    expect(checkpoints()).toEqual([workspace, workspace]);
  });

  it.each([{}, { sessionId: "id-without-key" }])("does not arm global suppression when the factory has no session key (%j)", async (owner) => {
    await rollback(undefined, false, owner);
    await end(sessionB);
    expect(checkpoints()).toEqual([workspace]);
  });

  it("does not consume another session's pending state without hook identity", async () => {
    await rollback();
    await end({});
    expect(checkpoints()).toEqual([workspace]);
    await end(sessionA);
    expect(checkpoints()).toEqual([workspace]);
  });

  it.each(["enabled", "disabled", "unavailable"])("avoids path resolution without a pending rollback (%s)", async (state) => {
    if (state === "disabled") pluginState.resolvedConfig!.autoCheckpoint = false;
    if (state === "unavailable") pluginState.environmentReady = false;
    const resolve = vi.spyOn(fs.realpathSync, "native");
    await end();
    expect(resolve).not.toHaveBeenCalled();
  });

  it("does not resolve another session's pending workspace", async () => {
    await rollback();
    const resolve = vi.spyOn(fs.realpathSync, "native");
    await end(sessionB);
    expect(resolve).not.toHaveBeenCalled();
    expect(checkpoints()).toEqual([workspace]);
  });

  it.each(["disabled", "unavailable"])("consumes pending state without resolving an inactive turn (%s)", async (state) => {
    await rollback();
    if (state === "disabled") pluginState.resolvedConfig!.autoCheckpoint = false;
    else pluginState.environmentReady = false;
    const resolve = vi.spyOn(fs.realpathSync, "native");
    await end();
    expect(resolve).not.toHaveBeenCalled();
    pluginState.resolvedConfig!.autoCheckpoint = true;
    pluginState.environmentReady = true;
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("keeps the default checkpoint after another workspace's rollback", async () => {
    await rollback(other);
    expect(calls[0][calls[0].indexOf("--workspace") + 1]).toBe(other);
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it.each([false, true])("suppresses one turn for the same workspace (explicit=%s)", async (explicit) => {
    await rollback(explicit ? workspace : undefined);
    await end();
    expect(checkpoints()).toEqual([]);
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("does not transfer or retain a skip when configuration changes", async () => {
    await rollback();
    await configure(other);
    await end();
    expect(checkpoints()).toEqual([other]);
    await configure(workspace);
    await end();
    expect(checkpoints()).toEqual([other, workspace]);
  });

  it.each(["returns false", "throws"])("pauses default operations and retries a saved workspace when setup %s", async (outcome) => {
    if (outcome === "throws") {
      vi.spyOn(BtrfsManager.prototype, "initialize").mockRejectedValueOnce(new Error("setup interrupted"))
        .mockRejectedValueOnce(new Error("setup interrupted again"));
    }
    setupFailed = true;
    const result = await configure(other);
    expect(result.content[0].text).toContain(`Config updated: workspace = ${other}`);
    expect(result.content[0].text).toContain("WARNING: workspace setup failed");
    expect(persistConfig).toHaveBeenCalledWith({ workspace: other });

    for (const operation of [await checkpoint(), await rollback(), await rollback(undefined, true)]) {
      expect(operation.details?.status).toBe("failed");
      expect(operation.content[0].text).toContain(`Default workspace ${other} is not ready`);
    }
    await end();
    await start();
    expect(checkpoints()).toEqual([]);
    expect(targets("rollback")).toEqual([]);

    setupFailed = false;
    const retry = await configure(other);
    expect(retry.content[0].text).not.toContain("WARNING");
    expect((await checkpoint()).details?.status).not.toBe("failed");
    expect((await rollback()).details?.status).not.toBe("failed");
    expect(checkpoints()).toEqual([other]);
    expect(targets("rollback")).toEqual([other]);
  });

  it("retries the configured target on session start and checkpoints only after it binds", async () => {
    setupFailed = true;
    await configure(other);
    await start();
    expect(checkpoints()).toEqual([]);
    setupFailed = false;
    await start();
    await end();
    expect(checkpoints()).toEqual([other, other]);
  });

  it("keeps default operations available while another session starts on the bound workspace", async () => {
    const execute = run.getMockImplementation()!;
    const started = deferred();
    const blocked = deferred();
    let paused = false;
    run.mockImplementation(async (bin: string, args: string[]) => {
      // Pause the old setup path or the initial checkpoint on the ready path.
      if (!paused && (args[0] === "list" || (
        args[0] === "checkpoint" && args.includes('{"auto":true,"type":"initial"}')
      ))) {
        paused = true;
        started.release();
        await blocked.promise;
      }
      return execute(bin, args);
    });

    const pending = start();
    await started.promise;
    try {
      expect((await checkpoint()).details?.status).not.toBe("failed");
      expect((await rollback(undefined, true, sessionB)).details?.status).not.toBe("failed");
      expect((await rollback(undefined, false, sessionB)).details?.status).not.toBe("failed");
      await end(sessionA);
      await start();
    } finally {
      blocked.release();
      await pending;
    }
    expect(checkpoints()).toEqual([workspace, workspace, workspace, workspace]);
    expect(targets("rollback")).toEqual([workspace, workspace]);
    expect(targets("init")).toEqual([]);
  });

  it("creates the initial checkpoint for a registered ID that init cannot use as a path", async () => {
    const id = "ws-default";
    expect(fs.existsSync(id)).toBe(false);
    await configure(id);
    expect(targets("status")).toContain(id);
    await start();
    expect(checkpoints()).toEqual([id]);
    expect(targets("init")).toEqual([]);
  });

  it.each([true, false])("reserves the workspace update during ID validation (ID resolves: %s)", async (resolved) => {
    const id = "ws-other";
    const lookup = pauseNextIdLookup(id);
    statusFailed = !resolved;
    const first = updateWorkspace(id);
    await lookup.entered;
    try {
      const conflict = await updateWorkspace(other);
      expect(conflict.details?.status).toBe("failed");
      expect(conflict.content[0].text).toMatch(/retry/i);
      expect(pluginState.resolvedConfig?.workspace).toBe(workspace);
      expect(persistConfig).not.toHaveBeenCalled();
      expect(CrontabManager.migrate).not.toHaveBeenCalled();
      expect((await checkpoint()).details?.status).not.toBe("failed");
      expect(checkpoints()).toEqual([workspace]);
    } finally {
      lookup.release();
      await first;
    }
    const firstResult = await first;
    expect(firstResult.details?.status).toBe(resolved ? undefined : "failed");
    expect(pluginState.resolvedConfig?.workspace).toBe(resolved ? id : workspace);
    await configure(other);
    expect(pluginState.resolvedConfig?.workspace).toBe(other);
  });

  it.each([true, false])("does not let an older enable overwrite a confirmed disable (initially %s)", async (initial) => {
    const id = "ws-other";
    await configure(id);
    pluginState.resolvedConfig!.autoCheckpoint = initial;
    vi.mocked(persistConfig).mockClear();
    const lookup = pauseNextIdLookup(id);
    const setAutoCheckpoint = (value: "true" | "false", owner: Owner) =>
      tool("ws-ckpt-config", owner).execute("config", { action: "update", key: "autoCheckpoint", value });

    const first = setAutoCheckpoint("true", sessionA);
    await lookup.entered;
    try {
      const disabled = await setAutoCheckpoint("false", sessionB);
      expect(disabled.details?.status).not.toBe("failed");
    } finally {
      lookup.release();
      await first;
    }
    const conflict = await first;
    expect(conflict.details?.status).toBe("failed");
    expect(conflict.content[0].text).toMatch(/retry/i);
    expect(pluginState.resolvedConfig?.autoCheckpoint).toBe(false);
    expect(vi.mocked(persistConfig).mock.calls).toEqual([[{ autoCheckpoint: false }]]);
    const enabled = await setAutoCheckpoint("true", sessionA);
    expect(enabled.details?.status).not.toBe("failed");
    expect(pluginState.resolvedConfig?.autoCheckpoint).toBe(true);
  });

  it.each([true, false])("does not overwrite a reloaded disable for the same workspace (initially %s)", async (initial) => {
    const id = "ws-other";
    await configure(id);
    pluginState.resolvedConfig!.autoCheckpoint = initial;
    vi.mocked(persistConfig).mockClear();
    const lookup = pauseNextIdLookup(id);
    const enabling = tool("ws-ckpt-config", sessionA).execute("config", {
      action: "update", key: "autoCheckpoint", value: "true",
    });
    await lookup.entered;
    try {
      vi.mocked(loadPersistedConfig).mockReturnValue({ workspace: id, autoCheckpoint: false });
      register(api);
      await pluginState.registrationSetup?.done;
      expect(pluginState.resolvedConfig?.workspace).toBe(id);
      expect(pluginState.resolvedConfig?.autoCheckpoint).toBe(false);
    } finally {
      lookup.release();
      await enabling;
    }
    const conflict = await enabling;
    expect(conflict.details?.status).toBe("failed");
    expect(conflict.content[0].text).toMatch(/retry/i);
    expect(pluginState.resolvedConfig?.autoCheckpoint).toBe(false);
    expect(persistConfig).not.toHaveBeenCalled();
  });

  it("dispatches each turn's message when another session finishes during the first ID lookup", async () => {
    const id = "ws-default";
    await configure(id);
    const receive = hooks.get("message_received")!;
    const lookup = pauseNextIdLookup(id);
    receive({ content: "session A message" }, sessionA);
    const first = end(sessionA);
    await lookup.entered;
    try {
      receive({ content: "session B message" }, sessionB);
      await end(sessionB);
    } finally {
      lookup.release();
      await first;
      receive({}, sessionB);
    }
    const messages = calls
      .filter((args) => args[0] === "checkpoint")
      .map((args) => args[args.indexOf("--message") + 1]);
    expect(checkpoints()).toEqual([id, id]);
    expect(messages).toEqual(["session B message", "session A message"]);
  });

  it.each(["path", "ID"])("refuses a workspace update from inside the target selected by %s", async (selector) => {
    vi.spyOn(process, "cwd").mockReturnValue(other);
    const result = await updateWorkspace(selector === "path" ? other : "ws-other");
    expect(result.details?.status).toBe("failed");
    expect(result.content[0].text).toContain(`inside workspace=${other}`);
    expect(pluginState.resolvedConfig?.workspace).toBe(workspace);
    expect(persistConfig).not.toHaveBeenCalled();
    expect(CrontabManager.migrate).not.toHaveBeenCalled();
    expect(targets("init")).toEqual([]);
  });

  it.each(["path", "ID"])("refuses to enable auto-checkpoint inside the workspace selected by %s", async (selector) => {
    await configure(selector === "path" ? other : "ws-other");
    pluginState.resolvedConfig!.autoCheckpoint = false;
    vi.spyOn(process, "cwd").mockReturnValue(other);
    const result = await tool("ws-ckpt-config", sessionA).execute("config", {
      action: "update", key: "autoCheckpoint", value: "true",
    });
    expect(result.details?.status).toBe("failed");
    expect(result.content[0].text).toContain(`inside workspace=${other}`);
    expect(pluginState.resolvedConfig?.autoCheckpoint).toBe(false);
  });

  it.each([
    ["session start", "path"], ["session start", "ID"],
    ["turn end", "path"], ["turn end", "ID"],
  ])("disables automatic checkpoint at %s inside the workspace selected by %s", async (hook, selector) => {
    await configure(selector === "path" ? other : "ws-other");
    vi.spyOn(process, "cwd").mockReturnValue(other);
    if (hook === "session start") await start();
    else await end();
    expect(pluginState.resolvedConfig?.autoCheckpoint).toBe(false);
    expect(checkpoints()).toEqual([]);
  });

  it.each(["path", "ID"])("refuses registration inside the workspace selected by %s", async (selector) => {
    vi.mocked(loadPersistedConfig).mockReturnValue({
      workspace: selector === "path" ? other : "ws-other", autoCheckpoint: true,
    });
    vi.spyOn(process, "cwd").mockReturnValue(other);
    register(api);
    await pluginState.registrationSetup?.done;
    expect(pluginState.environmentReady).toBe(false);
    expect(pluginState.manager?.getWorkspacePath()).toBe(workspace);
    expect(targets("init")).toEqual([]);
  });

  it("checks the registered path for manual checkpoints and keeps the original ID for dispatch", async () => {
    const id = "ws-other";
    await configure(id);
    const cwd = vi.spyOn(process, "cwd").mockReturnValue(other);
    for (const selector of [undefined, id]) {
      const result = await checkpoint(selector);
      expect(result.details?.status).toBe("failed");
      expect(result.content[0].text).toContain(`inside workspace=${other}`);
    }
    expect(checkpoints()).toEqual([]);
    cwd.mockRestore();
    expect((await checkpoint()).details?.status).not.toBe("failed");
    expect((await checkpoint(id)).details?.status).not.toBe("failed");
    expect(checkpoints()).toEqual([id, id]);
  });

  it("drops the old ID check when the workspace changes during session start", async () => {
    const id = "ws-other";
    await configure(id);
    const started = deferred();
    const blocked = deferred();
    const execute = run.getMockImplementation()!;
    run.mockImplementation(async (bin: string, args: string[]) => {
      if (args[0] === "status" && args.includes("json") && args.includes(id)) {
        started.release();
        await blocked.promise;
      }
      return execute(bin, args);
    });
    vi.spyOn(process, "cwd").mockReturnValue(other);
    const pending = start();
    await started.promise;
    try {
      await configure(workspace);
    } finally {
      blocked.release();
      await pending;
    }
    expect(pluginState.resolvedConfig?.autoCheckpoint).toBe(true);
    expect(checkpoints()).toEqual([]);
    await start();
    expect(checkpoints()).toEqual([workspace]);
  });

  it.each(["succeeds", "fails"])("keeps a runtime update bound when earlier registration %s", async (outcome) => {
    vi.mocked(loadPersistedConfig).mockReturnValue({ workspace, autoCheckpoint: false });
    pluginState.manager = null;
    pluginState.environmentReady = false;
    const execute = run.getMockImplementation()!;
    const started = deferred();
    const blocked = deferred();
    run.mockImplementation(async (bin: string, args: string[]) => {
      if (args[0] === "status" && args.includes(workspace)) {
        started.release();
        await blocked.promise;
      }
      if (outcome === "fails" && ["status", "init"].includes(args[0]) && args.includes(workspace)) {
        const stderr = "old workspace setup failed";
        throw Object.assign(new Error(stderr), { code: 1, stdout: "", stderr });
      }
      return execute(bin, args);
    });

    register(api);
    await started.promise;
    const update = configure(other);
    // Drain runnable work while the registration's older status is pending.
    await new Promise<void>((resolve) => setImmediate(resolve));
    blocked.release();
    const result = await update;
    await new Promise<void>((resolve) => setImmediate(resolve));

    expect(result.content[0].text).not.toContain("WARNING");
    expect((await checkpoint()).details?.status).not.toBe("failed");
    expect((await rollback(undefined, true)).details?.status).not.toBe("failed");
    expect((await rollback()).details?.status).not.toBe("failed");
    await start();
    await end();
    expect(checkpoints()).toEqual([other]);
    expect(targets("rollback")).toEqual([other, other]);
  });

  it("keeps the selected workspace when registration reloads during a runtime update", async () => {
    const started = deferred();
    const blocked = deferred();
    vi.mocked(loadPersistedConfig).mockReturnValue({ workspace, autoCheckpoint: false });
    vi.mocked(CrontabManager.migrate).mockImplementationOnce(async () => {
      started.release();
      await blocked.promise;
      return [];
    });

    const update = configure(other);
    await started.promise;
    register(api);
    blocked.release();
    const result = await update;

    expect(result.content[0].text).not.toContain("WARNING");
    expect((await checkpoint()).details?.status).not.toBe("failed");
    expect(checkpoints()).toEqual([other]);
  });

  it("applies the workspace read by registration after in-flight session recovery", async () => {
    setupFailed = true;
    await configure(other);
    setupFailed = false;
    const started = deferred();
    const blocked = deferred();
    const execute = run.getMockImplementation()!;
    run.mockImplementation(async (bin: string, args: string[]) => {
      if (args[0] === "status" && args.includes(other)) {
        started.release();
        await blocked.promise;
      }
      return execute(bin, args);
    });

    const newWorkspace = path.join(root, "reloaded");
    fs.mkdirSync(newWorkspace);
    const recovery = start();
    await started.promise;
    try {
      vi.mocked(loadPersistedConfig).mockReturnValue({ workspace: newWorkspace, autoCheckpoint: true });
      register(api);
      expect((await checkpoint()).details?.status).toBe("failed");
      expect(targets("status")).not.toContain(newWorkspace);
    } finally {
      blocked.release();
      await recovery;
      await pluginState.registrationSetup?.done;
    }

    expect(pluginState.resolvedConfig?.workspace).toBe(newWorkspace);
    expect(checkpoints()).toEqual([]);
    expect((await checkpoint()).details?.status).not.toBe("failed");
    expect((await rollback(undefined, true)).details?.status).not.toBe("failed");
    expect(checkpoints()).toEqual([newWorkspace]);
    expect(targets("rollback")).toEqual([newWorkspace]);
  });

  it("preserves explicit selectors and daemon checks while default operations are paused", async () => {
    setupFailed = true;
    await configure(other);
    expect((await checkpoint(workspace)).details?.status).not.toBe("failed");
    expect((await rollback("ws-other", true)).details?.status).not.toBe("failed");
    expect((await rollback("ws-other")).details?.status).not.toBe("failed");
    expect(checkpoints()).toEqual([workspace]);
    expect(targets("rollback")).toEqual(["ws-other", "ws-other"]);
    expect(targets("status")).toContain("ws-other");
    statusFailed = true;
    expect((await rollback("ws-other")).details?.status).toBe("failed");
    expect(targets("rollback")).toEqual(["ws-other", "ws-other"]);
  });

  it.each(["config update", "session start"])("blocks a second switch while %s is still initializing", async (source) => {
    if (source === "session start") {
      setupFailed = true;
      await configure(other);
      setupFailed = false;
    }

    const execute = run.getMockImplementation()!;
    let release!: () => void;
    let started!: () => void;
    const begun = new Promise<void>((resolve) => { started = resolve; });
    const waiting = new Promise<void>((resolve) => { release = resolve; });
    run.mockImplementation(async (bin: string, args: string[]) => {
      const result = await execute(bin, args);
      // The manager has already assigned the selector, but initialization is
      // still refreshing its snapshot cache.
      if (args[0] === "list" && args.includes(other)) { started(); await waiting; }
      return result;
    });
    const pending = source === "config update" ? configure(other) : start();
    await begun;
    const third = path.join(root, "third");
    const migrate = vi.mocked(CrontabManager.migrate);
    const migrationCount = migrate.mock.calls.length;
    try {
      for (const operation of [await checkpoint(), await rollback(), await rollback(undefined, true)]) {
        expect(operation.details?.status).toBe("failed");
        expect(operation.content[0].text).toContain(`Default workspace ${other} is not ready`);
      }
      const rejected = await updateWorkspace(third);
      expect(rejected.details?.status).toBe("failed");
      expect(rejected.content[0].text).toMatch(/retry/i);
      expect(persistConfig).not.toHaveBeenCalledWith({ workspace: third });
      expect(migrate).toHaveBeenCalledTimes(migrationCount);
      expect(calls.some((args) => args.includes(third))).toBe(false);
      const initCount = targets("init").length;
      await start();
      await end();
      expect(targets("init")).toHaveLength(initCount);
      expect(checkpoints()).toEqual([]);
      expect(targets("rollback")).toEqual([]);
    } finally {
      release();
      await pending;
    }
    expect(checkpoints()).toEqual(source === "session start" ? [other] : []);
    await configure(third);
  });

  it("can switch configuration to the restored workspace", async () => {
    await rollback(other);
    await configure(other);
    await end();
    expect(checkpoints()).toEqual([]);
    await end();
    expect(checkpoints()).toEqual([other]);
  });

  it.each([true, false])("does not suppress after preview or failure (preview=%s)", async (preview) => {
    failed = !preview;
    await rollback(undefined, preview);
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("retains all workspaces rolled back in the current turn", async () => {
    await rollback();
    await rollback(other);
    await end();
    expect(checkpoints()).toEqual([]);
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("matches a parent alias with a trailing dot and separator", async () => {
    const alias = path.join(root, "parent-alias");
    fs.symlinkSync(root, alias, "dir");
    await rollback(alias + "/default/./");
    await end();
    expect(checkpoints()).toEqual([]);
  });

  it("resolves a parent symlink before dotdot without lexical cancellation", async () => {
    const nested = path.join(root, "nested");
    fs.mkdirSync(path.join(nested, "child"), { recursive: true });
    fs.mkdirSync(path.join(nested, "default"));
    const alias = path.join(root, "alias");
    fs.symlinkSync(path.join(nested, "child"), alias, "dir");
    await rollback(alias + "/../default");
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("keeps registration identity when the final symlink target changes", async () => {
    const anchor = path.join(root, "anchor");
    fs.symlinkSync(workspace, anchor, "dir");
    await configure(anchor);
    await rollback();
    fs.unlinkSync(anchor);
    fs.symlinkSync(other, anchor, "dir");
    await end();
    expect(checkpoints()).toEqual([]);
  });

  it.each(["disabled", "unavailable"])("consumes suppression when the turn is %s", async (state) => {
    await rollback();
    if (state === "disabled") pluginState.resolvedConfig!.autoCheckpoint = false;
    else pluginState.environmentReady = false;
    await end();
    pluginState.resolvedConfig!.autoCheckpoint = true;
    pluginState.environmentReady = true;
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });

  it("does not merge distinct registration links to the same directory", async () => {
    const anchor = path.join(root, "anchor");
    fs.symlinkSync(workspace, anchor, "dir");
    await rollback(anchor);
    await end();
    expect(checkpoints()).toEqual([workspace]);
  });
});


it.each([".", "/"])("uses daemon registration identity for %s", (workspace) => {
  expect(workspaceRegistrationPath(workspace)).toBe(fs.realpathSync.native(workspace));
});
