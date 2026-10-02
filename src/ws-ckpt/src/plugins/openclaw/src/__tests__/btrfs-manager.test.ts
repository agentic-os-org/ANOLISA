import { describe, it, expect, vi, beforeEach } from "vitest";
import { mapErrorToLLMMessage, BtrfsManager } from "../btrfs-manager.js";
import type { PluginConfig, CommandOutput } from "../types.js";

// ---------------------------------------------------------------------------
// mapErrorToLLMMessage
// ---------------------------------------------------------------------------

describe("mapErrorToLLMMessage", () => {
  it("already exists with id", () => {
    expect(mapErrorToLLMMessage("already exists", { id: "s1" })).toContain("s1");
  });

  it("already exists without id", () => {
    expect(mapErrorToLLMMessage("already exists")).toContain("unknown");
  });

  it("active write", () => {
    expect(mapErrorToLLMMessage("active write")).toContain("retry");
  });

  it("write operations", () => {
    expect(mapErrorToLLMMessage("write operations")).toContain("retry");
  });

  it("insufficient disk", () => {
    expect(mapErrorToLLMMessage("Insufficient disk space")).toContain("disk space");
  });

  it("insufficient lowercase", () => {
    expect(mapErrorToLLMMessage("insufficient")).toContain("disk space");
  });

  it("cwd scan failed", () => {
    const msg = mapErrorToLLMMessage("cwd scan failed");
    expect(msg).toContain("retryable");
  });

  it("have cwd inside workspace", () => {
    const msg = mapErrorToLLMMessage("have cwd inside workspace");
    expect(msg).toContain("NOT retryable");
  });

  it("daemon is not running", () => {
    expect(mapErrorToLLMMessage("daemon is not running")).toContain("not responding");
  });

  it("daemon is starting up", () => {
    expect(mapErrorToLLMMessage("daemon is starting up")).toContain("not responding");
  });

  it("snapshot not found with id", () => {
    const msg = mapErrorToLLMMessage("Snapshot not found", { id: "abc" });
    expect(msg).toContain("abc");
  });

  it("workspace not found", () => {
    expect(mapErrorToLLMMessage("Workspace not found")).toContain("not found");
  });

  it("strips ANSI for generic errors", () => {
    const msg = mapErrorToLLMMessage("\x1b[31mred error\x1b[0m");
    expect(msg).toBe("red error");
  });
});

// ---------------------------------------------------------------------------
// BtrfsManager — unit tests
// ---------------------------------------------------------------------------

describe("BtrfsManager", () => {
  const cfg: PluginConfig = { workspace: "/ws", autoCheckpoint: false };

  it("getWorkspacePath is null before init", () => {
    const mgr = new BtrfsManager(cfg);
    expect(mgr.getWorkspacePath()).toBeNull();
  });

  it("getStore returns SnapshotStore", () => {
    const mgr = new BtrfsManager(cfg);
    expect(mgr.getStore()).toBeDefined();
    expect(mgr.getStore().count).toBe(0);
  });

  it("updateConfig", () => {
    const mgr = new BtrfsManager(cfg);
    mgr.updateConfig({ workspace: "/new", autoCheckpoint: true });
  });

  it("createCheckpoint fails when not initialized", async () => {
    const mgr = new BtrfsManager(cfg);
    const r = await mgr.createCheckpoint({ id: "test" });
    expect(r.success).toBe(false);
    expect(r.message).toContain("not initialized");
  });

  it("rollback fails when not initialized", async () => {
    const mgr = new BtrfsManager(cfg);
    const r = await mgr.rollback("snap1");
    expect(r.success).toBe(false);
    expect(r.message).toContain("not initialized");
  });

  it("listCheckpoints returns empty when not initialized", async () => {
    const mgr = new BtrfsManager(cfg);
    const list = await mgr.listCheckpoints();
    expect(list).toEqual([]);
  });

  it("hasChanges returns false when not initialized", async () => {
    const mgr = new BtrfsManager(cfg);
    expect(await mgr.hasChanges("a", "b")).toBe(false);
  });

  it("execDiffRaw fails when not initialized", async () => {
    const mgr = new BtrfsManager(cfg);
    const r = await mgr.execDiffRaw("a", "b");
    expect(r.success).toBe(false);
    expect(r.text).toContain("not initialized");
  });

  it("cleanup fails when not initialized", async () => {
    const mgr = new BtrfsManager(cfg);
    const r = await mgr.cleanup();
    expect(r.success).toBe(false);
    expect(r.message).toContain("not initialized");
  });

  it("getStatus succeeds even without init", async () => {
    const mgr = new BtrfsManager(cfg);
    // getStatus doesn't require workspacePath, uses optional
    const r = await mgr.getStatus();
    // Will fail because CLI isn't available, but shouldn't throw
    expect(r).toBeDefined();
    expect(typeof r.success).toBe("boolean");
  });
});

// ---------------------------------------------------------------------------
// BtrfsManager with mocked executor
// ---------------------------------------------------------------------------

describe("BtrfsManager with mocked executor", () => {
  const cfg: PluginConfig = { workspace: "/ws", autoCheckpoint: false };

  function ok(stdout = ""): CommandOutput {
    return { exitCode: 0, stdout, stderr: "" };
  }
  function fail(stderr: string): CommandOutput {
    return { exitCode: 1, stdout: "", stderr };
  }

  it("initialize sets workspacePath on success", async () => {
    const mgr = new BtrfsManager(cfg);
    // Mock the executor's init and list
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok("init ok"));
    exec.list = vi.fn().mockResolvedValue(ok("[]"));

    const ok2 = await mgr.initialize("/test/ws");
    expect(ok2).toBe(true);
    expect(mgr.getWorkspacePath()).toBe("/test/ws");
  });

  it("initialize handles AlreadyInitialized", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(fail("AlreadyInitialized"));
    exec.list = vi.fn().mockResolvedValue(ok("[]"));

    const ok2 = await mgr.initialize("/ws");
    expect(ok2).toBe(true);
    expect(mgr.getWorkspacePath()).toBe("/ws");
  });

  it("initialize returns false on real failure", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(fail("permission denied"));

    const r = await mgr.initialize("/ws");
    expect(r).toBe(false);
  });

  it("createCheckpoint success", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.checkpoint = vi.fn().mockResolvedValue(ok("done"));
    const r = await mgr.createCheckpoint({ id: "snap1", message: "test" });
    expect(r.success).toBe(true);
    expect(r.snapshot).toBe("snap1");
  });

  it("createCheckpoint handles error exit", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.checkpoint = vi.fn().mockResolvedValue(fail("already exists"));
    const r = await mgr.createCheckpoint({ id: "dup" });
    expect(r.success).toBe(false);
  });

  it("createCheckpoint detects skipped (empty workspace)", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.checkpoint = vi.fn().mockResolvedValue({
      exitCode: 0,
      stdout: "",
      stderr: "\u001b[33m⚠ Empty workspace, no snapshot created.\u001b[0m\n",
    });
    const r = await mgr.createCheckpoint({ id: "s1" });
    expect(r.success).toBe(true);
    expect(r.skipped).toBe(true);
    expect(mgr.getStore().getAll()).toHaveLength(0);
  });

  it("createCheckpoint handles exception", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.checkpoint = vi.fn().mockRejectedValue(new Error("boom"));
    const r = await mgr.createCheckpoint({ id: "s1" });
    expect(r.success).toBe(false);
    expect(r.message).toContain("boom");
  });

  it("createCheckpoint with metadata", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.checkpoint = vi.fn().mockResolvedValue(ok("done"));
    const r = await mgr.createCheckpoint({
      id: "s1",
      message: "m",
      metadata: '{"a":1}',
    });
    expect(r.success).toBe(true);
    expect(mgr.getStore().count).toBe(1);
  });

  it("createCheckpoint with invalid metadata JSON", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.checkpoint = vi.fn().mockResolvedValue(ok("done"));
    const r = await mgr.createCheckpoint({ id: "s1", metadata: "not-json" });
    expect(r.success).toBe(true);
  });

  it("rollback success", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.rollback = vi.fn().mockResolvedValue(ok("rolled back"));
    const r = await mgr.rollback("snap1");
    expect(r.success).toBe(true);
    expect(r.target).toBe("snap1");
  });

  it("rollback preview returns diff without refreshing cache", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");
    exec.list.mockClear();

    exec.rollback = vi.fn().mockResolvedValue(ok("\u001b[1mRollback preview\u001b[0m\nM  file.txt"));
    const r = await mgr.rollback("snap1", undefined, true);
    expect(r.success).toBe(true);
    expect(r.message).toContain("M  file.txt");
    expect(exec.rollback).toHaveBeenCalledWith("/ws", "snap1", undefined, true);
    expect(exec.list).not.toHaveBeenCalled();
  });

  it("rollback failure", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.rollback = vi.fn().mockResolvedValue(fail("Snapshot not found"));
    const r = await mgr.rollback("bad");
    expect(r.success).toBe(false);
  });

  it("rollback exception", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.rollback = vi.fn().mockRejectedValue(new Error("kaboom"));
    const r = await mgr.rollback("snap1");
    expect(r.success).toBe(false);
    expect(r.message).toContain("kaboom");
  });

  it("listCheckpoints parses JSON", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(
      ok(
        JSON.stringify([
          { snapshot: "s1", meta: { created_at: "2024-01-01T00:00:00Z", message: "m" } },
        ]),
      ),
    );
    await mgr.initialize("/ws");

    const list = await mgr.listCheckpoints();
    expect(list).toHaveLength(1);
    expect(list[0].snapshot).toBe("s1");
  });

  it("listCheckpoints preserves summary markers", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(
      ok(
        JSON.stringify([
          {
            id: "s-large",
            workspace: "/ws",
            meta: {
              created_at: "2024-01-01T00:00:00Z",
              pinned: true,
              missing: false,
            },
            detail: "summary",
            omitted_fields: ["message", "metadata"],
          },
        ]),
      ),
    );
    await mgr.initialize("/ws");

    const list = await mgr.listCheckpoints();
    expect(list).toEqual([
      expect.objectContaining({
        snapshot: "s-large",
        createdAt: "2024-01-01T00:00:00Z",
        message: undefined,
        metadata: undefined,
        detail: "summary",
        omittedFields: ["message", "metadata"],
      }),
    ]);
  });

  it("listCheckpoints surfaces CLI failure", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn()
      .mockResolvedValueOnce(ok("[]"))  // init
      .mockResolvedValueOnce(fail("err"));  // listCheckpoints
    await mgr.initialize("/ws");

    await expect(mgr.listCheckpoints()).rejects.toThrow("Failed to list checkpoints");
  });

  it("listCheckpoints does not return stale cache after CLI failure", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi
      .fn()
      .mockResolvedValueOnce(
        ok(JSON.stringify([{ id: "cached", meta: { created_at: "2024-01-01T00:00:00Z" } }])),
      )
      .mockResolvedValueOnce(fail("later page failed"));
    await mgr.initialize("/ws");

    expect((mgr as any).store.getAll()[0]?.snapshot).toBe("cached");
    await expect(mgr.listCheckpoints()).rejects.toThrow("Failed to list checkpoints");
  });

  it("listCheckpoints surfaces exception", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn()
      .mockResolvedValueOnce(ok("[]"))
      .mockRejectedValueOnce(new Error("fail"));
    await mgr.initialize("/ws");

    await expect(mgr.listCheckpoints()).rejects.toThrow("Failed to list checkpoints: fail");
  });

  it("execDiffRaw success", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.diff = vi.fn().mockResolvedValue(ok("file.txt changed"));
    const r = await mgr.execDiffRaw("a", "b");
    expect(r.success).toBe(true);
    expect(r.text).toContain("file.txt");
  });

  it("execDiffRaw no changes", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.diff = vi.fn().mockResolvedValue(ok(""));
    const r = await mgr.execDiffRaw("a", "b");
    expect(r.success).toBe(true);
    expect(r.text).toContain("No changes");
  });

  it("execDiffRaw failure", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.diff = vi.fn().mockResolvedValue(fail("error"));
    const r = await mgr.execDiffRaw("a", "b");
    expect(r.success).toBe(false);
  });

  it("execDiffRaw exception", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.diff = vi.fn().mockRejectedValue(new Error("err"));
    const r = await mgr.execDiffRaw("a", "b");
    expect(r.success).toBe(false);
  });

  it("hasChanges true when changes exist", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.diff = vi.fn().mockResolvedValue(ok("file.txt modified"));
    expect(await mgr.hasChanges("a", "b")).toBe(true);
  });

  it("hasChanges false when no differences", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.diff = vi.fn().mockResolvedValue(ok("No differences found."));
    expect(await mgr.hasChanges("a", "b")).toBe(false);
  });

  it("hasChanges true on failure (fail-closed)", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.diff = vi.fn().mockResolvedValue(fail("error"));
    expect(await mgr.hasChanges("a", "b")).toBe(true);
  });

  it("hasChanges true on exception (fail-closed)", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.diff = vi.fn().mockRejectedValue(new Error("err"));
    expect(await mgr.hasChanges("a", "b")).toBe(true);
  });

  it("getStatus success", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.status = vi.fn().mockResolvedValue(ok("daemon ok"));
    const r = await mgr.getStatus();
    expect(r.success).toBe(true);
    expect(r.daemonRunning).toBe(true);
  });

  it("getStatus failure", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.status = vi.fn().mockResolvedValue(fail("daemon is not running"));
    const r = await mgr.getStatus();
    expect(r.success).toBe(false);
    expect(r.daemonRunning).toBe(false);
  });

  it("getStatus exception", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.status = vi.fn().mockRejectedValue(new Error("err"));
    const r = await mgr.getStatus();
    expect(r.success).toBe(false);
  });

  it("cleanup success", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.cleanup = vi.fn().mockResolvedValue(ok("cleaned"));
    const r = await mgr.cleanup(5);
    expect(r.success).toBe(true);
  });

  it("cleanup failure", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.cleanup = vi.fn().mockResolvedValue(fail("error"));
    const r = await mgr.cleanup();
    expect(r.success).toBe(false);
  });

  it("cleanup exception", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));
    await mgr.initialize("/ws");

    exec.cleanup = vi.fn().mockRejectedValue(new Error("boom"));
    const r = await mgr.cleanup();
    expect(r.success).toBe(false);
    expect(r.message).toContain("boom");
  });

  it("ensureWorkspace when already initialized (status ok)", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.status = vi.fn().mockResolvedValue(ok("ok"));
    exec.list = vi.fn().mockResolvedValue(ok("[]"));

    const r = await mgr.ensureWorkspace("/ws");
    expect(r).toBe(true);
    expect(mgr.getWorkspacePath()).toBe("/ws");
  });

  it("ensureWorkspace falls back to init when status fails", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.status = vi.fn().mockResolvedValue(fail("not init"));
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok("[]"));

    const r = await mgr.ensureWorkspace("/ws");
    expect(r).toBe(true);
  });

  it("parseSnapshotList handles empty stdout", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn().mockResolvedValue(ok(""));
    await mgr.initialize("/ws");

    const list = await mgr.listCheckpoints();
    expect(list).toEqual([]);
  });

  it("parseSnapshotList handles invalid JSON", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn()
      .mockResolvedValueOnce(ok("[]"))  // init
      .mockResolvedValueOnce(ok("not json"));  // listCheckpoints
    await mgr.initialize("/ws");

    const list = await mgr.listCheckpoints();
    expect(list).toEqual([]);
  });

  it("parseSnapshotList handles non-array JSON", async () => {
    const mgr = new BtrfsManager(cfg);
    const exec = (mgr as any).executor;
    exec.init = vi.fn().mockResolvedValue(ok());
    exec.list = vi.fn()
      .mockResolvedValueOnce(ok("[]"))
      .mockResolvedValueOnce(ok('{"not": "array"}'));
    await mgr.initialize("/ws");

    const list = await mgr.listCheckpoints();
    expect(list).toEqual([]);
  });
});
