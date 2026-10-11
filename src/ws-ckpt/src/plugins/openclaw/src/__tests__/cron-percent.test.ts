import { execFileSync } from "node:child_process";
import { beforeEach, expect, it, vi } from "vitest";

vi.mock("fs", () => ({ mkdirSync: vi.fn(), rmdirSync: vi.fn() }));
vi.mock("../commands.js", () => ({ runCrontab: vi.fn() }));

import { runCrontab } from "../commands.js";
import { CrontabManager } from "../cron.js";

const mockRunCrontab = vi.mocked(runCrontab);
let installed: string;

beforeEach(() => {
  vi.clearAllMocks();
  installed = "# unrelated entry\n";
  mockRunCrontab.mockImplementation(async (args, options) => {
    if (args[0] === "-l") return { exitCode: 0, stdout: installed, stderr: "" };
    installed = options?.input ?? "";
    return { exitCode: 0, stdout: "", stderr: "" };
  });
});

// Cron scans for an unescaped percent before invoking the shell; shell quotes
// are ordinary bytes during this scan. An escaped percent loses its backslash.
function cronCommand(command: string): { command: string; stdin: string | null } {
  let result = "", escaped = false;
  for (let i = 0; i < command.length; i++) {
    const char = command[i];
    if (escaped) {
      result += (char === "%" ? "" : "\\") + char;
      escaped = false;
    } else if (char === "\\") {
      escaped = true;
    } else if (char === "%") {
      return { command: result, stdin: command.slice(i + 1) };
    } else {
      result += char;
    }
  }
  return { command: result + (escaped ? "\\" : ""), stdin: null };
}

it.each(["/work/50% project", "/work/%leading", "/work/trailing%", "/work/100%%", String.raw`/work/backslash\%project`, String.raw`/work/percent%\tail`])(
  "preserves %s through cron preprocessing and POSIX shell argv parsing",
  async (workspace) => {
    expect(await CrontabManager.sync(workspace, ["0 * * * *"])).toBe(true);
    const line = installed.split("\n").find((entry) => entry.startsWith("0 *"))!;
    const parsed = cronCommand(line.slice(line.indexOf("/usr/local/bin")));
    expect(parsed.stdin).toBeNull();
    expect(parsed.command).toContain(' -s "cron-$(date +%s)"');
    const argument = parsed.command.slice(parsed.command.indexOf(" -w ") + 4, parsed.command.indexOf(" -s "));
    // Only probe the generated argument; never execute the scheduled command.
    expect(execFileSync("/bin/sh", ["-c", `printf '%s' ${argument}`], { encoding: "utf8" })).toBe(workspace);
    expect(await CrontabManager.listInstalled(workspace)).toEqual(["0 * * * *"]);
    expect(await CrontabManager.sync(workspace, ["5 4 * * *"])).toBe(true);
    expect(await CrontabManager.listInstalled(workspace)).toEqual(["5 4 * * *"]);
    expect(await CrontabManager.remove(workspace)).toBe(true);
    expect(installed).toBe("# unrelated entry\n");
  },
);

it("replaces a legacy percent entry without removing a distinct workspace", async () => {
  installed += "0 * * * * ws-ckpt checkpoint -w '/work/50% project' -s old\n";
  installed += "0 * * * * ws-ckpt checkpoint -w '/work/50% project-extra' -s other\n";
  expect(await CrontabManager.sync("/work/50% project", ["1 * * * *"])).toBe(true);
  expect(installed).not.toContain("-s old");
  expect(installed).toContain("-s other");
  expect(await CrontabManager.listInstalled("/work/50% project")).toEqual(["1 * * * *"]);
});
