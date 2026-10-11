import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { persistConfig } from "../persist.js";

let stateDir: string;
let previousStateDir: string | undefined;
const original = '{"workspace":"/private/old","unknown":"kept"}\n';

beforeEach(() => {
  previousStateDir = process.env.OPENCLAW_STATE_DIR;
  stateDir = fs.mkdtempSync(path.join(os.tmpdir(), "ws-ckpt-persist-private-"));
  process.env.OPENCLAW_STATE_DIR = stateDir;
  fs.writeFileSync(path.join(stateDir, "ws-ckpt.json"), original);
  fs.writeFileSync(path.join(stateDir, "unrelated.tmp"), "unrelated");
});

afterEach(() => {
  vi.restoreAllMocks();
  if (previousStateDir === undefined) delete process.env.OPENCLAW_STATE_DIR;
  else process.env.OPENCLAW_STATE_DIR = previousStateDir;
  expect(path.dirname(stateDir)).toBe(os.tmpdir());
  expect(path.basename(stateDir)).toMatch(/^ws-ckpt-persist-private-/);
  fs.rmSync(stateDir, { recursive: true, force: true });
});

function expectIntact() {
  expect(fs.readFileSync(path.join(stateDir, "ws-ckpt.json"), "utf8")).toBe(
    original,
  );
  expect(fs.readFileSync(path.join(stateDir, "unrelated.tmp"), "utf8")).toBe(
    "unrelated",
  );
  expect(fs.readdirSync(stateDir).sort()).toEqual([
    "unrelated.tmp",
    "ws-ckpt.json",
  ]);
}

describe("persistConfig staging lifecycle", () => {
  it("removes a complete staged file when atomic rename fails", () => {
    vi.spyOn(fs, "renameSync").mockImplementation(() => {
      throw new Error("controlled rename failure");
    });
    expect(persistConfig({ workspace: "/private/new" })).toBe(
      "controlled rename failure",
    );
    expectIntact();
  });

  it("removes a partially written staging file without changing the prior config", () => {
    const write = fs.writeFileSync.bind(fs);
    vi.spyOn(fs, "writeFileSync").mockImplementationOnce(
      (file, data, options) => {
        write(file, String(data).slice(0, 8), options);
        throw new Error("controlled partial write");
      },
    );
    expect(persistConfig({ workspace: "/private/new" })).toBe(
      "controlled partial write",
    );
    expectIntact();
  });

  it("keeps the original failure when staging cleanup also fails", () => {
    vi.spyOn(fs, "renameSync").mockImplementation(() => {
      throw new Error("primary rename failure");
    });
    const cleanup = vi.spyOn(fs, "unlinkSync").mockImplementation(() => {
      throw new Error("secondary cleanup failure");
    });
    expect(persistConfig({ autoCheckpoint: true })).toBe(
      "primary rename failure",
    );
    expect(cleanup).toHaveBeenCalledWith(
      path.join(stateDir, `ws-ckpt.json.tmp.${process.pid}`),
    );
    expect(fs.readFileSync(path.join(stateDir, "ws-ckpt.json"), "utf8")).toBe(
      original,
    );
    expect(fs.readFileSync(path.join(stateDir, "unrelated.tmp"), "utf8")).toBe(
      "unrelated",
    );
  });

  it("does not remove unrelated state when failure occurs before staging", () => {
    const cleanup = vi.spyOn(fs, "unlinkSync");
    vi.spyOn(fs, "mkdirSync").mockImplementation(() => {
      throw new Error("controlled directory failure");
    });
    expect(persistConfig({ autoCheckpoint: true })).toBe(
      "controlled directory failure",
    );
    expect(cleanup).not.toHaveBeenCalled();
    expectIntact();
  });

  it("atomically replaces valid merged config and leaves no staging file on success", () => {
    expect(persistConfig({ autoCheckpoint: true })).toBe("");
    const saved = JSON.parse(
      fs.readFileSync(path.join(stateDir, "ws-ckpt.json"), "utf8"),
    );
    expect(saved).toEqual({
      workspace: "/private/old",
      unknown: "kept",
      autoCheckpoint: true,
    });
    expect(fs.statSync(path.join(stateDir, "ws-ckpt.json")).mode & 0o777).toBe(
      0o600,
    );
    expect(fs.readdirSync(stateDir).sort()).toEqual([
      "unrelated.tmp",
      "ws-ckpt.json",
    ]);
  });
});
