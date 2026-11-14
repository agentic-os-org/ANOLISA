/** Re-registration on a host that loads the plugin in read-only modes.
 *
 *  OpenClaw 2026.9.2 — the host #9306 reproduces on — performs a
 *  `tool-discovery` load and a `discovery` load per agent turn, each calling
 *  register() with its own session id, while the handlers of the earlier
 *  `full` registration stay live. That host also has no
 *  `lifecycle.onDispose`, so a guard that looks only for instance disposal
 *  does not fire there and the read-only load still tears down the client the
 *  gateway keeps dispatching to.
 *
 *  The mocks below are therefore the shape that host actually passes:
 *  `registrationMode` set, and a lifecycle facade that carries only
 *  `registerRuntimeLifecycle`. The workers are real stdio processes, so the
 *  assertions cover the lazy spawn and the JSON-RPC round trip.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import plugin from "../../src/index.js";

type Handler = (...args: any[]) => Promise<any>;
type WorkerEvent = { event: string; pid: number; session: string };

/** One `memory_search` hit, in the shape the recall hook consumes. */
const SEARCH_HITS = [{ path: "notes/cobalt.md", snippet: "cobalt quartz", score: 1 }];

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

test("a read-only load leaves the live full registration usable", {
  timeout: 20_000,
}, async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "memory-registration-mode-"));
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
const reply = (request, result) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: request.id, result }) + "\n");
fs.appendFileSync(eventsPath, JSON.stringify({ event: "spawn", pid: process.pid, session }) + "\n");
setInterval(() => {}, 1_000);
process.stdin.on("end", () => {});
process.on("SIGTERM", () => process.exit(0));
readline.createInterface({ input: process.stdin }).on("line", (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (request.method === "initialize") {
    reply(request, {});
  } else {
    const text = request.params.name === "memory_search"
      ? JSON.stringify(${JSON.stringify(SEARCH_HITS)})
      : "ready:" + session;
    reply(request, { content: [{ type: "text", text }] });
  }
});
`, { mode: 0o700 });

  const warnings: string[] = [];
  const gatewayStops: Handler[] = [];

  /**
   * Register once, in the mode the host uses for that load.
   *
   * `lifecycle` carries only `registerRuntimeLifecycle`, which is the whole of
   * the facade OpenClaw 2026.9.2 exposes — no `onDispose` to be found.
   */
  function register(registrationMode: string, sessionId: string) {
    const hooks = new Map<string, Handler>();
    const tools = new Map<string, { execute: Handler }>();
    plugin.register({
      pluginConfig: { binaryPath, userId: "1000", sessionId, profile: "advanced" },
      resolvePath: (path: string) => path,
      logger: {
        info() {},
        debug() {},
        error() {},
        warn: (message: string) => {
          warnings.push(message);
        },
      },
      on: (event: string, handler: Handler) => {
        hooks.set(event, handler);
        if (event === "gateway_stop") gatewayStops.push(handler);
      },
      registerTool: (tool: { name: string; execute: Handler }) => tools.set(tool.name, tool),
      registerMemoryCapability() {},
      registerMemoryCorpusSupplement() {},
      registrationMode,
      lifecycle: { registerRuntimeLifecycle: () => {} },
    } as never);
    return {
      recall: hooks.get("before_prompt_build")!,
      observe: (content: string) => tools.get("memory_observe")!.execute("test", { content }),
      getContext: () => tools.get("memory_get_context")!.execute("test", {}),
      gatewayStop: () => hooks.get("gateway_stop")!(),
    };
  }

  const full = register("full", "ses_full");
  // The per-turn loads of 2026.9.2, each with its own session id.
  const toolDiscovery = register("tool-discovery", "ses_tool_discovery");
  const discovery = register("discovery", "ses_discovery");

  const recalled = await full.recall({ prompt: "cobalt helium quartz silver" }, {});
  assert.ok(
    recalled?.prependContext?.includes("notes/cobalt.md"),
    `the full registration must still recall (warnings: ${warnings.join(" | ")})`,
  );

  const observed = await full.observe("An E2E observation carrying token OBS-2.");
  assert.equal(
    observed.details?.error,
    undefined,
    `the full registration must still observe (warnings: ${warnings.join(" | ")})`,
  );
  assert.equal(observed.details?.action, "observed");

  const contexts = await full.getContext();
  assert.equal(contexts.content[0].text, "ready:ses_full", "the full client keeps its session");

  assert.ok(
    !warnings.some((warning) => /previous client still active/.test(warning)),
    `a read-only load must not retire the live client: ${warnings.join(" | ")}`,
  );

  // A later full registration is still a replacement: it reaps the client of
  // the full registration it supersedes, and only that one. The read-only
  // clients are left alone, because the host may still dispatch to them.
  const before = warnings.length;
  const replacement = register("full", "ses_full_2");
  const replaced = warnings.slice(before).filter((warning) =>
    /previous client still active/.test(warning),
  );
  assert.equal(replaced.length, 1, `one reap, not one per registration: ${warnings.join(" | ")}`);
  const reaped = await full.getContext();
  assert.equal(reaped.details?.error, "agent-memory client is disposed");
  const contexts2 = await replacement.getContext();
  assert.equal(contexts2.content[0].text, "ready:ses_full_2");

  // Gateway shutdown is the only cleanup this host generation offers.
  for (const stop of gatewayStops) await stop();
  const workers = (await events()).filter((event) => event.event === "spawn");
  await waitFor(async () => workers.every((event) => !isAlive(event.pid)), "workers to be reaped");
  // The two full registrations each started a worker; the read-only loads never
  // did, which is the point of not tracking them.
  assert.equal(workers.length, 2, "only the dispatched full registrations spawn");
});
