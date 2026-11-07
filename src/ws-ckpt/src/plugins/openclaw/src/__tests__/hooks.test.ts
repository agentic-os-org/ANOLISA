import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../cron.js", () => ({
  CrontabManager: { syncWithRetry: vi.fn(async () => true) },
}));

const { pluginState } = await import("../state.js");
const { registerHooks } = await import("../hooks.js");
const { CrontabManager } = await import("../cron.js");

function registeredHandlers() {
  const handlers: Record<string, (event?: unknown) => Promise<void> | void> = {};
  const api = {
    on: (name: string, cb: (event?: unknown) => Promise<void> | void) => {
      handlers[name] = cb;
    },
  };
  return { handlers, api };
}

describe("session_start cron sync", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    pluginState.resolvedConfig = { workspace: "/srv/proj" } as never;
    pluginState.manager = null;
    pluginState.environmentReady = false;
  });

  it("re-syncs an empty schedule list so stale crontab lines are removed", async () => {
    const { handlers, api } = registeredHandlers();
    registerHooks(api as never, { autoCheckpoint: false, cronSchedules: [] } as never);
    await handlers["session_start"]();
    expect(CrontabManager.syncWithRetry).toHaveBeenCalledWith("/srv/proj", []);
  });

  it("syncs once per session start with the configured schedules", async () => {
    const { handlers, api } = registeredHandlers();
    registerHooks(api as never, { autoCheckpoint: false, cronSchedules: ["0 * * * *"] } as never);
    await handlers["session_start"]();
    expect(CrontabManager.syncWithRetry).toHaveBeenCalledTimes(1);
    expect(CrontabManager.syncWithRetry).toHaveBeenCalledWith("/srv/proj", ["0 * * * *"]);
  });

  it("does not touch the crontab without a configured workspace", async () => {
    pluginState.resolvedConfig = {} as never;
    const { handlers, api } = registeredHandlers();
    registerHooks(api as never, { autoCheckpoint: false, cronSchedules: [] } as never);
    await handlers["session_start"]();
    expect(CrontabManager.syncWithRetry).not.toHaveBeenCalled();
  });
});
