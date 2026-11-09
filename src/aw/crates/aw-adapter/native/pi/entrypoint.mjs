/** AW-owned foreground SDK launcher; application imports happen only after binding. */
import { createRequire } from "node:module";
import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

function packages(application) {
  let require = createRequire(pathToFileURL(application));
  const resolved = {};
  for (const name of ["pi-coding-agent", "pi-agent-core"]) {
    const fullName = `@earendil-works/${name}`;
    const directory = require.resolve
      .paths(fullName)
      .map((base) => join(base, fullName))
      .find((base) => existsSync(join(base, "package.json")));
    if (!directory)
      throw new Error(`Cannot resolve installed ${fullName} from application`);
    const metadata = JSON.parse(
      readFileSync(join(directory, "package.json"), "utf8"),
    );
    if (metadata.name !== fullName || typeof metadata.version !== "string" ||
        !/^[0-9][0-9A-Za-z.+_!-]{0,127}$/.test(metadata.version)) {
      throw new Error(`AW requires ${fullName} with valid version metadata`);
    }
    const entry = metadata.exports?.["."]?.import ?? metadata.main;
    if (entry !== "./dist/index.js")
      throw new Error(`Unsupported ${fullName} module layout`);
    resolved[name] = join(directory, entry);
    if (name === "pi-coding-agent") resolved.version = metadata.version;
    else resolved.coreVersion = metadata.version;
    if (name === "pi-coding-agent")
      require = createRequire(pathToFileURL(resolved[name]));
  }
  if (
    Number(process.versions.node.split(".")[0]) < 22 ||
    (process.versions.node.startsWith("22.") &&
      Number(process.versions.node.split(".")[1]) < 19)
  ) {
    throw new Error("AW Pi requires Node.js >=22.19.0");
  }
  return resolved;
}

/** Run an already prepared native host in its existing process. */
export async function runNativeWorker({ application, args = [] }) {
  const resolved = packages(application);
  const { configuration } = await import(new URL("./bridge.mjs", import.meta.url));
  const startup = configuration();
  if (!startup.resident_policy_reload)
    throw new Error("Native Pi startup requires aw instance prepare");
  if (startup.hooks.some(hook => hook.on_error === "withhold_result") &&
      (resolved.version !== "0.84.2" || resolved.coreVersion !== "0.84.2"))
    throw new Error("AW strict Pi output requires pi-coding-agent and pi-agent-core 0.84.2");
  const sdk = await import(pathToFileURL(resolved["pi-coding-agent"]).href);
  const core = await import(pathToFileURL(resolved["pi-agent-core"]).href);
  if (typeof sdk.AgentSession !== "function" || typeof core.Agent !== "function")
    throw new Error("AW Pi SDK requires native AgentSession and Agent");
  const app = await import(pathToFileURL(application).href);
  if (typeof app.createSession !== "function" ||
      (typeof app.runWorker !== "function" && typeof app.run !== "function"))
    throw new Error("Native Pi startup requires createSession and runWorker or run factories");
  if (typeof app.runWorker === "function" && typeof app.run === "function")
    throw new Error("Choose runWorker or run, not both");
  const worker = typeof app.runWorker === "function" ? app : {
    ...app,
    async runWorker({ createSession, closeSession }) {
      const { session } = await createSession();
      await app.run(session, { args, createSession, closeSession });
    },
  };
  const { runWorker } = await import(new URL("./resident.mjs", import.meta.url));
  await runWorker(worker, sdk, core, args, { sdk: resolved.version, core: resolved.coreVersion });
}

if (process.env.AW_PI_PROBE === "1") {
  const resolved = packages(process.argv.at(-1));
  process.stdout.write(`AW Pi SDK ${resolved.version}:${resolved.coreVersion}\n`);
} else if (process.argv[1] && pathToFileURL(process.argv[1]).href === import.meta.url) {
  const [application, ...args] = process.argv.slice(2);
  const resolved = packages(application);
  const { configuration } = await import(new URL("./bridge.mjs", import.meta.url));
  if (configuration().hooks.some(hook => hook.on_error === "withhold_result") &&
      (resolved.version !== "0.84.2" || resolved.coreVersion !== "0.84.2"))
    throw new Error("AW strict Pi output requires pi-coding-agent and pi-agent-core 0.84.2");
  const sdk = await import(pathToFileURL(resolved["pi-coding-agent"]).href);
  const core = await import(pathToFileURL(resolved["pi-agent-core"]).href);
  for (const [name, value] of [["AgentSession", sdk.AgentSession], ["Agent", core.Agent]])
    if (typeof value !== "function") throw new Error(`AW Pi SDK requires native ${name}`);
  const app = await import(pathToFileURL(application).href);
  if (
    typeof app.createSession !== "function" ||
    (typeof app.run !== "function" && typeof app.runWorker !== "function")
  ) {
    throw new Error(
      "Pi application must export createSession({sdk,args}) and run(session,{args})",
    );
  }
  if (typeof app.runWorker === "function") {
    if (typeof app.run === "function") throw new Error("Choose runWorker or run, not both");
    const { runWorker } = await import(new URL("./resident.mjs", import.meta.url));
    await runWorker(app, sdk, core, args, { sdk: resolved.version, core: resolved.coreVersion });
  } else {
  if (configuration().hooks.some(hook => hook.event === "input.submit"))
    throw new Error("AW Pi input.submit requires the runWorker factory");
  const installations = new Map();
  // All new/resume/fork/import replacements must use this common application factory.
  const createSession = async (options = {}) => {
    const result = await app.createSession({ ...options, sdk, args });
    const session = result?.session;
    if (
      !(session instanceof sdk.AgentSession) ||
      !(session.agent instanceof core.Agent)
    ) {
      throw new Error(
        "Pi factory must return the verified SDK createAgentSession result",
      );
    }
    if (installations.has(session))
      throw new Error("Pi factory returned an already managed session");
    const bridge = await import(new URL("./bridge.mjs", import.meta.url));
    const installed = bridge.installSessionBridge(session);
    installations.set(session, installed);
    return result;
  };
  try {
    const { session } = await createSession();
    installations.get(session).ready();
    await app.run(session, { args, createSession });
  } finally {
    for (const [session, installed] of installations) {
      installed.dispose();
      session.dispose();
    }
  }
}

}
