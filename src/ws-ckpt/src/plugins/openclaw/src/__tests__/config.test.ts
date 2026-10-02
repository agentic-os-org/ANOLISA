import { describe, it, expect } from "vitest";
import {
  parseWorkspaceCleanupJson,
  PluginConfigManager,
  DEFAULT_CONFIG,
} from "../config.js";

// ---------------------------------------------------------------------------
// parseWorkspaceCleanupJson
// ---------------------------------------------------------------------------

describe("parseWorkspaceCleanupJson", () => {
  const v1 = (effective: unknown) =>
    JSON.stringify({ schema: "ws-ckpt-policy/v1", effective });

  it("returns parse-error for invalid JSON", () => {
    const r = parseWorkspaceCleanupJson("not json");
    expect(r.kind).toBe("parse-error");
  });

  it("returns parse-error for non-object root", () => {
    const r = parseWorkspaceCleanupJson('"hello"');
    expect(r.kind).toBe("parse-error");
  });

  it("returns parse-error for null root", () => {
    const r = parseWorkspaceCleanupJson("null");
    expect(r.kind).toBe("parse-error");
  });

  it("returns parse-error for wrong schema", () => {
    const r = parseWorkspaceCleanupJson(JSON.stringify({ schema: "v99" }));
    expect(r.kind).toBe("parse-error");
    if (r.kind === "parse-error") expect(r.reason).toContain("v99");
  });

  it("returns parse-error for missing effective", () => {
    const r = parseWorkspaceCleanupJson(
      JSON.stringify({ schema: "ws-ckpt-policy/v1" }),
    );
    expect(r.kind).toBe("parse-error");
  });

  it("returns parse-error for non-object effective", () => {
    const r = parseWorkspaceCleanupJson(v1("string"));
    expect(r.kind).toBe("parse-error");
  });

  it("returns disabled when is_disabled is true", () => {
    const r = parseWorkspaceCleanupJson(v1({ is_disabled: true }));
    expect(r).toEqual({ kind: "disabled" });
  });

  it("returns count mode", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: { mode: "count", count: 5 } }),
    );
    expect(r).toEqual({ kind: "count", num: 5 });
  });

  it("returns count zero", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: { mode: "count", count: 0 } }),
    );
    expect(r).toEqual({ kind: "count", num: 0 });
  });

  it("rejects NaN count", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: { mode: "count", count: NaN } }),
    );
    expect(r.kind).toBe("parse-error");
  });

  it("rejects float count", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: { mode: "count", count: 3.5 } }),
    );
    expect(r.kind).toBe("parse-error");
  });

  it("rejects negative count", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: { mode: "count", count: -1 } }),
    );
    expect(r.kind).toBe("parse-error");
  });

  it("rejects string count", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: { mode: "count", count: "5" } }),
    );
    expect(r.kind).toBe("parse-error");
  });

  it("returns age mode", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: { mode: "age", raw: "7d" } }),
    );
    expect(r).toEqual({ kind: "age", duration: "7d" });
  });

  it("rejects non-string raw in age mode", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: { mode: "age", raw: 42 } }),
    );
    expect(r.kind).toBe("parse-error");
  });

  it("returns parse-error for unknown mode", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: { mode: "fancy" } }),
    );
    expect(r.kind).toBe("parse-error");
  });

  it("returns parse-error for missing auto_cleanup_keep", () => {
    const r = parseWorkspaceCleanupJson(v1({ is_disabled: false }));
    expect(r.kind).toBe("parse-error");
  });

  it("returns parse-error for non-object auto_cleanup_keep", () => {
    const r = parseWorkspaceCleanupJson(
      v1({ auto_cleanup_keep: "string" }),
    );
    expect(r.kind).toBe("parse-error");
  });
});

// ---------------------------------------------------------------------------
// PluginConfigManager
// ---------------------------------------------------------------------------

describe("PluginConfigManager", () => {
  it("uses DEFAULT_CONFIG when no user config", () => {
    const mgr = new PluginConfigManager();
    const cfg = mgr.getConfig();
    expect(cfg.autoCheckpoint).toBe(DEFAULT_CONFIG.autoCheckpoint);
  });

  it("merges user config over defaults", () => {
    const mgr = new PluginConfigManager({ autoCheckpoint: true });
    expect(mgr.getConfig().autoCheckpoint).toBe(true);
  });

  it("getConfig returns a copy", () => {
    const mgr = new PluginConfigManager();
    const a = mgr.getConfig();
    const b = mgr.getConfig();
    expect(a).not.toBe(b);
    expect(a).toEqual(b);
  });

  it("validate returns valid with no errors", () => {
    const mgr = new PluginConfigManager();
    const v = mgr.validate();
    expect(v.valid).toBe(true);
    expect(v.errors).toHaveLength(0);
  });

  it("filters invalid cron expressions from config", () => {
    const mgr = new PluginConfigManager({
      cronSchedules: ["0 * * * *", "bad", "5 4 * * *"],
    });
    const cfg = mgr.getConfig();
    expect(cfg.cronSchedules).toEqual(["0 * * * *", "5 4 * * *"]);
  });

  it("keeps all valid cron expressions", () => {
    const mgr = new PluginConfigManager({
      cronSchedules: ["0 * * * *", "5 4 * * *"],
    });
    expect(mgr.getConfig().cronSchedules).toEqual(["0 * * * *", "5 4 * * *"]);
  });
});

// ---------------------------------------------------------------------------
// DEFAULT_CONFIG
// ---------------------------------------------------------------------------

describe("DEFAULT_CONFIG", () => {
  it("has expected shape", () => {
    expect(DEFAULT_CONFIG.autoCheckpoint).toBe(false);
    expect(typeof DEFAULT_CONFIG.workspace).toBe("string");
  });
});
