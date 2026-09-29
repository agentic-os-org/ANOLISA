// Drive real compiled plugin handlers; the PII subprocess is never mocked.
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const request = JSON.parse(readFileSync(0, "utf8"));
const directory = process.env.PII_TEST_OPENCLAW_DIST;
const { piiScan } = await import(pathToFileURL(`${directory}/capabilities/pii-scan.js`));
const { observability } = await import(pathToFileURL(`${directory}/capabilities/observability.js`));
const hooks = new Map();
const logs = [];
const api = {
  pluginConfig: {},
  on: (name, callback) => hooks.set(name, callback),
  logger: {
    info: (message) => logs.push(message),
    warn: (message) => logs.push(message),
    debug: (message) => logs.push(message),
  },
};
const capability = request.hook === "llm_input" ? observability : piiScan;
capability.register(api);
const result = await hooks.get(request.hook)(request.event, request.context);
// Pending record subprocesses keep Node alive until observability has finished.
console.log(JSON.stringify({ result: result ?? null, logs }));
