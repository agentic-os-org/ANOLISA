import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

// Reset module-level `alreadyEnsured` guard between tests.
// We import the module fresh each time via dynamic import + vi.resetModules.

describe("WS_CKPT_TOOL_NAMES", () => {
  it("contains 7 tool names", async () => {
    const { WS_CKPT_TOOL_NAMES } = await import("../whitelist.js");
    expect(WS_CKPT_TOOL_NAMES).toHaveLength(7);
  });

  it("all start with ws-ckpt-", async () => {
    const { WS_CKPT_TOOL_NAMES } = await import("../whitelist.js");
    for (const name of WS_CKPT_TOOL_NAMES) {
      expect(name).toMatch(/^ws-ckpt-/);
    }
  });

  it("includes known tools", async () => {
    const { WS_CKPT_TOOL_NAMES } = await import("../whitelist.js");
    expect(WS_CKPT_TOOL_NAMES).toContain("ws-ckpt-checkpoint");
    expect(WS_CKPT_TOOL_NAMES).toContain("ws-ckpt-rollback");
    expect(WS_CKPT_TOOL_NAMES).toContain("ws-ckpt-list");
    expect(WS_CKPT_TOOL_NAMES).toContain("ws-ckpt-config");
    expect(WS_CKPT_TOOL_NAMES).toContain("ws-ckpt-status");
  });

  it("matches the OURS list embedded in install-openclaw.sh", async () => {
    const { WS_CKPT_TOOL_NAMES } = await import("../whitelist.js");

    // install-openclaw.sh pre-writes the allowlist via `openclaw config set`
    // and embeds its own copy of the tool names (shell cannot import TS).
    // Drift here means a new tool silently never gets allowlisted.
    const scriptPath = path.resolve(
      path.dirname(fileURLToPath(import.meta.url)),
      "../../../../../scripts/openclaw/install-openclaw.sh",
    );
    const script = fs.readFileSync(scriptPath, "utf-8");
    const match = script.match(/var OURS = \[([^\]]*)\]/);
    expect(match, "OURS array not found in install-openclaw.sh").not.toBeNull();
    const scriptTools = [...match![1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);

    expect([...scriptTools].sort()).toEqual([...WS_CKPT_TOOL_NAMES].sort());
  });
});

describe("ensureToolsAllowlist", () => {
  let origEnv: Record<string, string | undefined>;

  beforeEach(() => {
    vi.resetModules();
    origEnv = { ...process.env };
  });

  afterEach(() => {
    process.env = origEnv;
    vi.restoreAllMocks();
  });

  it("warns about missing tools without modifying openclaw.json", async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ws-ckpt-test-"));
    const configPath = path.join(tmpDir, "openclaw.json");
    const original = JSON.stringify({ tools: { alsoAllow: [] } });
    fs.writeFileSync(configPath, original);

    process.env.OPENCLAW_CONFIG_PATH = configPath;

    const { ensureToolsAllowlist, WS_CKPT_TOOL_NAMES } = await import(
      "../whitelist.js"
    );

    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    const api = {
      config: { tools: { alsoAllow: [] } },
    } as any;

    ensureToolsAllowlist(api);

    expect(warnSpy).toHaveBeenCalledTimes(1);
    const message = warnSpy.mock.calls[0][0] as string;
    for (const name of WS_CKPT_TOOL_NAMES) {
      expect(message).toContain(name);
    }
    // The plugin never writes openclaw.json — writes belong to the installer.
    expect(fs.readFileSync(configPath, "utf-8")).toBe(original);

    fs.rmSync(tmpDir, { recursive: true });
  });

  it("stays silent when all tools already present", async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ws-ckpt-test-"));
    const configPath = path.join(tmpDir, "openclaw.json");

    const { WS_CKPT_TOOL_NAMES } = await import("../whitelist.js");
    fs.writeFileSync(
      configPath,
      JSON.stringify({ tools: { alsoAllow: [...WS_CKPT_TOOL_NAMES] } }),
    );

    process.env.OPENCLAW_CONFIG_PATH = configPath;

    vi.resetModules();
    const mod = await import("../whitelist.js");
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    mod.ensureToolsAllowlist({ config: {} } as any);
    expect(warnSpy).not.toHaveBeenCalled();

    fs.rmSync(tmpDir, { recursive: true });
  });

  it("accepts tools.allow as the active allowlist", async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ws-ckpt-test-"));
    const configPath = path.join(tmpDir, "openclaw.json");
    const { WS_CKPT_TOOL_NAMES } = await import("../whitelist.js");
    fs.writeFileSync(
      configPath,
      JSON.stringify({ tools: { allow: [...WS_CKPT_TOOL_NAMES] } }),
    );
    process.env.OPENCLAW_CONFIG_PATH = configPath;

    vi.resetModules();
    const mod = await import("../whitelist.js");
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    mod.ensureToolsAllowlist({ config: {} } as any);
    expect(warnSpy).not.toHaveBeenCalled();

    fs.rmSync(tmpDir, { recursive: true });
  });

  it("uses effective config when the root file delegates through $include", async () => {
    const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "ws-ckpt-test-"));
    const configPath = path.join(tmpDir, "openclaw.json");
    fs.writeFileSync(configPath, JSON.stringify({ $include: "tools.json" }));
    process.env.OPENCLAW_CONFIG_PATH = configPath;

    const mod = await import("../whitelist.js");
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    mod.ensureToolsAllowlist({
      config: { tools: { alsoAllow: [...mod.WS_CKPT_TOOL_NAMES] } },
    } as any);
    expect(warnSpy).not.toHaveBeenCalled();

    fs.rmSync(tmpDir, { recursive: true });
  });

  it("handles missing config file gracefully — uses api.config fallback", async () => {
    process.env.OPENCLAW_CONFIG_PATH = "/nonexistent/openclaw.json";

    const mod = await import("../whitelist.js");

    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    // api has empty alsoAllow, so all tools are "missing" — warns but must
    // not throw.
    const api = {
      config: { tools: { alsoAllow: [] } },
    } as any;

    expect(() => mod.ensureToolsAllowlist(api)).not.toThrow();
    expect(warnSpy).toHaveBeenCalledTimes(1);
  });
});
