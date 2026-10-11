import { describe, it, expect, vi, afterEach } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { cwdInsideWorkspace, cwdInsideWorkspaceReason, UNAVAILABLE_MSG, pluginState } from "../state.js";

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

  // After init the registered workspace IS a symlink into backend storage,
  // and the kernel reports cwd with symlinks resolved (getcwd semantics).
  // These tests mirror that layout with a real symlink on disk.
  describe("symlinked workspace", () => {
    it("detects cwd reached through the workspace symlink", () => {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ws-ckpt-cwd-"));
      try {
        const subvol = path.join(dir, "subvol");
        fs.mkdirSync(path.join(subvol, "sub"), { recursive: true });
        const link = path.join(dir, "link");
        fs.symlinkSync(subvol, link);
        // Physical path, as process.cwd() would report it.
        vi.spyOn(process, "cwd").mockReturnValue(path.join(subvol, "sub"));

        const r = cwdInsideWorkspace(link);
        expect(r.inside).toBe(true);
      } finally {
        fs.rmSync(dir, { recursive: true, force: true });
      }
    });

    it("detects the workspace root reached through the symlink", () => {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ws-ckpt-cwd-"));
      try {
        const subvol = path.join(dir, "subvol");
        fs.mkdirSync(subvol);
        const link = path.join(dir, "link");
        fs.symlinkSync(subvol, link);
        vi.spyOn(process, "cwd").mockReturnValue(subvol);

        const r = cwdInsideWorkspace(link);
        expect(r.inside).toBe(true);
      } finally {
        fs.rmSync(dir, { recursive: true, force: true });
      }
    });

    it("does not match a sibling of the symlink target", () => {
      const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ws-ckpt-cwd-"));
      try {
        const subvol = path.join(dir, "subvol");
        const sibling = path.join(dir, "sibling");
        fs.mkdirSync(subvol);
        fs.mkdirSync(sibling);
        const link = path.join(dir, "link");
        fs.symlinkSync(subvol, link);
        vi.spyOn(process, "cwd").mockReturnValue(sibling);

        const r = cwdInsideWorkspace(link);
        expect(r.inside).toBe(false);
      } finally {
        fs.rmSync(dir, { recursive: true, force: true });
      }
    });

    it("falls back to the lexical path when it does not resolve", () => {
      // Pre-init workspaces may not exist yet; behavior must be unchanged.
      vi.spyOn(process, "cwd").mockReturnValue("/ws/subdir");
      const r = cwdInsideWorkspace("/ws");
      expect(r.inside).toBe(true);
    });
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
