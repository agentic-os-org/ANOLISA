import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("node:fs");
vi.mock("node:os", () => ({ default: { homedir: () => "/home/test" } }));

import fs from "node:fs";
import { loadPersistedConfig, persistConfig } from "../persist.js";

const mockExistsSync = fs.existsSync as ReturnType<typeof vi.fn>;
const mockReadFileSync = fs.readFileSync as ReturnType<typeof vi.fn>;
const mockWriteFileSync = fs.writeFileSync as ReturnType<typeof vi.fn>;
const mockMkdirSync = fs.mkdirSync as ReturnType<typeof vi.fn>;
const mockRenameSync = fs.renameSync as ReturnType<typeof vi.fn>;
const mockUnlinkSync = fs.unlinkSync as ReturnType<typeof vi.fn>;
const mockRmSync = fs.rmSync as ReturnType<typeof vi.fn>;

beforeEach(() => {
  // resetAllMocks (not clearAllMocks): implementations installed by an
  // earlier test must not leak into the next one.
  vi.resetAllMocks();
  delete process.env.OPENCLAW_STATE_DIR;
  // Deletion guards: cleanup may only ever remove the temp file the write
  // just created. A delete of anything else — e.g. the final config in a
  // future "clean up the corrupt config" edit — must fail the test instead
  // of passing silently.
  mockUnlinkSync.mockImplementation((p: unknown) => {
    throw new Error(`unexpected unlinkSync: ${p}`);
  });
  mockRmSync.mockImplementation((p: unknown) => {
    if (typeof p === "string" && p.includes("ws-ckpt.json.tmp.")) return undefined;
    throw new Error(`unexpected rmSync: ${p}`);
  });
});

describe("loadPersistedConfig", () => {
  it("returns empty when file does not exist", () => {
    mockExistsSync.mockReturnValue(false);
    expect(loadPersistedConfig()).toEqual({});
  });

  it("returns parsed fields from valid JSON", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue(
      JSON.stringify({ autoCheckpoint: true, workspace: "/ws", cronSchedules: ["0 * * * *"] })
    );
    const cfg = loadPersistedConfig();
    expect(cfg.autoCheckpoint).toBe(true);
    expect(cfg.workspace).toBe("/ws");
    expect(cfg.cronSchedules).toEqual(["0 * * * *"]);
  });

  it("returns only autoCheckpoint when only that is set", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue(JSON.stringify({ autoCheckpoint: false }));
    const cfg = loadPersistedConfig();
    expect(cfg.autoCheckpoint).toBe(false);
    expect(cfg.workspace).toBeUndefined();
  });

  it("returns empty for non-object JSON", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue('"string"');
    expect(loadPersistedConfig()).toEqual({});
  });

  it("returns empty for array JSON", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue("[1,2,3]");
    expect(loadPersistedConfig()).toEqual({});
  });

  it("returns empty for invalid JSON", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue("{bad}");
    expect(loadPersistedConfig()).toEqual({});
  });

  it("filters non-string cronSchedules entries", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue(
      JSON.stringify({ cronSchedules: ["0 * * * *", 123, null] })
    );
    const cfg = loadPersistedConfig();
    expect(cfg.cronSchedules).toEqual(["0 * * * *"]);
  });

  it("returns empty on read error", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockImplementation(() => { throw new Error("EACCES"); });
    expect(loadPersistedConfig()).toEqual({});
  });

  it("respects OPENCLAW_STATE_DIR env var", () => {
    process.env.OPENCLAW_STATE_DIR = "/custom/dir";
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue(JSON.stringify({ workspace: "/ws" }));
    loadPersistedConfig();
    expect(mockReadFileSync).toHaveBeenCalledWith(
      expect.stringContaining("/custom/dir/ws-ckpt.json"),
      "utf-8"
    );
  });
});

describe("persistConfig", () => {
  it("creates dir and writes file on happy path", () => {
    mockExistsSync.mockReturnValue(false);
    mockMkdirSync.mockReturnValue(undefined);
    mockWriteFileSync.mockReturnValue(undefined);
    mockRenameSync.mockReturnValue(undefined);

    const err = persistConfig({ autoCheckpoint: true });
    expect(err).toBe("");
    expect(mockMkdirSync).toHaveBeenCalled();
    expect(mockWriteFileSync).toHaveBeenCalled();
    expect(mockRenameSync).toHaveBeenCalled();
  });

  it("merges with existing config", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue(JSON.stringify({ workspace: "/old" }));
    mockMkdirSync.mockReturnValue(undefined);
    mockWriteFileSync.mockReturnValue(undefined);
    mockRenameSync.mockReturnValue(undefined);

    const err = persistConfig({ autoCheckpoint: true });
    expect(err).toBe("");
    const written = JSON.parse(
      (mockWriteFileSync.mock.calls[0][1] as string).trim()
    );
    expect(written.workspace).toBe("/old");
    expect(written.autoCheckpoint).toBe(true);
  });

  it("starts fresh when existing file has invalid JSON", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue("{bad}");
    mockMkdirSync.mockReturnValue(undefined);
    mockWriteFileSync.mockReturnValue(undefined);
    mockRenameSync.mockReturnValue(undefined);

    const err = persistConfig({ workspace: "/new" });
    expect(err).toBe("");
  });

  it("starts fresh when existing file is array", () => {
    mockExistsSync.mockReturnValue(true);
    mockReadFileSync.mockReturnValue("[1]");
    mockMkdirSync.mockReturnValue(undefined);
    mockWriteFileSync.mockReturnValue(undefined);
    mockRenameSync.mockReturnValue(undefined);

    const err = persistConfig({ workspace: "/new" });
    expect(err).toBe("");
    const written = JSON.parse(
      (mockWriteFileSync.mock.calls[0][1] as string).trim()
    );
    expect(written.workspace).toBe("/new");
  });

  it("returns error message on write failure", () => {
    mockExistsSync.mockReturnValue(false);
    mockMkdirSync.mockReturnValue(undefined);
    mockWriteFileSync.mockImplementation(() => { throw new Error("disk full"); });

    const err = persistConfig({ autoCheckpoint: true });
    expect(err).toContain("disk full");
  });

  it("respects OPENCLAW_STATE_DIR env var", () => {
    process.env.OPENCLAW_STATE_DIR = "/custom";
    mockExistsSync.mockReturnValue(false);
    mockMkdirSync.mockReturnValue(undefined);
    mockWriteFileSync.mockReturnValue(undefined);
    mockRenameSync.mockReturnValue(undefined);

    persistConfig({ workspace: "/ws" });
    expect(mockRenameSync).toHaveBeenCalledWith(
      expect.stringContaining("/custom/"),
      expect.stringContaining("/custom/ws-ckpt.json")
    );
  });

  it("removes only the temp file when the rename fails", () => {
    mockExistsSync.mockReturnValue(false);
    mockMkdirSync.mockReturnValue(undefined);
    mockWriteFileSync.mockReturnValue(undefined);
    mockRenameSync.mockImplementation(() => { throw new Error("EISDIR"); });

    const err = persistConfig({ autoCheckpoint: true });
    expect(err).toContain("EISDIR");

    expect(mockRmSync).toHaveBeenCalledTimes(1);
    const rmPath = mockRmSync.mock.calls[0][0] as string;
    expect(rmPath).toBe(mockWriteFileSync.mock.calls[0][0] as string);
    expect(rmPath).toContain("ws-ckpt.json.tmp.");
    expect(mockRmSync.mock.calls[0][1]).toEqual({ force: true });
    expect(mockUnlinkSync).not.toHaveBeenCalled();
  });

  it("removes a partial temp file when the write fails", () => {
    mockExistsSync.mockReturnValue(false);
    mockMkdirSync.mockReturnValue(undefined);
    mockWriteFileSync.mockImplementation(() => { throw new Error("ENOSPC"); });

    const err = persistConfig({ autoCheckpoint: true });
    expect(err).toContain("ENOSPC");

    expect(mockRmSync).toHaveBeenCalledTimes(1);
    const rmPath = mockRmSync.mock.calls[0][0] as string;
    expect(rmPath).toContain("ws-ckpt.json.tmp.");
    expect(rmPath).toContain(`.tmp.${process.pid}`);
    expect(mockRmSync.mock.calls[0][1]).toEqual({ force: true });
    expect(mockUnlinkSync).not.toHaveBeenCalled();
  });

  it("cleanup after a successful rename is a force no-op on the temp path", () => {
    mockExistsSync.mockReturnValue(false);
    mockMkdirSync.mockReturnValue(undefined);
    mockWriteFileSync.mockReturnValue(undefined);
    mockRenameSync.mockReturnValue(undefined);

    expect(persistConfig({ autoCheckpoint: true })).toBe("");

    expect(mockRmSync).toHaveBeenCalledTimes(1);
    const rmPath = mockRmSync.mock.calls[0][0] as string;
    expect(rmPath).toBe(mockWriteFileSync.mock.calls[0][0] as string);
    expect(mockRmSync.mock.calls[0][1]).toEqual({ force: true });
    expect(mockUnlinkSync).not.toHaveBeenCalled();
  });
});
