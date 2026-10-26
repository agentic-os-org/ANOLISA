/** Resolve the SDK from the worker without importing application code. */
import { createRequire } from "node:module";
import { readFileSync, realpathSync } from "node:fs";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
const require = createRequire(pathToFileURL(process.argv.at(-1)));
const entry = require.resolve("@qoder-ai/qoder-agent-sdk");
const metadata = JSON.parse(readFileSync(join(dirname(entry), "../package.json"), "utf8"));
if (metadata.name !== "@qoder-ai/qoder-agent-sdk" || metadata.version !== "1.0.54" ||
    metadata.qoderCliVersion !== "1.1.74" || metadata.qoderSdkBrand !== "global") {
  throw new Error("AW requires Qoder SDK 1.0.54 with bundled Worker 1.1.74 (global)");
}
if (Number(process.versions.node.split(".")[0]) < 22) throw new Error("AW Qoder SDK requires Node 22+");
process.stdout.write(realpathSync(entry) + "\n");
