import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

// Issue #10469: shellQuote escapes every single quote as '\'' when writing
// crontab entries, but extractWorkspace stopped at the first ' and never
// unescaped, so /tmp/alice's-project was truncated to /tmp/alice.  These
// tests pin the writer -> crontab line -> extractor round trip through the
// public CrontabManager surface, with runCrontab mocked like cron.test.ts.

vi.mock("fs", () => ({
  mkdirSync: vi.fn(),
  rmdirSync: vi.fn(),
  accessSync: vi.fn(),
  statSync: vi.fn(),
  constants: { X_OK: 1 },
}));

vi.mock("../commands.js", () => ({
  runCrontab: vi.fn(),
}));

import { accessSync, statSync, mkdirSync, rmdirSync } from "fs";
import { runCrontab } from "../commands.js";
import { CrontabManager } from "../cron.js";

const mockRunCrontab = runCrontab as ReturnType<typeof vi.fn>;
const mockMkdirSync = vi.mocked(mkdirSync);
const mockAccessSync = vi.mocked(accessSync);
const mockStatSync = vi.mocked(statSync);

const SCHEDULE = "0 3 * * *";
const QUOTED_WS = "/tmp/alice's-project";
const PLAIN_WS = "/plain-ws";

// The exact crontab line bytes the writer produces for QUOTED_WS, written out
// literally so the matcher is checked against the reported format.
const QUOTED_LINE =
  `0 3 * * * '/usr/bin/ws-ckpt' checkpoint -w '/tmp/alice'\\''s-project'` +
  ` -s "cron-$(date +\\%s)" -m "scheduled snapshot"` +
  ` --metadata '{"auto":true,"type":"cron"}' >/dev/null 2>&1`;

beforeEach(() => {
  vi.clearAllMocks();
  vi.stubEnv("PATH", "/usr/bin:/bin");
  mockMkdirSync.mockImplementation(() => undefined);
  mockAccessSync.mockImplementation(() => undefined);
  mockStatSync.mockReturnValue({ isFile: () => true } as ReturnType<typeof statSync>);
});

afterEach(() => {
  vi.unstubAllEnvs();
});

function writtenLines(): string[] {
  const writeCall = mockRunCrontab.mock.calls.at(-1)!;
  expect(writeCall[0]).toEqual(["-"]);
  return ((writeCall[1] as { input: string }).input)
    .split("\n")
    .filter((l) => l !== "");
}

function readReturns(stdout: string): void {
  mockRunCrontab.mockResolvedValueOnce({ exitCode: 0, stdout, stderr: "" });
}

function writeSucceeds(): void {
  mockRunCrontab.mockResolvedValueOnce({ exitCode: 0, stdout: "", stderr: "" });
}

describe("quoted workspace crontab ownership (issue #10469)", () => {
  it("writes an escaped line and recognizes it on resync (round trip)", async () => {
    readReturns("");
    writeSucceeds();
    expect(await CrontabManager.sync(QUOTED_WS, [SCHEDULE])).toBe(true);

    const lines = writtenLines();
    expect(lines).toHaveLength(1);
    expect(lines[0]).toBe(QUOTED_LINE);

    // Feeding the written line back: resync must be idempotent, not append.
    mockRunCrontab.mockClear();
    readReturns(lines.join("\n"));
    writeSucceeds();
    expect(await CrontabManager.sync(QUOTED_WS, [SCHEDULE])).toBe(true);
    expect(writtenLines()).toEqual(lines);
  });

  it("syncing the prefix workspace does not clobber the escaped entry", async () => {
    readReturns(QUOTED_LINE);
    writeSucceeds();
    expect(await CrontabManager.sync("/tmp/alice", [SCHEDULE])).toBe(true);

    const lines = writtenLines();
    expect(lines).toContain(QUOTED_LINE);
    expect(lines.filter((l) => l.includes("-w"))).toHaveLength(2);
  });

  it("removes the escaped entry for its exact workspace only", async () => {
    readReturns([QUOTED_LINE, `0 * * * * ws-ckpt checkpoint -w '${PLAIN_WS}'`].join("\n"));
    writeSucceeds();
    expect(await CrontabManager.remove(QUOTED_WS)).toBe(true);

    const lines = writtenLines();
    expect(lines).toEqual([`0 * * * * ws-ckpt checkpoint -w '${PLAIN_WS}'`]);
  });

  it("listInstalled returns schedules for the quoted workspace", async () => {
    readReturns([QUOTED_LINE, `0 * * * * ws-ckpt checkpoint -w '${PLAIN_WS}'`].join("\n"));
    expect(await CrontabManager.listInstalled(QUOTED_WS)).toEqual([SCHEDULE]);
  });

  it("listInstalled ignores the prefix workspace", async () => {
    readReturns(QUOTED_LINE);
    expect(await CrontabManager.listInstalled("/tmp/alice")).toEqual([]);
  });

  it("migrates a quoted workspace without leaving orphaned entries", async () => {
    const newWs = "/tmp/bob's-projects";
    readReturns(QUOTED_LINE);
    writeSucceeds(); // remove old
    readReturns("");
    writeSucceeds(); // install new
    const warnings = await CrontabManager.migrate(QUOTED_WS, newWs, [SCHEDULE]);
    expect(warnings).toEqual([]);
    const lines = writtenLines();
    expect(lines).toHaveLength(1);
    expect(lines[0]).toContain("-w '/tmp/bob'\\''s-projects'");
    expect(lines[0]).not.toContain("/tmp/alice");
  });
});
