/** Re-registration on a host that owns instance disposal.
 *
 *  Covers the OpenClaw 2026.9.36+/2026.10.x generation, which exposes
 *  `lifecycle.onDispose` and retires the instance it replaces itself. The
 *  host may register the plugin again inside a single agent turn, each time
 *  with a different session id, and keep dispatching auto-recall and tool
 *  calls to the handlers of the *earlier* registration, so register() must
 *  not dispose the previous client: doing so retires a client the host is
 *  still calling, and every memory operation on the live instance fails with
 *  "agent-memory client is disposed".
 *
 *  OpenClaw 2026.9.2 does not expose that hook at all; its read-only loads are
 *  covered by `plugin-registration-mode-test.ts`.
 *
 *  The workers below are real stdio processes, so the assertions cover the
 *  lazy spawn, the JSON-RPC round trip, and the retirement on the host's
 *  own disposal rather than a mocked client.
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

test("a re-register on a disposal-capable host leaves the earlier instance usable", {
  timeout: 20_000,
}, async (context) => {
  const directory = await mkdtemp(join(tmpdir(), "memory-reregister-"));
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
  const disposers = new Map<string, () => Promise<void>>();

  function register(sessionId: string) {
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
      on: (event: string, handler: Handler) => hooks.set(event, handler),
      registerTool: (tool: { name: string; execute: Handler }) => tools.set(tool.name, tool),
      registerMemoryCapability() {},
      registerMemoryCorpusSupplement() {},
      // Instance-scoped disposal, as 2026.9.36+/2026.10.x exposes it.
      lifecycle: {
        onDispose: (cleanup: () => Promise<void>) => {
          disposers.set(sessionId, cleanup);
          return () => {};
        },
      },
    } as never);
    return {
      recall: hooks.get("before_prompt_build")!,
      observe: (content: string) => tools.get("memory_observe")!.execute("test", { content }),
      getContext: () => tools.get("memory_get_context")!.execute("test", {}),
      cleanup: () => disposers.get(sessionId)!(),
    };
  }

  const first = register("ses_first");
  // The host registers again mid-turn without disposing the instance whose
  // hooks and tools it keeps calling.
  const second = register("ses_second");

  const recalled = await first.recall({ prompt: "cobalt helium quartz silver" }, {});
  assert.ok(
    recalled?.prependContext?.includes("notes/cobalt.md"),
    `earlier instance must still recall (warnings: ${warnings.join(" | ")})`,
  );

  const observed = await first.observe("An E2E observation carrying token OBS-1.");
  assert.equal(
    observed.details?.error,
    undefined,
    `earlier instance must still observe (warnings: ${warnings.join(" | ")})`,
  );
  assert.equal(observed.details?.action, "observed");

  const contexts = await second.getContext();
  assert.equal(contexts.content[0].text, "ready:ses_second", "new instance remains usable");

  assert.ok(
    !warnings.some((warning) => /previous client still active/.test(warning)),
    `register() must not retire the previous client: ${warnings.join(" | ")}`,
  );

  // Only the host's own disposal retires an instance; the intent behind
  // #9245 (retired instances must not restart their worker) still holds.
  await first.cleanup();
  const retired = await first.getContext();
  assert.equal(retired.details?.error, "agent-memory client is disposed");
  await second.cleanup();

  const workers = (await events()).filter((event) => event.event === "spawn");
  assert.equal(workers.length, 2, "one worker per dispatched instance, and no more");
  assert.ok(workers.every((event) => !isAlive(event.pid)), "both workers must be reaped");
});
