/** AW-owned foreground SDK launcher; application imports happen only after binding. */
import { createRequire } from "node:module";
import { readFileSync, existsSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const VERSION = "0.84.2";

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
    if (metadata.name !== fullName || metadata.version !== VERSION) {
      throw new Error(`AW requires ${fullName} ${VERSION}`);
    }
    const entry = metadata.exports?.["."]?.import ?? metadata.main;
    if (entry !== "./dist/index.js")
      throw new Error(`Unsupported ${fullName} module layout`);
    resolved[name] = join(directory, entry);
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

if (process.env.AW_PI_PROBE === "1") {
  packages(process.argv.at(-1));
  process.stdout.write(`AW Pi SDK ${VERSION}\n`);
} else {
  const [application, ...args] = process.argv.slice(2);
  const resolved = packages(application);
  const sdk = await import(pathToFileURL(resolved["pi-coding-agent"]).href);
  const core = await import(pathToFileURL(resolved["pi-agent-core"]).href);
  const app = await import(pathToFileURL(application).href);
  if (
    typeof app.createSession !== "function" ||
    typeof app.run !== "function"
  ) {
    throw new Error(
      "Pi application must export createSession({sdk,args}) and run(session,{args})",
    );
  }
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
