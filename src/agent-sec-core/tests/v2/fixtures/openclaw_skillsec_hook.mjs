// Drive compiled callbacks with real CLI subprocesses; this is not a native host.
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const request = JSON.parse(readFileSync(0, "utf8"));
const directory = process.env.SKILLSEC_TEST_OPENCLAW_DIST;
const { skillLedger } = await import(pathToFileURL(`${directory}/capabilities/skill-ledger.js`));
const hooks = new Map();
const log = (message) => console.error(message);
skillLedger.register({
  pluginConfig: {},
  on: (name, callback) => hooks.set(name, callback),
  logger: { info: log, warn: log, debug: log },
});
const handler = hooks.get("before_tool_call");
const result = handler ? await handler(request.event, request.context) : undefined;
console.log(JSON.stringify(result ?? null));
