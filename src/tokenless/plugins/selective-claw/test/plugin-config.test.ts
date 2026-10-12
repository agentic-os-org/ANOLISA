import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DatabaseSync } from "node:sqlite";
import { homedir } from "node:os";
import { join } from "node:path";
import activate from "../src/plugin/index.js";
import * as connection from "../src/db/connection.js";
import type { SelectiveClawConfig } from "../src/types.js";
import type { SelectiveContextEngine } from "../src/engine.js";

describe("OpenClaw plugin configuration", () => {
  let db: DatabaseSync;

  beforeEach(() => {
    db = connection.createConnection(":memory:");
    vi.spyOn(connection, "createConnection").mockReturnValue(db);
  });

  afterEach(() => {
    connection.closeConnection(db);
    vi.restoreAllMocks();
    vi.unstubAllEnvs();
  });

  function host(pluginConfig?: Partial<SelectiveClawConfig>) {
    return {
      config: { plugins: { entries: { "selective-claw": { config: pluginConfig } } } },
      pluginConfig,
      runtime: { llm: { complete: vi.fn() } },
      registerContextEngine: vi.fn(),
      registerTool: vi.fn(),
    };
  }

  it("does not create an archive or register anything when disabled", () => {
    const api = host({ enabled: false });
    activate(api);
    expect(connection.createConnection).not.toHaveBeenCalled();
    expect(api.registerContextEngine).not.toHaveBeenCalled();
    expect(api.registerTool).not.toHaveBeenCalled();
  });

  it("uses the configured archive and fresh-tail size from pluginConfig", async () => {
    const api = host({ dbPath: "/custom/archive.sqlite", freshTailTurns: 1 });
    activate(api);
    expect(connection.createConnection).toHaveBeenCalledWith("/custom/archive.sqlite");
    const engine = api.registerContextEngine.mock.calls[0][1]() as SelectiveContextEngine;
    const messages = [
      { role: "user", content: "old question" },
      { role: "assistant", content: "old answer" },
      { role: "user", content: "recent question" },
      { role: "assistant", content: "recent answer" },
    ];
    const result = await engine.assemble({ sessionId: "configured-session", messages });
    expect(result.messages).toHaveLength(3);
    expect(result.messages[1]).toBe(messages[2]);
    expect(result.messages[2]).toBe(messages[3]);
  });

  it("expands the documented home-relative database path", () => {
    activate(host({ dbPath: "~/.openclaw/custom-archive.sqlite" }));
    expect(connection.createConnection).toHaveBeenCalledWith(join(homedir(), ".openclaw", "custom-archive.sqlite"));
  });

  it("keeps the default state directory when pluginConfig is absent", () => {
    vi.stubEnv("OPENCLAW_STATE_DIR", "/isolated-state");
    activate(host());
    expect(connection.createConnection).toHaveBeenCalledWith(join("/isolated-state", "selective-claw.db"));
  });
});
