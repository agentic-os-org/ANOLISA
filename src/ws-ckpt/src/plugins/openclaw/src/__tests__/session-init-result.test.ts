import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { registerHooks } from "../hooks.js";
import { pluginState } from "../state.js";
import { CrontabManager } from "../cron.js";
import { CommandExecutor } from "../commands.js";
import { BtrfsManager } from "../btrfs-manager.js";
import type { PluginConfig } from "../types.js";
import type { OpenClawPluginApi } from "../../types-shim.js";

let previous: typeof pluginState;
let callbacks: Record<string, (event: unknown) => Promise<void>>;
let config: PluginConfig;
let initialize: ReturnType<typeof vi.fn>;
let checkpoint: ReturnType<typeof vi.fn>;

beforeEach(() => {
  previous = { ...pluginState };
  callbacks = {};
  initialize = vi.fn().mockResolvedValue(true);
  checkpoint = vi
    .fn()
    .mockResolvedValue({ success: true, snapshot: "fixture-snapshot" });
  config = {
    workspace: "/private/session-init-fixture",
    autoCheckpoint: true,
    cronSchedules: [],
  };
  pluginState.manager = {
    initialize,
    createCheckpoint: checkpoint,
  } as unknown as NonNullable<typeof pluginState.manager>;
  pluginState.environmentReady = true;
  pluginState.resolvedConfig = config;
  pluginState.skipNextAutoCheckpoint = false;
  vi.spyOn(console, "log").mockImplementation(() => {});
  vi.spyOn(console, "warn").mockImplementation(() => {});
  vi.spyOn(CrontabManager, "syncWithRetry").mockResolvedValue(true);
  const api = {
    on(name: string, callback: (event: unknown) => Promise<void>) {
      callbacks[name] = callback;
    },
  } as unknown as OpenClawPluginApi;
  registerHooks(api, config);
});

afterEach(() => {
  vi.restoreAllMocks();
  Object.assign(pluginState, previous);
});

describe("session_start initialization result", () => {
  it("does not checkpoint the previously active workspace through the real manager", async () => {
    const setup = vi
      .spyOn(CommandExecutor.prototype, "init")
      .mockResolvedValue({ exitCode: 0, stdout: "", stderr: "" });
    vi.spyOn(CommandExecutor.prototype, "list").mockResolvedValue({
      exitCode: 0,
      stdout: "[]",
      stderr: "",
    });
    const create = vi
      .spyOn(CommandExecutor.prototype, "checkpoint")
      .mockResolvedValue({ exitCode: 0, stdout: "", stderr: "" });
    const manager = new BtrfsManager(config);
    await manager.initialize("/private/previous-workspace-fixture");
    pluginState.manager = manager;
    setup.mockResolvedValue({
      exitCode: 1,
      stdout: "",
      stderr: "controlled initialization failure",
    });
    vi.spyOn(console, "error").mockImplementation(() => {});
    await callbacks.session_start({});
    expect(manager.getWorkspacePath()).toBe(
      "/private/previous-workspace-fixture",
    );
    expect(create).not.toHaveBeenCalled();
  });

  it("does not create or announce an initial snapshot after failed setup", async () => {
    initialize.mockResolvedValue(false);
    await callbacks.session_start({});
    expect(initialize).toHaveBeenCalledWith(config.workspace);
    expect(checkpoint).not.toHaveBeenCalled();
    expect(console.warn).toHaveBeenCalledWith(
      "[ws-ckpt] Session start workspace re-init failed",
    );
    expect(console.log).not.toHaveBeenCalledWith(
      expect.stringContaining("Initial checkpoint"),
    );
  });

  it("keeps successful setup and the initial checkpoint contract", async () => {
    await callbacks.session_start({});
    expect(checkpoint).toHaveBeenCalledOnce();
    expect(checkpoint).toHaveBeenCalledWith({
      id: expect.any(String),
      message: "session start",
      metadata: '{"auto":true,"type":"initial"}',
    });
    expect(console.warn).not.toHaveBeenCalled();
  });

  it("keeps an initialization exception from reaching checkpoint creation", async () => {
    const error = new Error("controlled setup exception");
    initialize.mockRejectedValue(error);
    await callbacks.session_start({});
    expect(checkpoint).not.toHaveBeenCalled();
    expect(console.warn).toHaveBeenCalledWith(
      "[ws-ckpt] Session start workspace re-init failed:",
      error,
    );
  });

  it("allows a later session retry after a returned failure", async () => {
    initialize.mockResolvedValueOnce(false).mockResolvedValueOnce(true);
    await callbacks.session_start({});
    expect(checkpoint).not.toHaveBeenCalled();
    expect(config.autoCheckpoint).toBe(true);
    await callbacks.session_start({});
    expect(initialize).toHaveBeenCalledTimes(2);
    expect(checkpoint).toHaveBeenCalledOnce();
  });

  it("preserves independent cron synchronization when setup fails", async () => {
    config.cronSchedules = ["0 * * * *"];
    initialize.mockResolvedValue(false);
    await callbacks.session_start({});
    expect(CrontabManager.syncWithRetry).toHaveBeenCalledWith(
      config.workspace,
      config.cronSchedules,
    );
    expect(checkpoint).not.toHaveBeenCalled();
  });
});
