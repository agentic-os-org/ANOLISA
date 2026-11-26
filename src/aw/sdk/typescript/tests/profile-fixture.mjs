import {
  appendFileSync,
  chmodSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { createServer } from "node:net";
import { once } from "node:events";
import { join } from "node:path";
import {
  isMainThread,
  parentPort,
  Worker,
  workerData,
} from "node:worker_threads";
import { ProfileSource } from "../src/profile-source.mjs";
import { SERVICE, SDK } from "../src/transport.mjs";

export const hook = (effects = [], failure_actions = []) => ({
  mode:
    effects.length || failure_actions.some((action) => action !== "report")
      ? "control"
      : "observe",
  effects: ["observe", ...effects],
  failure_actions,
});
export const field = (from, codec = "text") => ({ from, codec });

export function target(
  qualname,
  {
    id = qualname,
    module = "profile-fixture",
    call = "sync",
    hooks = { "session.start": hook() },
    fields = {},
    ...options
  } = {},
) {
  return { id, module, qualname, call, hooks, fields, ...options };
}

export function profile(targets, options = {}) {
  return {
    version: "aw-instrumentation-profile/v1alpha1",
    id: "profile-fixture",
    origin: "builtin",
    runtime: "js",
    package: { name: "profile-fixture", versions: ">=1.0,<2" },
    targets,
    ...options,
  };
}

export function appFixture(t, files, exports = "./main.cjs") {
  const root = mkdtempSync("/tmp/aw-prof-");
  const packageRoot = join(root, "node_modules", "profile-fixture");
  mkdirSync(packageRoot, { recursive: true });
  writeFileSync(
    join(packageRoot, "package.json"),
    JSON.stringify({
      name: "profile-fixture",
      version: "1.5.2",
      exports,
    }),
  );
  for (const [name, source] of Object.entries(files))
    writeFileSync(join(packageRoot, name), source);
  t.after(() => rmSync(root, { recursive: true, force: true }));
  return { root, packageRoot, entry: join(root, "app.mjs") };
}

export async function installProfile(t, document, options) {
  const source = await ProfileSource.probe(document, options);
  t.after(() => source.teardown());
  source.install();
  await source.verify();
  return source;
}

export async function policyFixture(t, root, hooks) {
  const endpoint = join(root, "sock");
  const log = join(root, "facts.jsonl");
  writeFileSync(log, "");
  const worker = new Worker(new URL(import.meta.url), {
    workerData: { endpoint, log, hooks },
  });
  t.after(() => worker.terminate().then(() => {}));
  await once(worker, "message");
  return {
    endpoint,
    events() {
      return readFileSync(log, "utf8")
        .trim()
        .split("\n")
        .filter(Boolean)
        .map(JSON.parse);
    },
  };
}

if (!isMainThread) {
  const { endpoint, log, hooks } = workerData;
  const routes = { events: {}, hooks };
  const handle = { binding: { instance_id: "parent" } };
  function dispatch({ method, command, native_input: payload, close }) {
    const clean = { code: 0, stdout: [], stderr: [] };
    switch (method) {
      case "status":
        return { sdk_protocol: SDK };
      case "registration":
        if (command.action === "register_sdk")
          return { state: "pending_ready", handle, routes };
        return { state: command.action === "ready" ? "ready" : "released" };
      case "open_scope":
        return { instance_id: "task" };
      case "scope_policy":
        return routes;
      case "acknowledge_scope":
      case "release_instance":
        return {};
      case "sdk_callback":
        if (close) return clean;
        appendFileSync(log, JSON.stringify(payload) + "\n");
        if (payload.text === "deny" || payload.input?.command === "blocked")
          return { ...clean, code: 2 };
        if (
          payload.event === "tool.before" &&
          payload.input?.command?.startsWith("raw:")
        )
          return {
            ...clean,
            directive: {
              type: "replace_input",
              input: {
                ...payload.input,
                command: payload.input.command.replace("raw:", "safe:"),
              },
            },
          };
        if (payload.event === "tool.after") {
          const result = payload.result;
          if (result === "withhold")
            return { ...clean, directive: { type: "withhold_result" } };
          if (
            result === "replace" ||
            (Array.isArray(result) && result.includes("replace"))
          )
            return {
              ...clean,
              directive: {
                type: "replace_result",
                result: Array.isArray(result) ? ["clean"] : "clean",
              },
            };
        }
        return clean;
      default:
        throw new Error(method);
    }
  }

  const identity = { generation: "profile-test", config_revision: "1" };
  const server = createServer((socket) => {
    let frame = Buffer.alloc(0);
    socket.on("data", (chunk) => {
      frame = Buffer.concat([frame, chunk]);
      if (frame.length < 4 || frame.length < frame.readUInt32BE() + 4) return;
      const operation = JSON.parse(frame.subarray(4)).operation;
      const reply = Buffer.from(
        JSON.stringify({
          api_version: SERVICE,
          identity,
          error: null,
          result: dispatch(operation),
        }),
      );
      const length = Buffer.alloc(4);
      length.writeUInt32BE(reply.length);
      socket.end(Buffer.concat([length, reply]));
    });
  });
  server.listen(endpoint, () => {
    chmodSync(endpoint, 0o600);
    parentPort.postMessage("ready");
  });
}
