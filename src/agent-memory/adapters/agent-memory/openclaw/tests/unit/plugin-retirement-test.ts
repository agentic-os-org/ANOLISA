/** Exercise plugin retirement through registered hooks and real stdio workers. */
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import plugin from "../../src/index.js";
import { buildRecallQueries } from "../../src/keyword-extract.js";

type Handler = (...args: any[]) => Promise<any>;
type WorkerEvent = { event: string; pid: number; session: string };

async function waitFor(predicate: () => Promise<boolean>, label: string) {
  const deadline = Date.now() + 5_000;
  while (!(await predicate())) {
    assert.ok(Date.now() < deadline, `timed out waiting for ${label}`);
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

function isAlive(pid: number): boolean {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ESRCH") return false;
    throw error;
  }
}

for (const retirement of ["reload", "disposal", "gateway_stop"] as const) {
  for (const inFlight of [false, true]) {
    test(`${retirement} cannot restart an ${inFlight ? "in-flight" : "idle"} retired client`, {
      timeout: 15_000,
    }, async (context) => {
      const directory = await mkdtemp(join(tmpdir(), "memory-retirement-"));
      const binaryPath = join(directory, "worker.cjs");
      const eventsPath = join(directory, "events.jsonl");
      const events = async (): Promise<WorkerEvent[]> => {
        let text: string;
        try {
          text = await readFile(eventsPath, "utf8");
        } catch (error) {
          if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
          throw error;
        }
        return text.split("\n").filter(Boolean).map((line) => JSON.parse(line));
      };
      context.after(async () => {
        const pids = (await events()).filter((event) => event.event === "spawn")
          .map((event) => event.pid);
        for (const pid of pids) {
          if (isAlive(pid)) process.kill(pid, "SIGKILL");
        }
        await waitFor(async () => pids.every((pid) => !isAlive(pid)), "fixture workers to exit");
        await rm(directory, { recursive: true, force: true });
      });
      await writeFile(binaryPath, String.raw`#!${process.execPath}
const fs = require("node:fs");
const readline = require("node:readline");
const eventsPath = ${JSON.stringify(eventsPath)};
const session = process.env.MEMORY_SESSION_ID;
const previous = fs.existsSync(eventsPath) ? fs.readFileSync(eventsPath, "utf8") : "";
const firstOld = session === "ses_old" && !previous.includes('"event":"spawn"');
const log = (event) => fs.appendFileSync(eventsPath, JSON.stringify({ event, pid: process.pid, session }) + "\n");
const reply = (request, result) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) + "\n");
let pendingSearch;
log("spawn");
setInterval(() => {}, 1_000);
process.stdin.on("end", () => {});
process.on("SIGTERM", () => {
  if (pendingSearch) {
    // Complete the first query while shutdown is still waiting for exit.
    reply(pendingSearch, { content: [{ type: "text", text: "[]" }] });
    setTimeout(() => process.exit(0), 300);
  } else {
    process.exit(0);
  }
});
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (request.method === "initialize") {
    // Keep a wrongly restarted old worker initializing until its predecessor exits.
    if (session === "ses_old" && !firstOld) setTimeout(() => reply(request, {}), 650);
    else reply(request, {});
  } else if (${inFlight} && firstOld && request.params.name === "memory_search") {
    pendingSearch = request;
    log("search-pending");
  } else {
    reply(request, { content: [{ type: "text", text: "ready:" + session }] });
  }
});
`, { mode: 0o700 });

      function register(sessionId: string) {
        const hooks = new Map<string, Handler>();
        const tools = new Map<string, { execute: Handler }>();
        let dispose: (() => Promise<void>) | undefined;
        plugin.register({
          pluginConfig: { binaryPath, userId: "1000", sessionId, profile: "advanced" },
          resolvePath: (path: string) => path,
          logger: { info() {}, debug() {}, error() {}, warn() {} },
          on: (event: string, handler: Handler) => hooks.set(event, handler),
          registerTool: (tool: { name: string; execute: Handler }) => tools.set(tool.name, tool),
          registerMemoryCapability() {},
          registerMemoryCorpusSupplement() {},
          ...(retirement === "disposal" ? {
            lifecycle: { onDispose: (cleanup: () => Promise<void>) => {
              dispose = cleanup;
              return () => {};
            } },
          } : {}),
        } as never);
        return {
          recall: hooks.get("before_prompt_build")!,
          getContext: () => tools.get("memory_get_context")!.execute("test", {}),
          cleanup: () => dispose ? dispose() : hooks.get("gateway_stop")!(),
        };
      }

      const old = register("ses_old");
      let recalling: Promise<unknown> | undefined;
      if (inFlight) {
        const prompt = "cobalt helium quartz silver";
        assert.equal(buildRecallQueries(prompt).length, 2);
        recalling = old.recall({ prompt }, {});
        await waitFor(async () => (await events()).some((event) => event.event === "search-pending"),
          "first registered recall query");
      }
      let replacement: ReturnType<typeof register>;
      if (retirement === "reload") {
        replacement = register("ses_new");
      } else {
        await old.cleanup();
        replacement = register("ses_new");
      }
      await recalling;
      if (!inFlight) {
        const result = await old.getContext();
        assert.ok(result.details?.error, "retired registered callback must reject new work");
      }
      await old.cleanup();
      try {
        const result = await replacement.getContext();
        assert.equal(result.content[0].text, "ready:ses_new", "new instance remains usable");
      } finally {
        await replacement.cleanup();
      }
      const oldWorkers = (await events()).filter((event) =>
        event.event === "spawn" && event.session === "ses_old");
      assert.equal(oldWorkers.length, inFlight ? 1 : 0, "retirement must prevent lazy restart");
      assert.ok(oldWorkers.every((event) => !isAlive(event.pid)), "retired workers must be reaped");
    });
  }
}
