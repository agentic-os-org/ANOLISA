import { describe, it, expect, vi, afterEach } from "vitest";
import { mkdirSync, mkdtempSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { cwdInsideWorkspace, cwdInsideWorkspaceReason, UNAVAILABLE_MSG, pluginState } from "../state.js";
import { handleCheckpoint } from "../handlers.js";

describe("cwdInsideWorkspace", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("detects cwd inside workspace", () => {
    vi.spyOn(process, "cwd").mockReturnValue("/ws/subdir");
    const r = cwdInsideWorkspace("/ws");
    expect(r.inside).toBe(true);
  });

  it("detects exact match", () => {
    vi.spyOn(process, "cwd").mockReturnValue("/ws");
    const r = cwdInsideWorkspace("/ws");
    expect(r.inside).toBe(true);
  });

  it("detects cwd outside workspace", () => {
    vi.spyOn(process, "cwd").mockReturnValue("/other");
    const r = cwdInsideWorkspace("/ws");
    expect(r.inside).toBe(false);
  });

  it("returns false when cwd throws", () => {
    vi.spyOn(process, "cwd").mockImplementation(() => {
      throw new Error("ENOENT");
    });
    const r = cwdInsideWorkspace("/ws");
    expect(r.inside).toBe(false);
    expect(r.cwd).toBe("");
  });

  it("does not match /workspace when workspace is /ws", () => {
    vi.spyOn(process, "cwd").mockReturnValue("/workspace");
    const r = cwdInsideWorkspace("/ws");
    expect(r.inside).toBe(false);
  });

  it("recognizes real directories behind a workspace symlink without matching siblings", () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ckpt-cwd-alias-"));
    const managed = path.join(sandbox, "managed");
    const workspace = path.join(sandbox, "workspace");
    mkdirSync(path.join(managed, "subdir"), { recursive: true });
    mkdirSync(`${managed}-sibling`);
    symlinkSync(managed, workspace, "dir");
    const cwd = vi.spyOn(process, "cwd");
    try {
      for (const directory of [managed, path.join(managed, "subdir")]) {
        cwd.mockReturnValue(directory);
        expect(cwdInsideWorkspace(workspace)).toEqual({ inside: true, cwd: directory });
      }
      cwd.mockReturnValue(`${managed}-sibling`);
      expect(cwdInsideWorkspace(workspace).inside).toBe(false);
    } finally {
      rmSync(sandbox, { recursive: true, force: true });
    }
  });

  it("refuses a checkpoint inside a managed workspace without calling the manager", async () => {
    const sandbox = mkdtempSync(path.join(tmpdir(), "ckpt-handler-alias-"));
    const managed = path.join(sandbox, "managed");
    const workspace = path.join(sandbox, "workspace");
    mkdirSync(managed);
    symlinkSync(managed, workspace, "dir");
    const previousState = { ...pluginState };
    const createCheckpoint = vi.fn().mockResolvedValue({ success: true, message: "created" });
    pluginState.manager = { createCheckpoint } as unknown as NonNullable<typeof pluginState.manager>;
    pluginState.environmentReady = true;
    pluginState.resolvedConfig = { workspace } as NonNullable<typeof pluginState.resolvedConfig>;
    vi.spyOn(process, "cwd").mockReturnValue(managed);
    try {
      const result = await handleCheckpoint(JSON.stringify({ id: "test-snapshot" }));
      expect(result.isError).toBe(true);
      expect(result.text).toContain(`workspace=${workspace}`);
      expect(createCheckpoint).not.toHaveBeenCalled();
      expect(pluginState.resolvedConfig.workspace).toBe(workspace);
    } finally {
      Object.assign(pluginState, previousState);
      rmSync(sandbox, { recursive: true, force: true });
    }
  });
});

describe("cwdInsideWorkspaceReason", () => {
  it("includes cwd and workspace", () => {
    const msg = cwdInsideWorkspaceReason("/ws/sub", "/ws");
    expect(msg).toContain("cwd=/ws/sub");
    expect(msg).toContain("workspace=/ws");
  });

  it("mentions inode replacement", () => {
    const msg = cwdInsideWorkspaceReason("/ws", "/ws");
    expect(msg).toContain("inode");
  });
});

describe("UNAVAILABLE_MSG", () => {
  it("is a non-empty string", () => {
    expect(UNAVAILABLE_MSG.length).toBeGreaterThan(0);
  });
});

describe("pluginState", () => {
  it("has expected initial shape", () => {
    expect(pluginState).toHaveProperty("manager");
    expect(pluginState).toHaveProperty("environmentReady");
    expect(pluginState).toHaveProperty("pluginApi");
    expect(pluginState).toHaveProperty("resolvedConfig");
  });
});
