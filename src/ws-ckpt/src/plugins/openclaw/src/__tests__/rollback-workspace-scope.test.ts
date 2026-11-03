import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { execFile } from "child_process";
import { BtrfsManager } from "../btrfs-manager.js";
import { DEFAULT_CONFIG } from "../config.js";
import { registerHooks } from "../hooks.js";
import { registerTools } from "../tool-registry.js";
import { pluginState, workspaceRegistrationPath } from "../state.js";
import { CrontabManager } from "../cron.js";
import type { AnyAgentTool, OpenClawPluginApi } from "../../types-shim.js";

vi.mock("child_process", () => {
  const fn = vi.fn();
  (fn as any)[Symbol.for("nodejs.util.promisify.custom")] = vi.fn();
  return { execFile: fn };
});
vi.mock("../persist.js", () => ({ persistConfig: () => "" }));
const run = (execFile as any)[Symbol.for("nodejs.util.promisify.custom")];

describe("registered rollback and turn-end workspace scope", () => {
  let root: string, workspace: string, other: string;
  let failed: boolean;
  let setupFailed: boolean;
  let statusPayload: string | undefined;
  let statusFailed: boolean;
  let calls: string[][];
  let hooks: Map<string, (...args: unknown[]) => unknown>;
  type Owner = { sessionKey?: string; sessionId?: string };
  type ToolFactory = (context: Owner) => AnyAgentTool;
  const sessionA: Owner = { sessionKey: "agent:main:a", sessionId: "id-a" };
  const sessionB: Owner = { sessionKey: "agent:main:b", sessionId: "id-b" };
  let tools: Map<string, AnyAgentTool | ToolFactory>;
  let owners: Set<Owner>;
  const originalState = { ...pluginState };

  beforeEach(async () => {
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
    const api = {
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
  async function configure(next: string) {
    const result = await tool("ws-ckpt-config", sessionA).execute("config", {
      action: "update", key: "workspace", value: next,
    });
    expect(result.details?.status).not.toBe("failed");
  }
  async function end(owner = sessionA) { await hooks.get("agent_end")!({}, owner); }
  function checkpoints() {
    return calls.filter((args) => args[0] === "checkpoint").map((args) => args[args.indexOf("--workspace") + 1]);
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

  it("consumes pending state and reports a hook lookup failure without skipping another turn", async () => {
    await configure("ws-default");
    await rollback(workspace);
    statusFailed = true;
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    await end();
    expect(warning).toHaveBeenCalledWith(expect.stringContaining("status unavailable"));
    expect(checkpoints()).toEqual(["ws-default"]);
    statusFailed = false;
    await end();
    expect(checkpoints()).toEqual(["ws-default", "ws-default"]);
  });

  it("does not add a daemon lookup to path-addressed rollbacks", async () => {
    await rollback(workspace);
    await end();
    expect(calls.some((args) => args[0] === "status")).toBe(false);
    expect(checkpoints()).toEqual([]);
  });

  it("refuses a default rollback if configuration changes during ID lookup", async () => {
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
    await configure(other);
    release();
    const result = await pending;
    expect(result.details?.status).toBe("failed");
    expect(result.content[0].text).toContain("changed");
    expect(calls.some((args) => args[0] === "rollback")).toBe(false);
    await end();
    expect(checkpoints()).toEqual([other]);
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

  it.each([false, true])("uses the manager workspace after failed setup (explicit=%s)", async (explicit) => {
    setupFailed = true;
    await configure(other);
    expect(pluginState.resolvedConfig!.workspace).toBe(other);
    expect(pluginState.manager!.getWorkspacePath()).toBe(workspace);
    await rollback(explicit ? other : undefined);
    const rollbackCall = calls.find((args) => args[0] === "rollback")!;
    expect(rollbackCall[rollbackCall.indexOf("--workspace") + 1]).toBe(explicit ? other : workspace);
    await end();
    expect(checkpoints()).toEqual(explicit ? [workspace] : []);
    if (!explicit) {
      await end();
      expect(checkpoints()).toEqual([workspace]);
    }
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
