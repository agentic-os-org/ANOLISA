import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Mock child_process so CommandExecutor (used in explicit-workspace paths) works
vi.mock("child_process", () => {
  const sym = Symbol.for("nodejs.util.promisify.custom");
  const promisifiedFn = vi.fn();
  const fn = vi.fn();
  (fn as any)[sym] = promisifiedFn;
  return { execFile: fn };
});

import { execFile } from "child_process";
import {
  textToolResult,
  handleCheckpoint,
  handleRollback,
  handleListCheckpoints,
  handleDelete,
  handleDiff,
  handleStatus,
  handleConfig,
} from "../handlers.js";
import { pluginState, UNAVAILABLE_MSG } from "../state.js";
import { CrontabManager } from "../cron.js";
import type { BtrfsManager } from "../btrfs-manager.js";

const promisifiedMock = (execFile as any)[
  Symbol.for("nodejs.util.promisify.custom")
] as ReturnType<typeof vi.fn>;

// ---------------------------------------------------------------------------
// textToolResult
// ---------------------------------------------------------------------------

describe("textToolResult", () => {
  it("wraps text in content array", () => {
    const r = textToolResult("hello");
    expect(r.content).toEqual([{ type: "text", text: "hello" }]);
    expect(r.details).toBeUndefined();
  });

  it("sets status failed when isError", () => {
    const r = textToolResult("oops", true);
    expect(r.details).toEqual({ status: "failed" });
  });

  it("no details when isError is false", () => {
    const r = textToolResult("ok", false);
    expect(r.details).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// Handler tests with pluginState mocked
// ---------------------------------------------------------------------------

describe("handlers — unavailable state", () => {
  let origManager: typeof pluginState.manager;
  let origReady: typeof pluginState.environmentReady;
  let origConfig: typeof pluginState.resolvedConfig;

  beforeEach(() => {
    origManager = pluginState.manager;
    origReady = pluginState.environmentReady;
    origConfig = pluginState.resolvedConfig;
    pluginState.manager = null;
    pluginState.environmentReady = false;
    pluginState.resolvedConfig = null;
  });

  afterEach(() => {
    pluginState.manager = origManager;
    pluginState.environmentReady = origReady;
    pluginState.resolvedConfig = origConfig;
  });

  it("handleCheckpoint returns unavailable", async () => {
    const r = await handleCheckpoint();
    expect(r.isError).toBe(true);
    expect(r.text).toBe(UNAVAILABLE_MSG);
  });

  it("handleRollback returns unavailable", async () => {
    const r = await handleRollback();
    expect(r.isError).toBe(true);
    expect(r.text).toBe(UNAVAILABLE_MSG);
  });

  it("handleListCheckpoints returns unavailable", async () => {
    const r = await handleListCheckpoints();
    expect(r.isError).toBe(true);
    expect(r.text).toBe(UNAVAILABLE_MSG);
  });

  it("handleDelete returns unavailable", async () => {
    const r = await handleDelete();
    expect(r.isError).toBe(true);
    expect(r.text).toBe(UNAVAILABLE_MSG);
  });

  it("handleDiff returns unavailable", async () => {
    const r = await handleDiff();
    expect(r.isError).toBe(true);
    expect(r.text).toBe(UNAVAILABLE_MSG);
  });

  it("handleStatus returns unavailable", async () => {
    const r = await handleStatus();
    expect(r.isError).toBe(true);
    expect(r.text).toBe(UNAVAILABLE_MSG);
  });

  it("handleConfig returns unavailable when no resolvedConfig", async () => {
    const r = await handleConfig();
    expect(r.isError).toBe(true);
    expect(r.text).toBe(UNAVAILABLE_MSG);
  });
});

// ---------------------------------------------------------------------------
// Handler validation tests (with mock manager)
// ---------------------------------------------------------------------------

describe("handlers — validation", () => {
  let origManager: typeof pluginState.manager;
  let origReady: typeof pluginState.environmentReady;
  let origConfig: typeof pluginState.resolvedConfig;
  let mockManager: any;

  beforeEach(() => {
    vi.clearAllMocks();
    origManager = pluginState.manager;
    origReady = pluginState.environmentReady;
    origConfig = pluginState.resolvedConfig;

    mockManager = {
      createCheckpoint: vi.fn(),
      rollback: vi.fn(),
      listCheckpoints: vi.fn(),
      execDiffRaw: vi.fn(),
      getStatus: vi.fn(),
      getStore: vi.fn().mockReturnValue({ remove: vi.fn() }),
    };
    pluginState.manager = mockManager as unknown as BtrfsManager;
    pluginState.environmentReady = true;
    pluginState.resolvedConfig = { workspace: "/ws", autoCheckpoint: false };
  });

  afterEach(() => {
    pluginState.manager = origManager;
    pluginState.environmentReady = origReady;
    pluginState.resolvedConfig = origConfig;
    pluginState.skipNextAutoCheckpoint = false;
  });

  it("handleCheckpoint requires id", async () => {
    const r = await handleCheckpoint(JSON.stringify({}));
    expect(r.isError).toBe(true);
    expect(r.text).toContain("id");
  });

  it("handleCheckpoint success via manager", async () => {
    mockManager.createCheckpoint.mockResolvedValue({
      success: true,
      message: "Checkpoint created: snap1",
      snapshot: "snap1",
    });
    const r = await handleCheckpoint(JSON.stringify({ id: "snap1" }));
    expect(r.isError).toBe(false);
    expect(r.text).toContain("snap1");
  });

  it("handleCheckpoint skipped", async () => {
    mockManager.createCheckpoint.mockResolvedValue({
      success: true,
      skipped: true,
      reason: "Empty workspace",
      message: "Empty workspace",
    });
    const r = await handleCheckpoint(JSON.stringify({ id: "snap1" }));
    expect(r.isError).toBe(false);
    expect(r.text).toContain("Empty workspace");
  });

  it("handleRollback requires target", async () => {
    const r = await handleRollback();
    expect(r.isError).toBe(true);
    expect(r.text).toContain("target");
  });

  it("handleRollback success via manager", async () => {
    mockManager.rollback.mockResolvedValue({
      success: true,
      target: "snap1",
      message: "Rolled back to snap1",
    });
    const r = await handleRollback("snap1");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("snap1");
  });

  it("handleRollback sets skipNextAutoCheckpoint on success", async () => {
    mockManager.rollback.mockResolvedValue({
      success: true,
      target: "snap1",
      message: "Rolled back to snap1",
    });
    pluginState.skipNextAutoCheckpoint = false;
    await handleRollback("snap1");
    expect(pluginState.skipNextAutoCheckpoint).toBe(true);
  });

  it("handleRollback does not set skipNextAutoCheckpoint on preview", async () => {
    mockManager.rollback.mockResolvedValue({
      success: true,
      target: "snap1",
      message: "Preview",
    });
    pluginState.skipNextAutoCheckpoint = false;
    await handleRollback("snap1", undefined, undefined, true);
    expect(pluginState.skipNextAutoCheckpoint).toBe(false);
  });

  it("handleRollback does not set skipNextAutoCheckpoint on failure", async () => {
    mockManager.rollback.mockResolvedValue({
      success: false,
      message: "failed",
    });
    pluginState.skipNextAutoCheckpoint = false;
    await handleRollback("snap1");
    expect(pluginState.skipNextAutoCheckpoint).toBe(false);
  });

  it("handleListCheckpoints empty", async () => {
    mockManager.listCheckpoints.mockResolvedValue([]);
    const r = await handleListCheckpoints();
    expect(r.isError).toBe(false);
    expect(r.text).toContain("No checkpoints");
  });

  it("handleListCheckpoints reports list failure", async () => {
    mockManager.listCheckpoints.mockRejectedValue(new Error("page two failed"));
    const r = await handleListCheckpoints();
    expect(r.isError).toBe(true);
    expect(r.text).toContain("page two failed");
  });

  it("handleListCheckpoints with data", async () => {
    mockManager.listCheckpoints.mockResolvedValue([
      { snapshot: "s1", createdAt: "2024-01-01T00:00:00Z", message: "first" },
      { snapshot: "s2", createdAt: "2024-01-02T00:00:00Z" },
    ]);
    const r = await handleListCheckpoints();
    expect(r.isError).toBe(false);
    expect(r.text).toContain("s1");
    expect(r.text).toContain("s2");
  });

  it("handleDelete requires snapshot", async () => {
    const r = await handleDelete();
    expect(r.isError).toBe(true);
    expect(r.text).toContain("snapshot");
  });

  it("handleDiff requires from", async () => {
    const r = await handleDiff();
    expect(r.isError).toBe(true);
    expect(r.text).toContain("from");
  });

  it("handleDiff success", async () => {
    mockManager.execDiffRaw.mockResolvedValue({
      success: true,
      text: "file.txt changed",
    });
    const r = await handleDiff("a", "b");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("file.txt");
  });

  it("handleStatus success", async () => {
    mockManager.getStatus.mockResolvedValue({
      success: true,
      daemonRunning: true,
      message: "daemon ok",
    });
    const r = await handleStatus();
    expect(r.isError).toBe(false);
    expect(r.text).toContain("daemon ok");
  });

  it("handleStatus failure", async () => {
    mockManager.getStatus.mockResolvedValue({
      success: false,
      daemonRunning: false,
      message: "daemon down",
    });
    const r = await handleStatus();
    expect(r.isError).toBe(true);
    expect(r.text).toContain("daemon down");
  });
});

// ---------------------------------------------------------------------------
// Explicit workspace paths (checkpoint / rollback / delete)
// ---------------------------------------------------------------------------

describe("handlers — explicit workspace", () => {
  let origManager: typeof pluginState.manager;
  let origReady: typeof pluginState.environmentReady;
  let origConfig: typeof pluginState.resolvedConfig;
  let origCwd: () => string;

  beforeEach(() => {
    vi.clearAllMocks();
    origManager = pluginState.manager;
    origReady = pluginState.environmentReady;
    origConfig = pluginState.resolvedConfig;
    origCwd = process.cwd;

    pluginState.manager = {
      createCheckpoint: vi.fn(),
      rollback: vi.fn(),
      listCheckpoints: vi.fn(),
      getStore: vi.fn().mockReturnValue({ remove: vi.fn() }),
      execDiffRaw: vi.fn(),
      getStatus: vi.fn(),
    } as any;
    pluginState.environmentReady = true;
    pluginState.resolvedConfig = { workspace: "/ws", autoCheckpoint: false };

    // Ensure cwd is outside workspace
    process.cwd = () => "/home/user";
  });

  afterEach(() => {
    pluginState.manager = origManager;
    pluginState.environmentReady = origReady;
    pluginState.resolvedConfig = origConfig;
    process.cwd = origCwd;
  });

  // --- checkpoint with explicit workspace ---
  it("checkpoint explicit workspace success", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "done", stderr: "" });
    const r = await handleCheckpoint(
      JSON.stringify({ id: "snap1", workspace: "/explicit" }),
    );
    expect(r.isError).toBe(false);
    expect(r.text).toContain("snap1");
  });

  it("checkpoint explicit workspace CLI failure", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "already exists";
    promisifiedMock.mockRejectedValue(err);
    const r = await handleCheckpoint(
      JSON.stringify({ id: "snap1", workspace: "/explicit" }),
    );
    expect(r.isError).toBe(true);
  });

  it("checkpoint explicit workspace exception", async () => {
    promisifiedMock.mockRejectedValue(new Error("boom"));
    const r = await handleCheckpoint(
      JSON.stringify({ id: "snap1", workspace: "/explicit" }),
    );
    expect(r.isError).toBe(true);
    expect(r.text).toContain("boom");
  });

  it("checkpoint explicit workspace skipped (empty)", async () => {
    promisifiedMock.mockResolvedValue({
      stdout: "",
      stderr: "\u001b[33m⚠ Empty workspace, no snapshot created.\u001b[0m\n",
    });
    const r = await handleCheckpoint(
      JSON.stringify({ id: "snap1", workspace: "/explicit" }),
    );
    expect(r.isError).toBe(false);
    expect(r.text).toContain("Empty workspace");
  });

  it("rollback explicit workspace preview", async () => {
    promisifiedMock.mockResolvedValue({
      stdout: "\u001b[1mRollback preview\u001b[0m\nM  file.txt\n",
      stderr: "",
    });
    const r = await handleRollback("snap1", "/explicit", undefined, true);
    expect(r.isError).toBe(false);
    expect(r.text).toContain("M  file.txt");
    const args = promisifiedMock.mock.calls[0][1];
    expect(args).toContain("--preview");
  });

  it("checkpoint explicit workspace cwd inside", async () => {
    process.cwd = () => "/explicit/sub";
    const r = await handleCheckpoint(
      JSON.stringify({ id: "snap1", workspace: "/explicit" }),
    );
    expect(r.isError).toBe(true);
    expect(r.text).toContain("Refused");
  });

  // --- rollback with explicit workspace ---
  it("rollback explicit workspace success", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "rolled back", stderr: "" });
    const r = await handleRollback("snap1", "/explicit");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("snap1");
  });

  it("rollback explicit workspace CLI failure", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "Snapshot not found";
    promisifiedMock.mockRejectedValue(err);
    const r = await handleRollback("snap1", "/explicit");
    expect(r.isError).toBe(true);
  });

  it("rollback explicit workspace exception", async () => {
    promisifiedMock.mockRejectedValue(new Error("kaboom"));
    const r = await handleRollback("snap1", "/explicit");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("kaboom");
  });

  it("rollback explicit workspace cwd inside", async () => {
    process.cwd = () => "/explicit";
    const r = await handleRollback("snap1", "/explicit");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("Refused");
  });

  // --- rollback cwd inside default workspace ---
  it("rollback cwd inside default workspace", async () => {
    process.cwd = () => "/ws/sub";
    const r = await handleRollback("snap1");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("Refused");
  });

  // --- checkpoint cwd inside default workspace ---
  it("checkpoint cwd inside default workspace", async () => {
    process.cwd = () => "/ws/sub";
    const r = await handleCheckpoint(JSON.stringify({ id: "snap1" }));
    expect(r.isError).toBe(true);
    expect(r.text).toContain("Refused");
  });

  // --- delete success ---
  it("delete success via CommandExecutor", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "deleted", stderr: "" });
    const r = await handleDelete("snap1", "/ws");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("snap1");
  });

  it("delete CLI failure", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "Snapshot not found";
    promisifiedMock.mockRejectedValue(err);
    const r = await handleDelete("snap1", "/ws");
    expect(r.isError).toBe(true);
  });

  it("delete exception", async () => {
    promisifiedMock.mockRejectedValue(new Error("boom"));
    const r = await handleDelete("snap1", "/ws");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("boom");
  });

  it("delete no workspace available", async () => {
    pluginState.resolvedConfig = { workspace: "", autoCheckpoint: false };
    const r = await handleDelete("snap1");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("No workspace");
  });
});

// ---------------------------------------------------------------------------
// handleConfig tests
// ---------------------------------------------------------------------------

describe("handleConfig", () => {
  let origConfig: typeof pluginState.resolvedConfig;
  let origCwd: () => string;

  beforeEach(() => {
    vi.clearAllMocks();
    origConfig = pluginState.resolvedConfig;
    origCwd = process.cwd;
    pluginState.resolvedConfig = { workspace: "/ws", autoCheckpoint: false };
    process.cwd = () => "/home/user";
  });

  afterEach(() => {
    pluginState.resolvedConfig = origConfig;
    process.cwd = origCwd;
  });

  it("unknown action returns error", async () => {
    const r = await handleConfig("destroy");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("Unknown action");
  });

  it("update without key returns error", async () => {
    const r = await handleConfig("update");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("key");
  });

  it("update unknown key returns error", async () => {
    const r = await handleConfig("update", "unknownKey", "val");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("Unknown config key");
  });

  it("update autoCheckpoint without value returns error", async () => {
    const r = await handleConfig("update", "autoCheckpoint");
    expect(r.isError).toBe(true);
  });

  it("update autoCheckpoint with invalid value returns error", async () => {
    const r = await handleConfig("update", "autoCheckpoint", "maybe");
    expect(r.isError).toBe(true);
  });

  it("update autoCheckpoint true", async () => {
    const r = await handleConfig("update", "autoCheckpoint", "true");
    expect(r.isError).toBe(false);
    expect(pluginState.resolvedConfig!.autoCheckpoint).toBe(true);
  });

  it("update autoCheckpoint false", async () => {
    pluginState.resolvedConfig!.autoCheckpoint = true;
    const r = await handleConfig("update", "autoCheckpoint", "false");
    expect(r.isError).toBe(false);
    expect(pluginState.resolvedConfig!.autoCheckpoint).toBe(false);
  });

  it("update autoCheckpoint accepts aliases", async () => {
    for (const val of ["1", "yes", "on", "enabled"]) {
      pluginState.resolvedConfig!.autoCheckpoint = false;
      const r = await handleConfig("update", "autoCheckpoint", val);
      expect(r.isError).toBe(false);
      expect(pluginState.resolvedConfig!.autoCheckpoint).toBe(true);
    }
  });

  it("update autoCheckpoint true with cwd inside workspace", async () => {
    process.cwd = () => "/ws/sub";
    const r = await handleConfig("update", "autoCheckpoint", "true");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("Refused");
  });

  it("update workspace without value returns error", async () => {
    const r = await handleConfig("update", "workspace");
    expect(r.isError).toBe(true);
  });

  it("update workspace", async () => {
    const migrateSpy = vi.spyOn(CrontabManager, "migrate").mockResolvedValue([]);
    const persistSpy = vi.spyOn(await import("../persist.js"), "persistConfig").mockReturnValue("");
    const r = await handleConfig("update", "workspace", "/new/path");
    expect(r.isError).toBe(false);
    expect(pluginState.resolvedConfig!.workspace).toBe("/new/path");
    migrateSpy.mockRestore();
    persistSpy.mockRestore();
  });

  it("update maxSnapshotsNum without value returns error", async () => {
    const r = await handleConfig("update", "maxSnapshotsNum");
    expect(r.isError).toBe(true);
  });

  it("update maxSnapshotsNum invalid returns error", async () => {
    const r = await handleConfig("update", "maxSnapshotsNum", "abc");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("positive integer");
  });

  it("update maxSnapshotsNum success", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "ok", stderr: "" });
    const r = await handleConfig("update", "maxSnapshotsNum", "10");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("10");
  });

  it("update maxSnapshotsNum CLI failure", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "error";
    promisifiedMock.mockRejectedValue(err);
    const r = await handleConfig("update", "maxSnapshotsNum", "10");
    expect(r.isError).toBe(true);
  });

  it("update maxSnapshotsNum unset", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "reset", stderr: "" });
    const r = await handleConfig("update", "maxSnapshotsNum", "unset");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("unset");
  });

  it("update maxSnapshotsNum unset failure", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "error";
    promisifiedMock.mockRejectedValue(err);
    const r = await handleConfig("update", "maxSnapshotsNum", "unset");
    expect(r.isError).toBe(true);
  });

  it("update maxSnapshotsDuration without value returns error", async () => {
    const r = await handleConfig("update", "maxSnapshotsDuration");
    expect(r.isError).toBe(true);
  });

  it("update maxSnapshotsDuration success", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "ok", stderr: "" });
    const r = await handleConfig("update", "maxSnapshotsDuration", "7d");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("7d");
  });

  it("update maxSnapshotsDuration unset", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "reset", stderr: "" });
    const r = await handleConfig("update", "maxSnapshotsDuration", "unset");
    expect(r.isError).toBe(false);
  });

  it("update maxSnapshotsDuration unset failure", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "error";
    promisifiedMock.mockRejectedValue(err);
    const r = await handleConfig("update", "maxSnapshotsDuration", "unset");
    expect(r.isError).toBe(true);
  });

  it("update maxSnapshotsDuration CLI failure", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "error";
    promisifiedMock.mockRejectedValue(err);
    const r = await handleConfig("update", "maxSnapshotsDuration", "7d");
    expect(r.isError).toBe(true);
  });

  it("set alias works like update", async () => {
    const r = await handleConfig("set", "workspace");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("requires");
  });

  // --- view action ---
  it("view with daemon success (disabled policy)", async () => {
    const policyJson = JSON.stringify({
      schema: "ws-ckpt-policy/v1",
      effective: { is_disabled: true },
    });
    promisifiedMock.mockResolvedValue({ stdout: policyJson, stderr: "" });
    const r = await handleConfig("view");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("auto-cleanup disabled");
  });

  it("view with daemon success (count policy)", async () => {
    const policyJson = JSON.stringify({
      schema: "ws-ckpt-policy/v1",
      effective: {
        is_disabled: false,
        auto_cleanup_keep: { mode: "count", count: 5 },
      },
    });
    promisifiedMock.mockResolvedValue({ stdout: policyJson, stderr: "" });
    const r = await handleConfig("view");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("5");
  });

  it("view with daemon success (age policy)", async () => {
    const policyJson = JSON.stringify({
      schema: "ws-ckpt-policy/v1",
      effective: {
        is_disabled: false,
        auto_cleanup_keep: { mode: "age", raw: "7d" },
      },
    });
    promisifiedMock.mockResolvedValue({ stdout: policyJson, stderr: "" });
    const r = await handleConfig("view");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("7d");
  });

  it("view with daemon unreachable", async () => {
    const err: any = new Error("fail");
    err.code = 1;
    err.stdout = "";
    err.stderr = "daemon not running";
    promisifiedMock.mockRejectedValue(err);
    const r = await handleConfig("view");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("daemon unreachable");
  });

  it("view with unparseable daemon response", async () => {
    promisifiedMock.mockResolvedValue({ stdout: "not json", stderr: "" });
    const r = await handleConfig("view");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("unparseable");
  });

  it("view default action", async () => {
    const policyJson = JSON.stringify({
      schema: "ws-ckpt-policy/v1",
      effective: { is_disabled: true },
    });
    promisifiedMock.mockResolvedValue({ stdout: policyJson, stderr: "" });
    const r = await handleConfig();
    expect(r.isError).toBe(false);
    expect(r.text).toContain("autoCheckpoint");
  });

  it("view with cronSchedules configured", async () => {
    pluginState.resolvedConfig = {
      workspace: "/ws",
      autoCheckpoint: false,
      cronSchedules: ["0 * * * *"],
    };
    const policyJson = JSON.stringify({
      schema: "ws-ckpt-policy/v1",
      effective: { is_disabled: true },
    });
    promisifiedMock.mockResolvedValue({ stdout: policyJson, stderr: "" });
    const r = await handleConfig("view");
    expect(r.isError).toBe(false);
    expect(r.text).toContain("0 * * * *");
  });

  it("cronSchedules no value returns error", async () => {
    const r = await handleConfig("update", "cronSchedules");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("requires a value");
  });

  it("cronSchedules no workspace returns error", async () => {
    pluginState.resolvedConfig = { workspace: "", autoCheckpoint: false };
    const r = await handleConfig("update", "cronSchedules", 'add "0 * * * *"');
    expect(r.isError).toBe(true);
    expect(r.text).toContain("No workspace");
  });

  it("cronSchedules add invalid returns error", async () => {
    const r = await handleConfig("update", "cronSchedules", "add bad");
    expect(r.isError).toBe(true);
    expect(r.text).toContain("Invalid cron");
  });

  it("cronSchedules add success", async () => {
    // Mock the persist and crontab functions
    const { persistConfig: origPersist } = await import("../persist.js");
    const { CrontabManager: origCron } = await import("../cron.js");
    const persistSpy = vi.spyOn(await import("../persist.js"), "persistConfig").mockReturnValue("");
    const syncSpy = vi.spyOn(CrontabManager, "syncWithRetry").mockResolvedValue(true);

    const r = await handleConfig("update", "cronSchedules", 'add "0 * * * *"');
    expect(r.isError).toBe(false);
    expect(r.text).toContain("0 * * * *");

    persistSpy.mockRestore();
    syncSpy.mockRestore();
  });

  it("cronSchedules sync failure shows warning", async () => {
    const persistSpy = vi.spyOn(await import("../persist.js"), "persistConfig").mockReturnValue("");
    const syncSpy = vi.spyOn(CrontabManager, "syncWithRetry").mockResolvedValue(false);

    const r = await handleConfig("update", "cronSchedules", 'add "0 * * * *"');
    expect(r.isError).toBe(false);
    expect(r.text).toContain("WARNING");
    expect(r.text).toContain("Failed to sync");

    persistSpy.mockRestore();
    syncSpy.mockRestore();
  });

  it("cronSchedules persist failure shows warning", async () => {
    const persistSpy = vi.spyOn(await import("../persist.js"), "persistConfig").mockReturnValue("disk full");
    const syncSpy = vi.spyOn(CrontabManager, "syncWithRetry").mockResolvedValue(true);

    const r = await handleConfig("update", "cronSchedules", 'add "0 * * * *"');
    expect(r.isError).toBe(false);
    expect(r.text).toContain("WARNING");
    expect(r.text).toContain("disk full");

    persistSpy.mockRestore();
    syncSpy.mockRestore();
  });
});

// ---------------------------------------------------------------------------
// handleRollback — numAncestors validation
// ---------------------------------------------------------------------------

describe("handleRollback — numAncestors", () => {
  let origManager: typeof pluginState.manager;
  let origReady: typeof pluginState.environmentReady;
  let origConfig: typeof pluginState.resolvedConfig;
  let mockManager: any;

  beforeEach(() => {
    vi.clearAllMocks();
    origManager = pluginState.manager;
    origReady = pluginState.environmentReady;
    origConfig = pluginState.resolvedConfig;

    mockManager = {
      rollback: vi.fn(),
    };
    pluginState.manager = mockManager as unknown as BtrfsManager;
    pluginState.environmentReady = true;
    pluginState.resolvedConfig = { workspace: "/ws", autoCheckpoint: false };
  });

  afterEach(() => {
    pluginState.manager = origManager;
    pluginState.environmentReady = origReady;
    pluginState.resolvedConfig = origConfig;
  });

  it("both target and numAncestors returns error", async () => {
    const r = await handleRollback("snap1", undefined, 2);
    expect(r.isError).toBe(true);
    expect(r.text).toContain("mutually exclusive");
  });

  it("numAncestors < 1 returns error", async () => {
    const r = await handleRollback(undefined, undefined, 0);
    expect(r.isError).toBe(true);
    expect(r.text).toContain(">= 1");
  });

  it("numAncestors NaN returns error", async () => {
    const r = await handleRollback(undefined, undefined, NaN);
    expect(r.isError).toBe(true);
    expect(r.text).toContain(">= 1");
  });

  it("numAncestors Infinity returns error", async () => {
    const r = await handleRollback(undefined, undefined, Infinity);
    expect(r.isError).toBe(true);
    expect(r.text).toContain(">= 1");
  });

  it("no workspace returns error", async () => {
    pluginState.resolvedConfig = { workspace: "", autoCheckpoint: false };
    const r = await handleRollback(undefined, undefined, 2);
    expect(r.isError).toBe(true);
    expect(r.text).toContain("No workspace");
  });

  it("numAncestors via manager success", async () => {
    mockManager.rollback.mockResolvedValue({
      success: true,
      message: "Rolled back 2 steps",
    });
    const r = await handleRollback(undefined, undefined, 2);
    expect(r.isError).toBe(false);
    expect(mockManager.rollback).toHaveBeenCalledWith(undefined, 2, false);
  });
});
