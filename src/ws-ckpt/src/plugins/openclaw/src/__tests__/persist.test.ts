import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("node:fs");
vi.mock("node:os", () => ({ default: { homedir: () => "/home/test" } }));

import fs from "node:fs";
import { loadPersistedConfig, persistConfig } from "../persist.js";

const mockExistsSync = fs.existsSync as ReturnType<typeof vi.fn>;
const mockReadFileSync = fs.readFileSync as ReturnType<typeof vi.fn>;
const mockWriteFileSync = fs.writeFileSync as ReturnType<typeof vi.fn>;
const mockMkdirSync = fs.mkdirSync as ReturnType<typeof vi.fn>;
const mockRenameSync = fs.renameSync as ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.clearAllMocks();
  delete process.env.OPENCLAW_STATE_DIR;
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
});
