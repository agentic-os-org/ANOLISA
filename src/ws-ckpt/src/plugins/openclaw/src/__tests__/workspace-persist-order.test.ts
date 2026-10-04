import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

import { handleConfig } from "../handlers.js";
import { pluginState } from "../state.js";
import { CrontabManager } from "../cron.js";
import * as persist from "../persist.js";

// `ws-ckpt-config update workspace` must not activate a workspace whose
// persistence failed: the in-memory pivot drives every later checkpoint/
// rollback, and the crontab migration is a persistent side effect, so a
// rejected save leaves the session (and the host cron) on a path that
// config storage never recorded.
describe("handleConfig update workspace — persist failure", () => {
  let origConfig: typeof pluginState.resolvedConfig;
  let origManager: typeof pluginState.manager;
  let origReady: typeof pluginState.environmentReady;

  beforeEach(() => {
    origConfig = pluginState.resolvedConfig;
    origManager = pluginState.manager;
    origReady = pluginState.environmentReady;
    pluginState.resolvedConfig = {
      workspace: "/old-ws",
      autoCheckpoint: false,
      cronSchedules: ["0 * * * *"],
    };
    pluginState.manager = {
      ensureWorkspace: vi.fn().mockResolvedValue(true),
    } as any;
    pluginState.environmentReady = true;
  });

  afterEach(() => {
    pluginState.resolvedConfig = origConfig;
    pluginState.manager = origManager;
    pluginState.environmentReady = origReady;
    vi.restoreAllMocks();
  });

  it("keeps the previous workspace and cron entries when persistence fails", async () => {
    const persistSpy = vi.spyOn(persist, "persistConfig").mockReturnValue("disk full");
    const migrateSpy = vi.spyOn(CrontabManager, "migrate").mockResolvedValue([]);

    const result = await handleConfig("update", "workspace", "/new-ws");

    expect(result.isError).toBe(true);
    expect(pluginState.resolvedConfig?.workspace).toBe("/old-ws");
    expect(migrateSpy).not.toHaveBeenCalled();
    expect(pluginState.manager!.ensureWorkspace).not.toHaveBeenCalled();
    persistSpy.mockRestore();
    migrateSpy.mockRestore();
  });

  it("still activates the workspace when persistence succeeds", async () => {
    const persistSpy = vi.spyOn(persist, "persistConfig").mockReturnValue("");
    const migrateSpy = vi.spyOn(CrontabManager, "migrate").mockResolvedValue([]);

    const result = await handleConfig("update", "workspace", "/new-ws");

    expect(result.isError).toBe(false);
    expect(pluginState.resolvedConfig?.workspace).toBe("/new-ws");
    expect(migrateSpy).toHaveBeenCalledWith("/old-ws", "/new-ws", ["0 * * * *"]);
    persistSpy.mockRestore();
    migrateSpy.mockRestore();
  });
});
