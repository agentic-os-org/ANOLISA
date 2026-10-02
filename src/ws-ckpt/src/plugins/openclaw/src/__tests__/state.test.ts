import { describe, it, expect, vi, afterEach } from "vitest";
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
