import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { BtrfsManager } from "../btrfs-manager.js";
import { handleListCheckpoints } from "../handlers.js";
import { pluginState } from "../state.js";

const good = [{ snapshot: "snapshot-a", meta: { created_at: "2026-10-05T00:00:00Z", message: "original" } }];
const ok = (stdout: string) => ({ exitCode: 0, stdout, stderr: "" });
let manager: BtrfsManager;
let executor: { init: ReturnType<typeof vi.fn>; list: ReturnType<typeof vi.fn>; rollback: ReturnType<typeof vi.fn> };

beforeEach(async () => {
  manager = new BtrfsManager({ workspace: "/ws", autoCheckpoint: false });
  executor = {
    init: vi.fn().mockResolvedValue(ok("")),
    list: vi.fn().mockResolvedValue(ok(JSON.stringify(good))),
    rollback: vi.fn().mockResolvedValue(ok("rolled back")),
  };
  (manager as any).executor = executor;
  await manager.initialize("/ws");
  pluginState.manager = manager;
  pluginState.environmentReady = true;
});

afterEach(() => {
  pluginState.manager = null;
  pluginState.environmentReady = false;
});

it.each(["{truncated", "", "null", "{}", '"not a list"', "[null]", "[{}]"])(
  "rejects invalid CLI list %s and preserves the last valid cache",
  async (stdout) => {
    const cached = manager.getStore().getAll();
    executor.list.mockResolvedValue(ok(stdout));
    await expect(manager.listCheckpoints()).rejects.toThrow("Failed to list checkpoints:");
    expect(manager.getStore().getAll()).toEqual(cached);
    const response = await handleListCheckpoints();
    expect(response.isError).toBe(true);
    expect(response.text).toContain("Failed to list checkpoints:");
    expect(response.text).not.toContain("No checkpoints");
  },
);

it("accepts a genuine empty JSON list and replaces the valid cache", async () => {
  executor.list.mockResolvedValue(ok("[]"));
  expect(await manager.listCheckpoints()).toEqual([]);
  expect(manager.getStore().count).toBe(0);
  expect((await handleListCheckpoints()).isError).toBe(false);
});

it("retains nested summary fields and supported legacy top-level entries", async () => {
  executor.list.mockResolvedValue(ok(JSON.stringify([
    { snapshot: "new", meta: { message: "nested", created_at: "2026-10-05T00:00:00Z", metadata: { kind: "new" } }, detail: "summary", omitted_fields: ["metadata"] },
    { id: "legacy", message: "top-level", createdAt: "2026-10-04T00:00:00Z" },
  ])));
  const entries = await manager.listCheckpoints();
  expect(entries.find((entry) => entry.snapshot === "new")).toMatchObject({ message: "nested", metadata: { kind: "new" }, detail: "summary", omittedFields: ["metadata"] });
  expect(entries.find((entry) => entry.snapshot === "legacy")).toMatchObject({ message: "top-level", createdAt: "2026-10-04T00:00:00Z", detail: "full" });
});

it("keeps nonzero CLI exits as errors without parsing their stdout", async () => {
  executor.list.mockResolvedValue({ exitCode: 1, stdout: "[]", stderr: "daemon is not running" });
  const response = await handleListCheckpoints();
  expect(response.isError).toBe(true);
  expect(response.text).toContain("daemon");
  expect(manager.getStore().getAll()[0].snapshot).toBe("snapshot-a");
});

it("does not erase the cache when a successful rollback's refresh is malformed", async () => {
  executor.list.mockResolvedValue(ok("{truncated"));
  expect((await manager.rollback("snapshot-a")).success).toBe(true);
  expect(manager.getStore().getAll()[0].snapshot).toBe("snapshot-a");
});
